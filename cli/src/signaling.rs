use crate::config::Config;
use crate::protocol::{ClientMsg, ServerMsg};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub struct Signaling {
    pub tx: mpsc::UnboundedSender<String>,
    pub rx: mpsc::UnboundedReceiver<ServerMsg>,
}

/// Open a WS, returning channels for outbound JSON strings and inbound parsed
/// events. A single background task drives both halves — when *either* side
/// errors (write failure, read failure, server close), both channels close so
/// the daemon's reconnect loop fires. Periodic pings keep idle connections
/// alive through NAT timeouts and detect zombie sockets quickly.
pub async fn connect(cfg: &Config) -> Result<Signaling> {
    let url = cfg.ws_url()?;
    use tokio_tungstenite::tungstenite::Error as WsError;
    let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(v) => v,
        // Inspect the structured HTTP status rather than string-matching the
        // Display (which may not contain the numeric code). The daemon keys off
        // the "(NNN)" suffix in the message to decide mandatory-update vs retry.
        Err(WsError::Http(resp)) => {
            let code = resp.status().as_u16();
            return Err(match code {
                503 => anyhow::anyhow!("server is currently inactive (503)"),
                426 => anyhow::anyhow!(
                    "this sociacli build is below the server's minimum version (426) — run `sociacli update`"
                ),
                other => anyhow::anyhow!("server rejected the connection (HTTP {other})"),
            });
        }
        Err(e) => return Err(anyhow::anyhow!(e.to_string())),
    };
    let (mut sink, mut stream) = ws.split();
    let (tx, mut tx_rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMsg>();

    tokio::spawn(async move {
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(20));
        ping.tick().await; // burn the immediate first tick
        loop {
            tokio::select! {
                outbound = tx_rx.recv() => {
                    let Some(s) = outbound else { break };
                    if let Err(e) = sink.send(Message::Text(s)).await {
                        tracing::warn!(?e, "signaling: outbound write failed — closing");
                        break;
                    }
                }
                inbound = stream.next() => {
                    let Some(frame) = inbound else { break };
                    let frame = match frame {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::warn!(?e, "signaling: inbound read failed — closing");
                            break;
                        }
                    };
                    let text = match frame {
                        Message::Text(t) => t,
                        Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                        Message::Close(_) => {
                            tracing::info!("signaling: server closed the connection");
                            break;
                        }
                    };
                    match serde_json::from_str::<ServerMsg>(&text) {
                        Ok(m) => {
                            if in_tx.send(m).is_err() {
                                break;
                            }
                        }
                        Err(e) => tracing::warn!(?e, raw = %text, "drop unparseable frame"),
                    }
                }
                _ = ping.tick() => {
                    if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                        tracing::warn!(?e, "signaling: ping failed — closing");
                        break;
                    }
                }
            }
        }
        // Both ends close together: drop in_tx (frees `Signaling::rx` so the
        // daemon's `sig.rx.recv() => None` arm fires + triggers reconnect)
        // and close the sink so the server learns.
        let _ = sink.close().await;
        drop(in_tx);
    });

    Ok(Signaling { tx, rx: in_rx })
}

pub fn send(tx: &mpsc::UnboundedSender<String>, msg: &ClientMsg<'_>) -> Result<()> {
    let s = serde_json::to_string(msg)?;
    tx.send(s)?;
    Ok(())
}
