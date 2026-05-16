// Local WebSocket fanout to the Tauri overlay window.
//
// Daemon (`sociacli listen`) spawns a server on ws://127.0.0.1:<port>.
// Overlay window connects; every signaling event the daemon receives gets
// pushed to all connected overlay clients as a JSON `OverlayEvent`.
//
// Some event kinds (config, hello) are "sticky" — cached so a late-arriving
// overlay client still receives the most recent value on connect.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

pub const DEFAULT_PORT: u16 = 8923;
const STICKY_KINDS: &[&str] = &["config", "hello", "friends"];

/// One inbound IPC command from a CLI client. The daemon executes it against
/// its own state / signaling WS and can push back zero or more replies on
/// `reply` (each is a JSON string that the client gets as a Text frame).
pub struct IpcCmd {
    pub req: serde_json::Value,
    pub reply: mpsc::UnboundedSender<String>,
}

/// Inbound command receiver — yielded by `spawn` for the daemon to drain.
pub type CmdRx = mpsc::UnboundedReceiver<IpcCmd>;

#[derive(Clone, Debug, Serialize)]
pub struct OverlayEvent {
    pub kind: String,
    pub payload: serde_json::Value,
}

impl OverlayEvent {
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

#[derive(Clone)]
pub struct OverlayIpc {
    tx: broadcast::Sender<OverlayEvent>,
    sticky: Arc<Mutex<HashMap<String, OverlayEvent>>>,
}

impl OverlayIpc {
    pub fn send(&self, ev: OverlayEvent) {
        if STICKY_KINDS.contains(&ev.kind.as_str()) {
            if let Ok(mut g) = self.sticky.lock() {
                g.insert(ev.kind.clone(), ev.clone());
            }
        }
        let _ = self.tx.send(ev);
    }
}

/// Start the IPC server. Returns a handle the daemon can push to, plus a
/// receiver carrying inbound command frames from any connected client.
///
/// Binds the listener *up front* and returns `Err` if the port is already
/// taken — this is the daemon's single-instance lock. A second daemon that
/// loses the bind must exit, otherwise it would open its own signaling WS with
/// the same token and ping-pong the kick logic with the first daemon forever
/// (the `ws_open`/`ws_close` storm).
pub async fn spawn(port: u16) -> Result<(OverlayIpc, CmdRx)> {
    let listener = bind_with_retry(port).await?;
    tracing::info!("overlay IPC on ws://127.0.0.1:{port}");
    let (tx, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<IpcCmd>();
    let sticky: Arc<Mutex<HashMap<String, OverlayEvent>>> = Arc::new(Mutex::new(HashMap::new()));
    let handle = OverlayIpc {
        tx: tx.clone(),
        sticky: sticky.clone(),
    };
    tokio::spawn(async move {
        if let Err(e) = run(listener, tx, sticky, cmd_tx).await {
            tracing::error!(?e, "overlay IPC server crashed");
        }
    });
    Ok((handle, cmd_rx))
}

/// Bind the IPC port, retrying briefly. A `daemon restart` kills the old
/// daemon and spawns a new one immediately; the OS can hold the old listening
/// port for a fraction of a second, so a single `bind` would lose, trip the
/// single-instance lock, and exit — leaving NO daemon and every command dead.
/// Retrying for ~2s rides out that handover while still giving up if another
/// daemon genuinely owns the port the whole time.
async fn bind_with_retry(port: u16) -> Result<TcpListener> {
    let mut last_err = None;
    for attempt in 0..20u32 {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                if attempt == 0 {
                    tracing::debug!(%port, "IPC port busy — waiting for handover");
                }
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    Err(last_err
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("could not bind IPC port {port}")))
}

async fn run(
    listener: TcpListener,
    tx: broadcast::Sender<OverlayEvent>,
    sticky: Arc<Mutex<HashMap<String, OverlayEvent>>>,
    cmd_tx: mpsc::UnboundedSender<IpcCmd>,
) -> Result<()> {
    loop {
        let (sock, addr) = listener.accept().await?;
        let rx = tx.subscribe();
        let sticky = sticky.clone();
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(sock).await {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(%addr, ?e, "ws upgrade failed");
                    return;
                }
            };
            tracing::info!(%addr, "overlay connected");
            let (sink, stream) = ws.split();
            let replay: Vec<OverlayEvent> = sticky
                .lock()
                .map(|g| g.values().cloned().collect())
                .unwrap_or_default();
            if let Err(e) = serve_client(sink, stream, rx, replay, cmd_tx).await {
                tracing::warn!(%addr, ?e, "overlay client error");
            }
            tracing::info!(%addr, "overlay disconnected");
        });
    }
}

async fn serve_client(
    mut sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    mut stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
    mut rx: broadcast::Receiver<OverlayEvent>,
    replay: Vec<OverlayEvent>,
    cmd_tx: mpsc::UnboundedSender<IpcCmd>,
) -> Result<()> {
    for ev in replay {
        if let Ok(s) = serde_json::to_string(&ev) {
            sink.send(Message::Text(s)).await?;
        }
    }
    // Per-client reply channel: handle_ipc_cmd uses this to push replies for
    // commands originating on *this* socket only.
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(ev) => {
                    let s = serde_json::to_string(&ev)?;
                    sink.send(Message::Text(s)).await?;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "overlay client lagging");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            r = reply_rx.recv() => {
                let Some(s) = r else { continue };
                sink.send(Message::Text(s)).await?;
            }
            frame = stream.next() => match frame {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(Message::Text(t))) => {
                    if let Ok(req) = serde_json::from_str::<serde_json::Value>(&t) {
                        let _ = cmd_tx.send(IpcCmd { req, reply: reply_tx.clone() });
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    }
    Ok(())
}
