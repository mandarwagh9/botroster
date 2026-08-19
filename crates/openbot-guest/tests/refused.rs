//! What a guest does when the hub refuses it.
//!
//! Upgrading a hub underneath a running guest is the ordinary operational
//! sequence the reconnect logic exists to survive. If the upgraded hub refuses
//! the guest's protocol version, two properties matter:
//!
//! * The hub's refusal says exactly what is wrong, and the guest must pass
//!   that on. Parsing the reply as a `HelloAck` fails on a missing field, and
//!   reporting that instead would surface a version mismatch as
//!   `missing field \`connection_id\``, which names neither version nor any
//!   way to fix it.
//! * Retrying is right for an outage and wrong for a refusal. Waiting does not
//!   change a protocol version, and meanwhile `openbot status` reports no
//!   computer attached while the guest is silently turned away every thirty
//!   seconds.
//!
//! The fake hub below covers both: it accepts once so the guest gets past its
//! first connection, then refuses everything after.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// A hub that accepts the first handshake and refuses every one after it.
///
/// Returns the URL to point a guest at. The listener lives as long as the
/// task, which the test's runtime drops at the end.
async fn hub_that_turns_sour(code: i32, message: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/tools", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                // The hello. Its contents do not matter to this test.
                let _ = ws.next().await;

                let first = seen.fetch_add(1, Ordering::Relaxed) == 0;
                let reply = if first {
                    json!({
                        "connection_id": "conn-1",
                        "user_id": "test-user",
                        "hub_version": "0.0.1",
                        "supported_protocol_versions": ["1.0.0"],
                        "capabilities": [],
                    })
                } else {
                    json!({ "code": code, "message": message })
                };
                let _ = ws.send(Message::Text(reply.to_string())).await;

                if first {
                    // Drop the accepted connection, which is what a hub
                    // restart looks like from the guest's side.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            });
        }
    });

    url
}

fn guest(
    url: &str,
    dir: &std::path::Path,
) -> (openbot_guest::GuestConfig, Arc<openbot_guest::Context>) {
    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.join("computer"), true).unwrap(),
        dir.join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.to_owned(),
        server_id: "refused-test".into(),
        description: "a guest the hub will turn away".into(),
    };
    (cfg, ctx)
}

#[tokio::test]
async fn a_hub_that_refuses_forever_is_not_retried_forever() {
    let dir = tempfile::tempdir().unwrap();
    // -32600 is `INVALID_REQUEST`: this guest asked for something the hub will
    // not grant. It will not grant it in thirty seconds either.
    let url = hub_that_turns_sour(-32600, "unsupported protocol_version \"1.0.0\"").await;
    let (cfg, ctx) = guest(&url, dir.path());

    // Backoff runs 0.5s, 1s, 2s, ... to a 30s ceiling and never stops, so
    // anything that returns at all has stopped intentionally. Twenty seconds
    // is long enough to be sure and short enough for a test suite.
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        openbot_guest::run_supervised(cfg, ctx),
    )
    .await
    .expect("the guest was still retrying a refusal it will never get past");

    let err = out.expect_err("a refused guest reported success");
    let text = err.to_string();
    assert!(
        text.contains("unsupported protocol_version"),
        "the hub said why and the guest did not pass it on: {text}"
    );
}

#[tokio::test]
async fn a_hub_that_is_merely_unhappy_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    // Not a contract violation: a hub that is starting up, or briefly
    // failing. Giving up on this would turn a blip into an outage needing
    // operator intervention.
    let url = hub_that_turns_sour(-32603, "internal error").await;
    let (cfg, ctx) = guest(&url, dir.path());

    let out = tokio::time::timeout(
        Duration::from_secs(6),
        openbot_guest::run_supervised(cfg, ctx),
    )
    .await;

    assert!(
        out.is_err(),
        "the guest gave up on a transient failure instead of retrying: {:?}",
        out.map(|r| r.map_err(|e| e.to_string()))
    );
}
