//! Who gets to open a connection to the hub at all.
//!
//! Every other control in this crate is downstream of this one. The policy gate
//! lives in the hub so the caller cannot delete it, approvals are addressed to
//! the session's owner so a stranger cannot answer them — and all of that is
//! only worth anything if the peer on the other end had to be entitled to be
//! there.
//!
//! It was not. Every connection was handed `dev_principal()`, which carries
//! `SCOPE_TOOL_INVOKE`, and an approval is authorised on socket identity — so a
//! peer that opens its own session is the owner of it and is the one the hub
//! asks for permission. A page could approve its own `shell.exec`.
//!
//! The two tests here are the two halves of the same question, and the second
//! one is why this file is not just an assertion that the first one passes.

use std::sync::Arc;
use std::time::Duration;

use botroster_proto::{Hello, HelloAck};
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

async fn hub_url() -> anyhow::Result<String> {
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));
    Ok(format!("ws://{addr}/v1/tools"))
}

/// A page in a browser cannot open a connection.
///
/// Browsers do not apply CORS to WebSocket handshakes: `new
/// WebSocket("ws://127.0.0.1:8443/v1/tools")` from any page a person visits
/// reaches the listener, and the handshake is a plain JSON text frame a page can
/// send. The port is a fixed default in both binaries, so there is nothing to
/// guess.
///
/// A browser is required to send `Origin` and cannot be made not to, which is
/// what makes this checkable at all.
#[tokio::test]
async fn a_connection_that_announces_itself_as_a_web_page_is_refused() -> anyhow::Result<()> {
    let url = hub_url().await?;

    let mut req = url.as_str().into_client_request()?;
    req.headers_mut()
        .insert("origin", "https://evil.example".parse()?);

    let refused = tokio_tungstenite::connect_async(req).await;
    let Err(e) = refused else {
        panic!(
            "a page claiming to be https://evil.example completed the upgrade. While a computer \
             is running, that page can read the workspace, drive the logged-in browser, and run \
             shell.exec by approving its own request."
        );
    };
    let shown = e.to_string();
    assert!(
        shown.contains("403") || shown.to_lowercase().contains("forbidden"),
        "refused, but not in a way a client can act on: {shown}"
    );
    Ok(())
}

/// A native client, which sends no Origin, still connects.
///
/// The anti-vacuity half, and the one that makes this a real test rather than a
/// restatement of the fix. A listener that refused everything would satisfy the
/// assertion above and would be a far worse outcome than the hole it closes:
/// the CLI, the desktop client and the guest all connect here.
#[tokio::test]
async fn a_native_client_sending_no_origin_still_connects() -> anyhow::Result<()> {
    let url = hub_url().await?;

    let (mut sock, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(&url),
    )
    .await??;

    // All the way through the handshake, not merely a completed upgrade: the
    // check runs before `register`, and getting that ordering wrong would
    // refuse real clients at a place this test would otherwise not reach.
    sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => {
            let _: HelloAck = serde_json::from_str(&t)?;
        }
        other => panic!("a client sending no Origin was not allowed to handshake: {other:?}"),
    }
    Ok(())
}
