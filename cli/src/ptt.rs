// Push-to-talk pipeline.
//
// Capture: cpal default input → downmix mono → 48 kHz s16 → audiopus → Opus.
// Frames are 20 ms (960 samples). No external binary required.
//
// Encoded packets feed straight into the peer's `TrackLocalStaticSample`
// audio track (Opus mime-type). Receiver-side decoding + playback live in
// `peers.rs`.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webrtc::media::Sample;

use crate::peers::Peer;

const SAMPLE_RATE: u32 = 48_000;
const FRAME_MS: usize = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize) * FRAME_MS / 1000; // 960

/// Capture mic via cpal, encode Opus, push to the peer's audio track until
/// the cpal stream errors or the encoder thread joins.
///
/// Must be called from inside a tokio runtime — uses `Handle::current` to
/// drive the async `write_sample` calls from the encoding thread.
/// Enumerate input device names. Used by `sociacli mic list`.
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    for d in host.input_devices().context("enumerate input devices")? {
        if let Ok(n) = d.name() {
            names.push(n);
        }
    }
    Ok(names)
}

/// Pick the cpal input device whose name matches `preferred` (substring,
/// case-insensitive). Falls back to the host default when `preferred` is
/// `None` or no device matches.
fn pick_device(preferred: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(want) = preferred {
        let want = want.to_lowercase();
        if let Ok(devs) = host.input_devices() {
            for d in devs {
                let n = d.name().unwrap_or_default();
                if n.to_lowercase().contains(&want) {
                    tracing::info!(device = %n, "ptt: using preferred input device");
                    return Ok(d);
                }
            }
        }
        tracing::warn!(want = %want, "ptt: preferred input device not found, falling back to default");
    }
    host.default_input_device()
        .context("no default audio input device")
}

/// Convenience for callers that just want to stream until end-of-program,
/// using the OS default input device.
pub fn transmit_blocking(peer: Arc<Peer>) -> Result<()> {
    transmit_blocking_until(peer, Arc::new(AtomicBool::new(false)), None)
}

/// Same as `transmit_blocking`, but stops cleanly the moment `stop` flips to
/// `true`. Used by the hold-to-talk hotkey path. `device` is an optional
/// name fragment (case-insensitive match against cpal's input device names).
pub fn transmit_blocking_until(
    peer: Arc<Peer>,
    stop: Arc<AtomicBool>,
    device: Option<String>,
) -> Result<()> {
    let device = pick_device(device.as_deref())?;

    let supported = pick_config(&device)?;
    let sample_format = supported.sample_format();
    let cfg_pre: StreamConfig = supported.clone().into();
    let channels = cfg_pre.channels as usize;
    let cfg = StreamConfig {
        channels: cfg_pre.channels,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    let (pcm_tx, pcm_rx) = bounded::<Vec<i16>>(64);
    let err_fn = |e| tracing::error!(?e, "audio input stream error");

    // cpal Stream is !Send on some platforms; keep it on this thread for its lifetime.
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &cfg,
            mk_callback_f32(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &cfg,
            mk_callback_i16(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &cfg,
            mk_callback_u16(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::I8 => device.build_input_stream(
            &cfg,
            mk_callback_i8(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::U8 => device.build_input_stream(
            &cfg,
            mk_callback_u8(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::I32 => device.build_input_stream(
            &cfg,
            mk_callback_i32(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        SampleFormat::F64 => device.build_input_stream(
            &cfg,
            mk_callback_f64(pcm_tx.clone(), channels),
            err_fn,
            None,
        )?,
        other => anyhow::bail!("unsupported sample format: {other:?}"),
    };
    stream.play().context("start cpal input stream")?;
    tracing::info!(
        "PTT: capturing from `{}` @ {} Hz, {} ch",
        device.name().unwrap_or_else(|_| "?".into()),
        SAMPLE_RATE,
        channels
    );

    // Encoding loop owns the peer + audiopus encoder.
    let encode_result = encode_loop(peer, pcm_rx, stop);
    drop(stream);
    encode_result
}

fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    // Want 48 kHz; channels we'll downmix in software.
    let configs = device
        .supported_input_configs()
        .context("query input configs")?
        .collect::<Vec<_>>();

    // First pass: any range that covers 48 kHz exactly.
    if let Some(found) = configs
        .iter()
        .find(|r| r.min_sample_rate().0 <= SAMPLE_RATE && r.max_sample_rate().0 >= SAMPLE_RATE)
    {
        return Ok(found.clone().with_sample_rate(cpal::SampleRate(SAMPLE_RATE)));
    }
    // Fallback: the device's own default.
    device
        .default_input_config()
        .context("no usable input config")
}

fn mk_callback_f32(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[f32], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[f32], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc = 0.0f32;
            for &s in chunk {
                acc += s;
            }
            let v = (acc / channels as f32).clamp(-1.0, 1.0);
            buf.push((v * 32_767.0) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_i16(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[i16], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[i16], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc: i32 = 0;
            for &s in chunk {
                acc += s as i32;
            }
            buf.push((acc / channels as i32) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_u16(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[u16], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[u16], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc: i32 = 0;
            for &s in chunk {
                acc += s as i32 - 32_768;
            }
            buf.push((acc / channels as i32) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_i8(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[i8], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[i8], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc: i32 = 0;
            for &s in chunk {
                acc += s as i32;
            }
            // i8 → i16: scale into the full i16 range (×256).
            buf.push(((acc / channels as i32) * 256) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_u8(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[u8], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[u8], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc: i32 = 0;
            for &s in chunk {
                acc += s as i32 - 128;
            }
            // u8 (centered at 128) → i16: scale ×256.
            buf.push(((acc / channels as i32) * 256) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_i32(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[i32], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[i32], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc: i64 = 0;
            for &s in chunk {
                acc += s as i64;
            }
            // i32 → i16: shift down 16 bits after averaging.
            buf.push(((acc / channels as i64) >> 16) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}
fn mk_callback_f64(
    tx: Sender<Vec<i16>>,
    channels: usize,
) -> impl FnMut(&[f64], &cpal::InputCallbackInfo) + Send + 'static {
    let mut buf = Vec::with_capacity(FRAME_SAMPLES);
    move |data: &[f64], _| {
        for chunk in data.chunks_exact(channels) {
            let mut acc = 0.0f64;
            for &s in chunk {
                acc += s;
            }
            let v = (acc / channels as f64).clamp(-1.0, 1.0);
            buf.push((v * 32_767.0) as i16);
            if buf.len() == FRAME_SAMPLES {
                let _ = tx.try_send(std::mem::replace(&mut buf, Vec::with_capacity(FRAME_SAMPLES)));
            }
        }
    }
}

fn encode_loop(
    peer: Arc<Peer>,
    pcm_rx: Receiver<Vec<i16>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut enc = audiopus::coder::Encoder::new(
        audiopus::SampleRate::Hz48000,
        audiopus::Channels::Mono,
        audiopus::Application::Voip,
    )
    .map_err(|e| anyhow!("opus encoder init: {e:?}"))?;
    enc.set_bitrate(audiopus::Bitrate::BitsPerSecond(24_000)).ok();

    let mut out = vec![0u8; 4_000];
    let handle = tokio::runtime::Handle::current();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let samples = match pcm_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(s) => s,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let n = match enc.encode(&samples, &mut out) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(?e, "opus encode failed");
                continue;
            }
        };
        let sample = Sample {
            data: bytes::Bytes::copy_from_slice(&out[..n]),
            duration: Duration::from_millis(FRAME_MS as u64),
            ..Default::default()
        };
        let peer = peer.clone();
        let res = handle.block_on(async move { peer.audio.write_sample(&sample).await });
        if let Err(e) = res {
            tracing::warn!(?e, "write_sample failed; ending PTT");
            break;
        }
    }
    Ok(())
}

/// Best-effort local SFX. Plays a wav from disk if present; otherwise silent.
pub fn play_blip(path: Option<&std::path::Path>) {
    let Some(path) = path else { return };
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let Ok(file) = std::fs::File::open(&path) else { return };
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            return;
        };
        let Ok(sink) = rodio::Sink::try_new(&handle) else { return };
        let Ok(decoder) = rodio::Decoder::new(std::io::BufReader::new(file)) else {
            return;
        };
        sink.append(decoder);
        sink.sleep_until_end();
    });
}
