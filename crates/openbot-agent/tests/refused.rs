//! What a person sees when the hub turns their client away.
//!
//! The hub answers a handshake it will not accept with an `RpcError` that says
//! precisely what is wrong ("unsupported protocol_version …"). A client that
//! parses that reply as a `HelloAck` hits a missing field and reports the
//! parse failure instead:
//!
//! ```text
//! Error: handshake: missing field `connection_id` at line 1 column 89
//! ```
//!
//! which names no version, no mismatch and no fix, while the hub's own
//! sentence sits unused in the payload. These tests hold that the refusal is
//! surfaced verbatim.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// A hub that refuses every handshake with `reply`.
async fn hub_replying(reply: serde_json::Value) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/tools", listener.local_addr().unwrap());

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let reply = reply.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let _ = ws.next().await;
                let _ = ws.send(Message::Text(reply.to_string())).await;
            });
        }
    });

    url
}

#[tokio::test]
async fn a_refused_handshake_reports_what_the_hub_said() {
    let url = hub_replying(json!({
        "code": -32600,
        "message": "unsupported protocol_version \"1.0.0\"; this hub speaks 2.0.0",
    }))
    .await;

    let err = openbot_agent::HubClient::connect(&url)
        .await
        .err()
        .expect("connecting to a hub that refuses should fail");

    let text = err.to_string();
    assert!(
        text.contains("unsupported protocol_version"),
        "the hub's reason was dropped: {text}"
    );
    assert!(
        text.contains("2.0.0"),
        "the version that would fix it was dropped: {text}"
    );
    assert!(
        !text.contains("missing field"),
        "a parse failure is being reported instead of the refusal: {text}"
    );
}

#[tokio::test]
async fn a_reply_that_is_neither_an_ack_nor_an_error_still_says_so() {
    // The fallback has to stay: a hub speaking something else entirely is a
    // different problem from a hub saying no, and reporting it as a refusal
    // would invent a reason nobody gave.
    let url = hub_replying(json!({ "something": "else" })).await;

    let err = openbot_agent::HubClient::connect(&url)
        .await
        .err()
        .expect("a nonsense reply should fail");

    let text = err.to_string();
    assert!(text.contains("handshake"), "{text}");
    assert!(
        !text.contains("refused"),
        "a malformed reply was reported as a refusal: {text}"
    );
}
