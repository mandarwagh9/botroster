//! An upstream client's hello, answered or refused, on a real hub.
//!
//! Slice B1. The interop peers in `grokbot-recon\interop` are built from
//! `xai-org/grok-build` at SOURCE_REV `559751f` and exist to be a peer this
//! project did not write. Against the hub as it stood at `5ef932c` they stopped
//! at the handshake with `-32007`, and the raw frame said why: the SDK carries
//! its credential in the `Authorization: Bearer` upgrade header and has no token
//! field on its hello at all (`xai-tool-protocol/src/handshake.rs:31-37`), while
//! this hub read a token from nowhere else.
//!
//! These tests hand-write the frames rather than importing upstream's types,
//! because under D1 = A no upstream code enters this repository. Each fixture
//! carries the file it was read from.
//!
//! Two properties are held apart on purpose, and the split is the point of the
//! interop switch:
//!
//! - the **shape** — a bare JSON hello, a `computer_hub_version` ack, a token in
//!   a header — is implemented unconditionally, because it is a superset of what
//!   this hub already accepted and no existing client can tell the difference;
//! - the **version** — answering `1.0.0` at all — is refused unless
//!   `BOTROSTER_INTEROP=1`, because claiming compatibility this project has not
//!   finished proving is the exact claim Phase 0 removed. `botroster-1` remains
//!   spoken either way, so an interop hub is strictly more permissive and never
//!   less.
//!
//! Every negative case asserts the *code* as well as the fact of a refusal,
//! because a hub that refuses everything for the wrong reason passes any test
//! written as "it was refused".

use std::time::Duration;

use botroster_proto::Hello;
use botrosterd::hub::Admission;
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::MaybeTlsStream;

const TOKEN: &str = "the-hub-token";

/// A hub that requires `TOKEN`, with the interop switch as given.
async fn hub(interop: bool) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let hub = Hub::with_policy(Policy::default())
        .admitting(Admission::Token(TOKEN.to_owned()))
        .accepting_upstream_protocol(interop);
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    let handle =
        tokio::spawn(std::sync::Arc::new(Server::new(std::sync::Arc::new(hub))).serve(listener));
    Ok((format!("ws://{addr}/v1/tools"), handle))
}

/// Send one hello and return the hub's first text frame, verbatim.
///
/// Deliberately not typed: the whole question here is what is on the wire, and a
/// parser would answer a different question.
async fn exchange(
    url: &str,
    path_suffix: &str,
    bearer: Option<&str>,
    origin: Option<&str>,
    hello: &str,
) -> anyhow::Result<String> {
    let full = format!("{url}{path_suffix}");
    let mut req = full.as_str().into_client_request()?;
    if let Some(t) = bearer {
        let headers = req.headers_mut();
        headers.insert(
            "authorization",
            format!("Bearer {t}").parse().expect("a header value"),
        );
    }
    if let Some(o) = origin {
        req.headers_mut()
            .insert("origin", o.parse().expect("a header value"));
    }
    let (mut ws, response) = tokio_tungstenite::connect_async(req).await?;
    assert_eq!(response.status(), 101, "the upgrade itself was refused");

    ws.send(Message::Text(hello.to_owned())).await?;
    let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .map_err(|_| anyhow::anyhow!("the hub never answered the hello"))?
        .ok_or_else(|| anyhow::anyhow!("the socket closed with no answer"))?;
    match frame? {
        Message::Text(t) => Ok(t.to_string()),
        Message::Close(_) => Err(anyhow::anyhow!("closed without answering")),
        other => Err(anyhow::anyhow!("not a text frame: {other:?}")),
    }
}

/// The upstream harness hello: bare JSON, no envelope, and no token field,
/// because `HelloMsg` has none (`xai-tool-protocol/src/handshake.rs:31-37`).
const UPSTREAM_HELLO: &str = r#"{"protocol_version":"1.0.0","kind":"harness"}"#;

/// A hello carrying a token in the frame, which is how this project's own
/// clients have always authenticated (`botroster-proto`'s `Hello.token`).
const OWN_HELLO: &str =
    r#"{"protocol_version":"botroster-1","kind":"harness","token":"the-hub-token"}"#;

#[tokio::test]
async fn an_upstream_hello_is_accepted_under_the_interop_switch() {
    let (url, _hub) = hub(true).await.expect("hub");
    let ack: serde_json::Value = serde_json::from_str(
        &exchange(&url, "", Some(TOKEN), None, UPSTREAM_HELLO)
            .await
            .expect("answered"),
    )
    .expect("the ack is JSON");

    // `computer_hub_version` is the name the SDK reads
    // (`xai-tool-protocol/src/handshake.rs:44`). It is what the hub's answer is
    // called upstream, and an SDK parsing our old `hub_version` finds no
    // connection_id... no: it fails to find the field it requires.
    assert!(
        ack.get("computer_hub_version").is_some(),
        "the ack has no `computer_hub_version`: {ack}"
    );
    assert!(
        ack.get("hub_version").is_none(),
        "the ack still carries the old `hub_version` as well: {ack}"
    );
    // And the ack must *offer* 1.0.0, because that is the only thing
    // `send_hello` checks before returning (`handshake.rs:58-66`).
    let offered = ack["supported_protocol_versions"]
        .as_array()
        .expect("supported_protocol_versions is an array");
    assert!(
        offered.iter().any(|v| v == "1.0.0"),
        "the ack does not offer 1.0.0: {ack}"
    );
    assert!(
        offered.iter().any(|v| v == "botroster-1"),
        "an interop hub stopped offering its own version: {ack}"
    );
}

#[tokio::test]
async fn an_upstream_hello_is_refused_when_the_interop_switch_is_off() {
    let (url, _hub) = hub(false).await.expect("hub");
    let frame = exchange(&url, "", Some(TOKEN), None, UPSTREAM_HELLO)
        .await
        .expect("answered");
    let err: serde_json::Value = serde_json::from_str(&frame).expect("the refusal is JSON");

    // -32605 is upstream's own `unsupported_protocol_version`
    // (`error_codes.rs:22`), so the code is one the SDK already understands.
    assert_eq!(
        err["code"], -32605,
        "wrong code for an unsupported version: {frame}"
    );
    // The message has to name what we do speak, or a peer is left guessing.
    let message = err["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("botroster-1"),
        "the refusal does not say which version this hub speaks: {message:?}"
    );
}

#[tokio::test]
async fn a_botroster_hello_is_unaffected_by_the_interop_switch() {
    // The switch must only ever widen. If an interop hub stopped answering
    // `botroster-1`, turning it on would break the project's own clients.
    for interop in [false, true] {
        let (url, _hub) = hub(interop).await.expect("hub");
        let ack = exchange(&url, "", None, None, OWN_HELLO)
            .await
            .unwrap_or_else(|e| panic!("interop={interop}: {e}"));
        let ack: serde_json::Value = serde_json::from_str(&ack).expect("JSON");
        assert!(
            ack.get("computer_hub_version").is_some(),
            "interop={interop}: a BOTROSTER hello was not answered: {ack}"
        );
    }
}

#[tokio::test]
async fn a_token_in_the_hello_still_works_with_no_header() {
    // The existing path, unchanged. An interop hub must not have quietly
    // replaced it with the header.
    let (url, _hub) = hub(true).await.expect("hub");
    let ack = exchange(&url, "", None, None, OWN_HELLO)
        .await
        .expect("answered");
    let ack: serde_json::Value = serde_json::from_str(&ack).expect("JSON");
    assert!(
        ack.get("connection_id").is_some(),
        "the header-less hello was not admitted: {ack}"
    );
}

#[tokio::test]
async fn a_hello_with_no_credential_at_all_is_refused() {
    // Fail closed, and with the code that says so. This is the case the
    // interop work must not have weakened: the header is an *additional* place
    // to present the token, never an alternative to presenting one.
    let (url, _hub) = hub(true).await.expect("hub");
    let frame = exchange(&url, "", None, None, UPSTREAM_HELLO)
        .await
        .expect("answered");
    let err: serde_json::Value = serde_json::from_str(&frame).expect("JSON");
    assert_eq!(err["code"], -32007, "expected UNAUTHENTICATED: {frame}");
}

#[tokio::test]
async fn a_wrong_bearer_is_refused() {
    let (url, _hub) = hub(true).await.expect("hub");
    let frame = exchange(&url, "", Some("not-the-token"), None, UPSTREAM_HELLO)
        .await
        .expect("answered");
    let err: serde_json::Value = serde_json::from_str(&frame).expect("JSON");
    assert_eq!(
        err["code"], -32007,
        "a wrong bearer was not refused as unauthenticated: {frame}"
    );
}

#[tokio::test]
async fn a_bearer_that_is_not_a_bearer_scheme_is_refused() {
    // `Authorization: Basic ...` or a bare token with no scheme must not be
    // accepted by a substring check. The SDK sends `Bearer `; anything else is
    // not the credential this hub issued.
    let (url, _hub) = hub(true).await.expect("hub");
    let mut req = format!("{url}/")
        .as_str()
        .into_client_request()
        .expect("request");
    req.headers_mut().insert(
        "authorization",
        format!("Basic {TOKEN}").parse().expect("header"),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("upgrade");
    ws.send(Message::Text(UPSTREAM_HELLO.into()))
        .await
        .expect("send");
    let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("answered in time")
        .expect("a frame")
        .expect("a readable frame");
    let text = match frame {
        Message::Text(t) => t.to_string(),
        other => panic!("not a text frame: {other:?}"),
    };
    let err: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(
        err["code"], -32007,
        "a non-Bearer authorization was treated as the credential: {text}"
    );
}

#[tokio::test]
async fn a_role_query_is_accepted_and_ignored() {
    // The SDK appends `?role=harness` to the URL. Ignoring it is correct: the
    // hello's own `kind` is what this hub routes on, and the query is not part
    // of the protocol. Refusing the URL would fail for a reason the SDK cannot
    // act on.
    let (url, _hub) = hub(true).await.expect("hub");
    for suffix in ["?role=harness", "?role=tool_server", "?role=bot_client"] {
        let ack = exchange(&url, suffix, Some(TOKEN), None, UPSTREAM_HELLO)
            .await
            .unwrap_or_else(|e| panic!("{suffix} was refused: {e}"));
        let ack: serde_json::Value = serde_json::from_str(&ack).expect("JSON");
        assert!(
            ack.get("connection_id").is_some(),
            "{suffix} produced no connection: {ack}"
        );
    }
}

#[tokio::test]
async fn a_web_origin_is_still_refused_at_the_upgrade() {
    // Unchanged by any of this, and the reason the callback is wrapped rather
    // than replaced: a page in a browser must not be able to drive the computer
    // by presenting a token it somehow has.
    let (url, _hub) = hub(true).await.expect("hub");
    let err = exchange(
        &url,
        "",
        Some(TOKEN),
        Some("https://evil.example"),
        UPSTREAM_HELLO,
    )
    .await
    .expect_err("a page was admitted");
    assert!(
        err.to_string().contains("403") || err.to_string().contains("Forbidden"),
        "expected the upgrade to be refused as forbidden, got: {err}"
    );
}

/// Named so the unused-import lint does not hide a real dependency change: the
/// socket type is part of this file's contract even though `exchange` hides it.
#[allow(dead_code)]
type _Sock = MaybeTlsStream<tokio::net::TcpStream>;

/// Send one JSON-RPC request on a fresh connection and return the answer frame.
async fn rpc(
    url: &str,
    bearer: &str,
    method: &str,
    params: serde_json::Value,
    envelope_session: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let mut req = url.into_client_request()?;
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {bearer}").parse().expect("header"),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;
    ws.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    // One frame, and it is the ack or it is a refusal. There is no second
    // chance and nothing to retry, so this is a read rather than a loop: the
    // first version was written as one and clippy was right that it never
    // iterates.
    let first = match tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("closed during the handshake"))??
    {
        Message::Text(t) => t,
        other => anyhow::bail!("not text: {other:?}"),
    };
    let seen: serde_json::Value = serde_json::from_str(&first)?;
    anyhow::ensure!(
        seen.get("connection_id").is_some(),
        "the handshake was refused: {seen}"
    );

    let mut envelope = serde_json::Map::new();
    envelope.insert("jsonrpc".into(), "2.0".into());
    envelope.insert("id".into(), 7.into());
    envelope.insert("method".into(), method.into());
    envelope.insert("params".into(), params);
    if let Some(s) = envelope_session {
        envelope.insert("session_id".into(), s.into());
    }
    ws.send(Message::Text(serde_json::to_string(&envelope)?))
        .await?;

    let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("closed before the answer"))??;
    let text = match frame {
        Message::Text(t) => t.to_string(),
        other => anyhow::bail!("not text: {other:?}"),
    };
    Ok(serde_json::from_str(&text)?)
}

/// `session_open` must honour a session id carried on the envelope.
///
/// The published protocol has no session field in `session_open` params at all
/// (`xai-tool-protocol/src/frames.rs:620-628`) — the session rides on the
/// envelope, as it does on every other session-scoped method. This hub read the
/// session from params only, so an upstream client asking for
/// `interop-session-1` was handed a freshly minted `sess-N` and then spent the
/// rest of its life naming a session the hub had never heard of: its first
/// `session_bind` came back `no such session`.
///
/// Params keep priority, so a client of this project's own protocol is
/// unaffected. This only adds the case that was silently missing.
#[tokio::test]
async fn a_session_id_on_the_envelope_is_honoured_by_session_open() {
    let (url, _hub) = hub(true).await.expect("hub");
    let result = rpc(
        &url,
        TOKEN,
        "session_open",
        serde_json::json!({ "resume": false }),
        Some("interop-session-1"),
    )
    .await
    .expect("session_open answered");

    assert_eq!(
        result["result"]["session_id"], "interop-session-1",
        "the hub minted its own session id and ignored the one the client asked for: \
         {result}"
    );
}

#[tokio::test]
async fn a_session_id_in_params_still_wins_over_the_envelope() {
    // The reverse precedence would silently break this project's own clients,
    // which are the only ones that send a session in params.
    let (url, _hub) = hub(true).await.expect("hub");
    let result = rpc(
        &url,
        TOKEN,
        "session_open",
        serde_json::json!({ "session_id": "from-params" }),
        Some("from-envelope"),
    )
    .await
    .expect("session_open answered");
    assert_eq!(
        result["result"]["session_id"], "from-params",
        "the envelope overrode the explicit parameter: {result}"
    );
}

#[tokio::test]
async fn session_open_still_mints_an_id_when_the_client_names_none() {
    let (url, _hub) = hub(true).await.expect("hub");
    let result = rpc(&url, TOKEN, "session_open", serde_json::json!({}), None)
        .await
        .expect("session_open answered");
    let minted = result["result"]["session_id"].as_str().unwrap_or_default();
    assert!(
        !minted.is_empty(),
        "no session id came back for a client that named none: {result}"
    );
}
