// WebRTC peer manager.
//
// One `RTCPeerConnection` per friend. Each connection carries:
//   * a "ctrl" data channel for JSON action frames (msg, invite, ptt control)
//   * an Opus audio track ("ptt-audio") used by the PTT pipeline
//
// SDP/ICE is exchanged through the signaling server as opaque JSON payloads
// inside `ClientMsg::Signal { to, payload }` / `ServerMsg::Signal { from, payload }`.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use webrtc::api::{APIBuilder, API};
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTPCodecType};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_remote::TrackRemote;

#[derive(Debug, Clone)]
pub struct IncomingAction {
    pub from: String,
    pub frame: serde_json::Value,
}

pub struct Peer {
    pub friend_id: String,
    pub pc: Arc<RTCPeerConnection>,
    pub ctrl: RwLock<Option<Arc<RTCDataChannel>>>,
    pub audio: Arc<TrackLocalStaticSample>,
}

/// Commands the local audio sink thread accepts.
#[derive(Clone)]
pub enum AudioCmd {
    /// Decoded PCM frame from a remote peer (48 kHz mono i16).
    Samples(Vec<i16>),
    /// A short generated sine beep (used for PTT start/end blips).
    Beep { freq: f32, ms: u64, gain: f32 },
}

#[derive(Debug, Clone, Copy)]
pub struct AudioFx {
    /// 300–3400 Hz band-pass on incoming PCM (telephone / radio colour).
    pub radio_effect: bool,
    /// Synthesize start/end beeps on PTT transitions.
    pub radio_beeps: bool,
}

impl Default for AudioFx {
    fn default() -> Self {
        Self {
            radio_effect: true,
            radio_beeps: true,
        }
    }
}

pub struct PeerManager {
    api: API,
    peers: Mutex<HashMap<String, Arc<Peer>>>,
    /// Raw JSON `ClientMsg` frames to forward over the signaling WS.
    signal_tx: mpsc::UnboundedSender<String>,
    /// Action frames arriving over data channels (from any peer).
    action_tx: mpsc::UnboundedSender<IncomingAction>,
    /// Commands for the local audio sink (samples + generated beeps).
    audio_tx: crossbeam_channel::Sender<AudioCmd>,
    fx: AudioFx,
    ice: Vec<RTCIceServer>,
}

impl PeerManager {
    pub fn new(
        signal_tx: mpsc::UnboundedSender<String>,
        action_tx: mpsc::UnboundedSender<IncomingAction>,
        fx: AudioFx,
        ice: Vec<RTCIceServer>,
    ) -> Arc<Self> {
        let mut media = MediaEngine::default();
        let _ = media.register_default_codecs();
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media)
            .unwrap_or_else(|_| Registry::new());
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .build();
        let audio_tx = spawn_audio_sink(fx);
        Arc::new(Self {
            api,
            peers: Mutex::new(HashMap::new()),
            signal_tx,
            action_tx,
            audio_tx,
            fx,
            ice: if ice.is_empty() {
                // Never run with zero ICE servers — host candidates alone fail
                // across any NAT. Fall back to public STUN.
                vec![RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".into()],
                    ..Default::default()
                }]
            } else {
                ice
            },
        })
    }

    /// Convert config ICE entries into webrtc `RTCIceServer`s.
    pub fn ice_from_config(servers: &[crate::config::IceServer]) -> Vec<RTCIceServer> {
        servers
            .iter()
            .map(|s| RTCIceServer {
                urls: s.urls.clone(),
                username: s.username.clone().unwrap_or_default(),
                credential: s.credential.clone().unwrap_or_default(),
                ..Default::default()
            })
            .collect()
    }

    pub fn audio_sink(&self) -> crossbeam_channel::Sender<AudioCmd> {
        self.audio_tx.clone()
    }

    pub fn fx(&self) -> AudioFx {
        self.fx
    }

    /// Play a short start blip on the local speaker (caller respects fx flag).
    pub fn beep_start(&self) {
        if self.fx.radio_beeps {
            let _ = self.audio_tx.send(AudioCmd::Beep {
                freq: 1_200.0,
                ms: 80,
                gain: 0.18,
            });
        }
    }
    /// Play a short end blip on the local speaker (caller respects fx flag).
    pub fn beep_end(&self) {
        if self.fx.radio_beeps {
            let _ = self.audio_tx.send(AudioCmd::Beep {
                freq: 440.0,
                ms: 100,
                gain: 0.18,
            });
        }
    }

    fn config(&self) -> RTCConfiguration {
        RTCConfiguration {
            ice_servers: self.ice.clone(),
            ..Default::default()
        }
    }

    fn send_signal(&self, friend_id: &str, payload: serde_json::Value) {
        let frame = serde_json::json!({
            "t": "signal",
            "to": friend_id,
            "payload": payload,
        });
        if let Ok(s) = serde_json::to_string(&frame) {
            let _ = self.signal_tx.send(s);
        } else {
            tracing::warn!("failed to serialize signal frame");
        }
    }

    async fn get_or_create(self: &Arc<Self>, friend_id: &str, initiator: bool) -> Result<Arc<Peer>> {
        {
            let peers = self.peers.lock().await;
            if let Some(p) = peers.get(friend_id) {
                return Ok(p.clone());
            }
        }
        let pc = Arc::new(self.api.new_peer_connection(self.config()).await?);

        // Audio track for PTT — always present; PTT pipeline writes samples on demand.
        let audio = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 1,
                ..Default::default()
            },
            "ptt-audio".to_string(),
            format!("ptt-{}", friend_id),
        ));
        let _ = pc
            .add_track(Arc::clone(&audio) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        let peer = Arc::new(Peer {
            friend_id: friend_id.to_string(),
            pc: pc.clone(),
            ctrl: RwLock::new(None),
            audio,
        });
        self.peers.lock().await.insert(friend_id.to_string(), peer.clone());

        // ICE candidates → signaling
        let manager = self.clone();
        let fid_ice = friend_id.to_string();
        pc.on_ice_candidate(Box::new(move |cand| {
            let manager = manager.clone();
            let fid = fid_ice.clone();
            Box::pin(async move {
                if let Some(c) = cand {
                    if let Ok(init) = c.to_json() {
                        let payload = serde_json::json!({
                            "type": "candidate",
                            "candidate": init,
                        });
                        manager.send_signal(&fid, payload);
                    }
                }
            })
        }));

        // Incoming audio track → decode Opus → push to local sink
        let audio_tx = self.audio_tx.clone();
        let beeps = self.fx.radio_beeps;
        pc.on_track(Box::new(move |track: Arc<TrackRemote>, _, _| {
            let audio_tx = audio_tx.clone();
            Box::pin(async move {
                if track.kind() != RTPCodecType::Audio {
                    return;
                }
                tokio::spawn(async move {
                    if let Err(e) = pump_remote_audio(track, audio_tx, beeps).await {
                        tracing::warn!(?e, "remote audio pump ended");
                    }
                });
            })
        }));

        // Incoming data channels (when we're the answerer)
        let peer_dc = peer.clone();
        let action_tx_dc = self.action_tx.clone();
        let fid_dc = friend_id.to_string();
        pc.on_data_channel(Box::new(move |ch: Arc<RTCDataChannel>| {
            let peer_dc = peer_dc.clone();
            let action_tx = action_tx_dc.clone();
            let fid = fid_dc.clone();
            Box::pin(async move {
                if ch.label() == "ctrl" {
                    {
                        let mut g = peer_dc.ctrl.write().await;
                        *g = Some(ch.clone());
                    }
                    let action_tx = action_tx.clone();
                    let fid = fid.clone();
                    ch.on_message(Box::new(move |msg: DataChannelMessage| {
                        let action_tx = action_tx.clone();
                        let fid = fid.clone();
                        Box::pin(async move {
                            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&msg.data) {
                                let _ = action_tx.send(IncomingAction { from: fid, frame: v });
                            }
                        })
                    }));
                }
            })
        }));

        if initiator {
            let ch = pc
                .create_data_channel("ctrl", Some(RTCDataChannelInit::default()))
                .await?;
            {
                let mut g = peer.ctrl.write().await;
                *g = Some(ch.clone());
            }
            let action_tx = self.action_tx.clone();
            let fid = friend_id.to_string();
            ch.on_message(Box::new(move |msg: DataChannelMessage| {
                let action_tx = action_tx.clone();
                let fid = fid.clone();
                Box::pin(async move {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&msg.data) {
                        let _ = action_tx.send(IncomingAction { from: fid, frame: v });
                    }
                })
            }));
        }

        Ok(peer)
    }

    pub async fn dial(self: &Arc<Self>, friend_id: &str) -> Result<()> {
        let peer = self.get_or_create(friend_id, true).await?;
        let offer = peer.pc.create_offer(None).await?;
        peer.pc.set_local_description(offer.clone()).await?;
        self.send_signal(
            friend_id,
            serde_json::json!({ "type": "offer", "sdp": offer.sdp }),
        );
        Ok(())
    }

    pub async fn handle_signal(
        self: &Arc<Self>,
        from: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "offer" => {
                let sdp = payload
                    .get("sdp")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("offer missing sdp"))?;
                let peer = self.get_or_create(from, false).await?;
                let desc = RTCSessionDescription::offer(sdp.to_string())?;
                peer.pc.set_remote_description(desc).await?;
                let answer = peer.pc.create_answer(None).await?;
                peer.pc.set_local_description(answer.clone()).await?;
                self.send_signal(
                    from,
                    serde_json::json!({ "type": "answer", "sdp": answer.sdp }),
                );
            }
            "answer" => {
                let sdp = payload
                    .get("sdp")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("answer missing sdp"))?;
                let peer = self.get_or_create(from, true).await?;
                let desc = RTCSessionDescription::answer(sdp.to_string())?;
                peer.pc.set_remote_description(desc).await?;
            }
            "candidate" => {
                let cand = payload
                    .get("candidate")
                    .ok_or_else(|| anyhow!("candidate missing payload"))?;
                let init: RTCIceCandidateInit = serde_json::from_value(cand.clone())?;
                let peer = self.get_or_create(from, false).await?;
                peer.pc.add_ice_candidate(init).await?;
            }
            other => tracing::warn!(kind = other, "unknown signal type"),
        }
        Ok(())
    }

    /// Attempt to deliver an action frame to a friend over the data channel.
    /// Returns true on success, false when no open channel is available — in
    /// that case the caller should fall back to server-relayed `send_action`.
    pub async fn try_send_action(&self, friend_id: &str, frame: &serde_json::Value) -> bool {
        let peers = self.peers.lock().await;
        let Some(peer) = peers.get(friend_id).cloned() else {
            return false;
        };
        drop(peers);
        let ctrl = peer.ctrl.read().await;
        let Some(ch) = ctrl.as_ref() else { return false };
        let Ok(s) = serde_json::to_string(frame) else {
            return false;
        };
        ch.send_text(s).await.is_ok()
    }

    pub async fn peer(&self, friend_id: &str) -> Option<Arc<Peer>> {
        self.peers.lock().await.get(friend_id).cloned()
    }
}

async fn pump_remote_audio(
    track: Arc<TrackRemote>,
    audio_tx: crossbeam_channel::Sender<AudioCmd>,
    beeps: bool,
) -> Result<()> {
    let mut dec = audiopus::coder::Decoder::new(
        audiopus::SampleRate::Hz48000,
        audiopus::Channels::Mono,
    )
    .map_err(|e| anyhow!("opus decoder: {e:?}"))?;
    let mut out = vec![0i16; 5760]; // up to 120 ms of 48 kHz mono
    let mut talking = false;
    loop {
        // Wait for the next RTP packet, but bail to a silence-timeout branch if
        // nothing arrives for ~400 ms while we're "talking" — that's our cue to
        // emit the end-of-transmission beep.
        let next = tokio::time::timeout(Duration::from_millis(400), track.read_rtp());
        let rtp = match next.await {
            Ok(Ok((p, _))) => p,
            Ok(Err(_)) => break, // track ended
            Err(_) => {
                if talking && beeps {
                    let _ = audio_tx.send(AudioCmd::Beep {
                        freq: 440.0,
                        ms: 100,
                        gain: 0.18,
                    });
                }
                talking = false;
                continue;
            }
        };
        if rtp.payload.is_empty() {
            continue;
        }
        let n = match dec.decode(Some(rtp.payload.as_ref()), &mut out[..], false) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(?e, "opus decode error");
                continue;
            }
        };
        if n == 0 {
            continue;
        }
        if !talking && beeps {
            let _ = audio_tx.send(AudioCmd::Beep {
                freq: 1_200.0,
                ms: 80,
                gain: 0.18,
            });
        }
        talking = true;
        let _ = audio_tx.send(AudioCmd::Samples(out[..n].to_vec()));
    }
    if talking && beeps {
        let _ = audio_tx.send(AudioCmd::Beep {
            freq: 440.0,
            ms: 100,
            gain: 0.18,
        });
    }
    Ok(())
}

fn spawn_audio_sink(fx: AudioFx) -> crossbeam_channel::Sender<AudioCmd> {
    let (tx, rx) = crossbeam_channel::unbounded::<AudioCmd>();
    std::thread::Builder::new()
        .name("sociacli-audio".into())
        .spawn(move || {
            use rodio::Source;
            let (_stream, handle) = match rodio::OutputStream::try_default() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(?e, "no audio output device — remote PTT will be silent");
                    return;
                }
            };
            let sink = match rodio::Sink::try_new(&handle) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(?e, "rodio sink failed");
                    return;
                }
            };
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    AudioCmd::Samples(pcm) => {
                        let buf = rodio::buffer::SamplesBuffer::new(1, 48_000, pcm);
                        let src = buf.convert_samples::<f32>();
                        if fx.radio_effect {
                            sink.append(src.high_pass(300).low_pass(3_400));
                        } else {
                            sink.append(src);
                        }
                    }
                    AudioCmd::Beep { freq, ms, gain } => {
                        let s = rodio::source::SineWave::new(freq)
                            .take_duration(Duration::from_millis(ms))
                            .amplify(gain);
                        sink.append(s);
                    }
                }
            }
        })
        .expect("spawn audio thread");
    tx
}
