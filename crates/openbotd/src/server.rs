//! WebSocket listener: accept, authorise, handshake, then pump frames.

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::{Frame, Hello, Principal};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::hub::Hub;

/// How long to let a closing connection's writer flush before giving up.
const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Server {
    pub hub: Arc<Hub>,
}

impl Server {
    pub fn new(hub: Arc<Hub>) -> Self {
        Self { hub }
    }

    /// Bind and return the real local address, so a caller (a test, or a
    /// supervisor writing a port file) can learn the port when binding to :0.
    pub async fn bind(addr: &str) -> anyhow::Result<(TcpListener, SocketAddr)> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        Ok((listener, local))
    }

    /// Accept until the listener dies. Each connection runs on its own task.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let me = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = me.connection(stream, peer).await {
                            tracing::debug!(%peer, error = %e, "connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::error!(error = %e, "accept failed");
                    return;
                }
            }
        }
    }

    async fn connection(&self, stream: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
        // Nagle hurts here: frames are small and latency-sensitive.
        stream.set_nodelay(true).ok();

        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut sink, mut source) = ws.split();

        // The client never announces who it is; identity is derived from the
        // upgrade credential. Until OIDC lands this is the dev principal.
        let principal: Principal = crate::hub::dev_principal();

        // Handshake: a bare `hello`, answered by a bare `hello_ack`.
        let hello: Hello = match source.next().await {
            Some(Ok(Message::Text(t))) => serde_json::from_str(&t)
                .map_err(|e| anyhow::anyhow!("malformed hello: {e} ({})", shape(&t)))?,
            Some(Ok(other)) => anyhow::bail!("expected a text hello, got {other:?}"),
            Some(Err(e)) => anyhow::bail!("socket error during handshake: {e}"),
            None => anyhow::bail!("peer closed before sending hello"),
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (conn_id, ack) = match self.hub.register(&hello, principal, tx).await {
            Ok(v) => v,
            Err(e) => {
                // Report the refusal on the wire before closing, so the client
                // gets a diagnosable reason rather than a bare disconnect.
                let _ = sink.send(Message::Text(serde_json::to_string(&e)?)).await;
                let _ = sink.close().await;
                anyhow::bail!("handshake refused: {}", e.message);
            }
        };
        sink.send(Message::Text(serde_json::to_string(&ack)?))
            .await?;

        tracing::info!(%peer, conn = %conn_id, kind = ?hello.kind, server_id = ?hello.server_id, "peer connected");

        // Writer task owns the sink.
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        while let Some(msg) = source.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(e) => {
                    tracing::debug!(conn = %conn_id, error = %e, "read error");
                    break;
                }
            };
            match Frame::decode(&text) {
                Ok(frame) => self.hub.on_frame(&conn_id, frame).await,
                Err(e) => {
                    tracing::warn!(conn = %conn_id, error = %e, frame = %shape(&text), "undecodable frame")
                }
            }
        }

        // `disconnect` drops the Conn, which drops the outbound sender, which
        // ends the writer loop on its own. Await that drain rather than
        // aborting it: an abort races the flush and can discard a frame
        // already queued, including a final error response the peer needs to
        // diagnose why it was dropped. The timeout bounds a wedged socket.
        self.hub.disconnect(&conn_id).await;
        if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer)
            .await
            .is_err()
        {
            tracing::debug!(conn = %conn_id, "writer did not drain in time");
        }
        tracing::info!(conn = %conn_id, "peer disconnected");
        Ok(())
    }
}

/// Describe an undecodable frame without quoting what was in it.
///
/// Every frame a peer sends crosses this path on its way to `Frame::decode`,
/// including the response to `secret.request`, which carries a credential a
/// person just typed. Logging the raw payload at `warn` (the level operators
/// keep and ship) would write that credential to disk in clear text whenever a
/// version skew, a renamed field, or a truncated write made serde refuse.
///
/// Undecodable means the shape is unknown, so nothing can be selectively
/// redacted; the only safe rule is to report structure and never a value.
/// Keys and the method name are enough to tell a version skew from a truncated
/// write from garbage, which is what this log is for.
fn shape(text: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        // Not even JSON: there are no keys to name, and the bytes are exactly
        // what must not be quoted.
        return format!("unparseable, {} bytes", text.len());
    };
    let serde_json::Value::Object(map) = v else {
        return format!("{}, {} bytes", kind(&v), text.len());
    };
    let mut keys: Vec<String> = map
        .keys()
        // Bounded in both directions for the same reason the method name is:
        // this frame just failed to decode, and a `warn` line whose length
        // the sender chooses is its own problem. A key is a name in the
        // protocol; nothing legitimate here is long or plentiful.
        .map(|k| k.chars().take(40).collect::<String>())
        .collect();
    keys.sort_unstable();
    let over = keys.len().saturating_sub(12);
    keys.truncate(12);
    let more = if over > 0 {
        format!(",+{over} more")
    } else {
        String::new()
    };
    // The method is a name from the protocol, not payload, and it is usually
    // the reason the decode failed. Bounded anyway, since the frame is
    // untrusted.
    let method = match map.get("method") {
        Some(serde_json::Value::String(m)) => {
            let m: String = m.chars().take(40).collect();
            format!(" method={m}")
        }
        _ => String::new(),
    };
    format!(
        "keys=[{}{more}]{method}, {} bytes",
        keys.join(","),
        text.len()
    )
}

fn kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame that failed to decode is described, never quoted.
    ///
    /// The case that matters is the response to `secret.request`: it carries a
    /// credential, it crosses this path, and the log level is `warn`.
    #[test]
    fn an_undecodable_frame_is_described_without_its_values() {
        const VALUE: &str = "sk-live-NEVER-IN-A-LOG-9f2c";
        let s = shape(&format!(
            r#"{{"id":7,"method":"secret.request","result":{{"value":"{VALUE}"}},"unknown":1}}"#
        ));
        assert!(!s.contains(VALUE), "the credential is in the log line: {s}");
        assert!(
            !s.contains("sk-live"),
            "a prefix of the credential is in the log line: {s}"
        );
        // Still diagnosable: which frame, which fields, how big.
        assert!(s.contains("secret.request"), "{s}");
        assert!(s.contains("result") && s.contains("unknown"), "{s}");
        assert!(s.contains("bytes"), "{s}");
    }

    /// Values hide in every shape, not only in objects.
    #[test]
    fn a_bare_or_broken_payload_is_not_quoted_either() {
        const VALUE: &str = "sk-live-NEVER-IN-A-LOG-9f2c";
        for payload in [
            format!("\"{VALUE}\""),         // a bare JSON string
            format!("[\"{VALUE}\"]"),       // an array
            format!("{{\"a\":\"{VALUE}\""), // truncated mid-write: not JSON at all
        ] {
            let s = shape(&payload);
            assert!(!s.contains(VALUE), "leaked from {payload}: {s}");
            assert!(s.contains("bytes"), "{s}");
        }
    }

    /// The sender does not get to choose how long the log line is.
    ///
    /// Everything echoed here comes from a frame that failed to decode, so
    /// every part of it is bounded: the method name, each key, and the number
    /// of keys.
    #[test]
    fn nothing_the_sender_controls_is_unbounded() {
        let long = "x".repeat(500);
        for payload in [
            format!(r#"{{"method":"{long}"}}"#),
            format!(r#"{{"{long}":1}}"#),
            format!(
                "{{{}}}",
                (0..200)
                    .map(|i| format!(r#""k{i}":1"#))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ] {
            let s = shape(&payload);
            assert!(
                s.len() < 300,
                "the sender chose a {}-char log line: {s}",
                s.len()
            );
        }
    }
}
