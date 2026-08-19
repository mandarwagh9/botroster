//! A Bot can ask for a credential it does not have, and never learns it.
//!
//! The secure secret request: masked, absent from the transcript, never shown
//! to the model. The broker's argument is that the guest can use a credential
//! and never read one. Without this tool, the only way to get one in is a
//! person typing `openbot secret set`, and a Bot that needed a token would
//! have no move except asking in conversation, which puts the value in the
//! model's context and in the log on disk, the failure the broker exists to
//! prevent.
//!
//! The hub asks the person over the same channel as an approval, stores what
//! comes back, and tells the caller a name and a fingerprint.

mod support;

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::approval::SecretRequestResult;
use openbot_proto::frames::*;
use openbot_proto::{Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, SessionId};
use openbotd::hub::Hub;
use openbotd::policy::Policy;
use openbotd::secrets::SecretStore;
use openbotd::server::Server;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const VALUE: &str = "sk-live-NEVER-IN-A-TRANSCRIPT-9f2c";

async fn harness(url: &str) -> anyhow::Result<Sock> {
    let (mut sock, _) = connect_async(url).await?;
    sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => {
            let _: HelloAck = serde_json::from_str(&t)?;
        }
        other => anyhow::bail!("bad handshake: {other:?}"),
    }
    Ok(sock)
}

/// Ask for a credential, answering the hub's request the given way, and return
/// the tool call's own outcome.
async fn request_a_secret(
    answer: Option<&str>,
) -> anyhow::Result<(Outcome, Arc<SecretStore>, tempfile::TempDir)> {
    let dir = tempfile::tempdir()?;
    let secrets = Arc::new(SecretStore::open(dir.path())?);
    let hub = Arc::new(
        Hub::with_policy(Policy::allow_all())
            .with_secrets(Arc::clone(&secrets))
            .with_approval_timeout(Duration::from_secs(3)),
    );
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    let mut sock = harness(&url).await?;
    let send = |id: i64, method: Method, params: serde_json::Value, sid: Option<&SessionId>| {
        let mut req = Request::new(RpcId::Num(id), method, Some(params));
        if let Some(s) = sid {
            req = req.in_session(s.clone());
        }
        Frame::Request(req).encode()
    };

    sock.send(Message::Text(send(1, Method::SessionOpen, json!({}), None)))
        .await?;
    let sid: SessionId = loop {
        let Some(Ok(Message::Text(t))) = sock.next().await else {
            anyhow::bail!("socket closed");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == RpcId::Num(1) {
                let Outcome::Result(v) = r.outcome else {
                    anyhow::bail!("session/open failed");
                };
                break serde_json::from_value::<SessionOpenResult>(v)?.session_id;
            }
        }
    };

    let call = json!({
        "call_id": "call-1",
        "tool_id": "secret.request",
        "args": { "name": "linear-token", "why": "to file the issue you asked for" },
    });
    let call_id = RpcId::Num(2);
    sock.send(Message::Text(send(2, Method::ToolCall, call, Some(&sid))))
        .await?;

    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered the tool call"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed waiting for the hub");
        };
        match Frame::decode(&t)? {
            Frame::Request(r) if r.parsed_method() == Some(Method::SecretRequest) => {
                // The person is asked by name and told why.
                let params = r.params.clone().expect("params");
                assert_eq!(params["name"], "linear-token", "{params}");
                assert!(
                    params["why"].as_str().is_some_and(|w| !w.is_empty()),
                    "the person is asked for a credential with no reason: {params}"
                );
                match answer {
                    Some(v) => {
                        let reply = Response::ok(
                            r.id.clone(),
                            SecretRequestResult {
                                value: Some(v.to_owned()),
                            },
                        );
                        sock.send(Message::Text(Frame::Response(reply).encode()))
                            .await?;
                    }
                    None => {
                        // Declined: answered with nothing at all.
                        let reply = Response::ok(r.id.clone(), SecretRequestResult { value: None });
                        sock.send(Message::Text(Frame::Response(reply).encode()))
                            .await?;
                    }
                }
            }
            Frame::Response(r) if r.id == call_id => return Ok((r.outcome, secrets, dir)),
            _ => {}
        }
    }
}

/// The value is stored, and the caller is told a name and a fingerprint.
#[tokio::test]
async fn a_supplied_credential_is_stored_and_never_returned() -> anyhow::Result<()> {
    let (outcome, secrets, _dir) = request_a_secret(Some(VALUE)).await?;

    let Outcome::Result(v) = outcome else {
        anyhow::bail!("the request failed: {outcome:?}");
    };

    // What the model sees, in full. Serialised, because that is the form it
    // reaches the conversation log and every client in.
    let rendered = v.to_string();
    assert!(
        !rendered.contains(VALUE),
        "the credential came back to the caller: {rendered}"
    );
    assert!(
        !rendered.contains("NEVER-IN-A-TRANSCRIPT"),
        "even part of it came back: {rendered}"
    );
    assert!(rendered.contains("linear-token"), "{rendered}");

    // And it really was stored, so the Bot can now use it through a connector.
    let held = secrets.get("linear-token").expect("stored");
    assert_eq!(held.expose(), VALUE);

    // The fingerprint the caller was given identifies it without revealing it.
    assert!(
        rendered.contains(&held.fingerprint()),
        "the fingerprint does not match what was stored: {rendered}"
    );
    Ok(())
}

/// Declining is a refusal the Bot can act on, and stores nothing.
#[tokio::test]
async fn declining_stores_nothing_and_says_so() -> anyhow::Result<()> {
    let (outcome, secrets, _dir) = request_a_secret(None).await?;

    let Outcome::Error(e) = outcome else {
        anyhow::bail!("a declined request succeeded: {outcome:?}");
    };
    assert!(
        e.message.contains("linear-token"),
        "the refusal should name what was refused: {}",
        e.message
    );
    assert!(
        secrets.get("linear-token").is_err(),
        "a declined request stored something"
    );
    Ok(())
}

/// An unattended run reaches the gate and still stores nothing.
///
/// The shipped policy allows `secret.request` rather than sending it to the
/// `RequireApproval` fallback, because asking a person for a credential is the
/// approval and a second dialog in front of it is one decision costing two
/// prompts. So `--approve auto` passes the gate outright and is stopped by
/// `supply` instead. `no_unattended_mode_hands_over_a_credential` in
/// `openbot-cli` tests `supply` directly and exercises neither gate; this
/// pins the end state through the whole hub: the tool is offered, the call is
/// made, and no credential exists after it.
#[tokio::test]
async fn an_auto_approving_run_passes_the_gate_and_still_supplies_nothing() -> anyhow::Result<()> {
    // The shipped default, not `allow_all`: the point is what a real run does.
    assert_eq!(
        openbotd::policy::Policy::default().evaluate("secret.request", &json!({"name": "x"})),
        openbotd::policy::Verdict::Allow,
        "the gate would stop this before `supply` was ever consulted"
    );

    let (outcome, secrets, _dir) = request_a_secret(None).await?;
    assert!(
        matches!(outcome, Outcome::Error(_)),
        "an unattended run was handed a credential: {outcome:?}"
    );
    assert!(
        secrets.get("linear-token").is_err(),
        "an unattended run stored a credential nobody typed"
    );
    Ok(())
}
