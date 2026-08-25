//! When an approval does not happen, the denial says which way it did not
//! happen.
//!
//! Three things stop a call at the gate: nobody answered, the approver
//! answered with an error, and the approver answered with something that could
//! not be read. All three deny; the gate fails closed and that is not in
//! question here. What these tests hold is that the three are reported
//! differently. Two of them are answers, arriving promptly. Somebody whose
//! approver is crashing, or emitting a decision this hub cannot parse, must
//! not be told their approver never replied, which would send them to look at
//! timeouts, networks and sleeping laptops instead of at the component that
//! is broken.
//!
//! The tool call is `shell.exec` under a policy that asks, so the hub really
//! does open the gate; the harness below answers the hub's approval request by
//! hand, deliberately badly.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::*;
use openbot_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, ServerId, SessionId,
};
use openbotd::hub::Hub;
use openbotd::policy::{Policy, Rule};
use openbotd::server::Server;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// How the harness answers the hub's approval request.
#[derive(Clone, Copy)]
enum Answer {
    /// A well-formed reply that is not an `ApprovalDecision`.
    Unreadable,
    /// An RPC error, as an approver whose own code threw would send.
    Error,
}

/// Run one gated `shell.exec`, answering the approval the given way, and
/// return the error the hub sent back for the tool call.
async fn denial_message(answer: Answer) -> anyhow::Result<String> {
    let hub = Arc::new(Hub::with_policy(Policy {
        rules: vec![Rule::ask("shell.exec", "running a command")],
        ..Policy::allow_all()
    }));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ws = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "openbot-workspace".into(),
        description: "approval-reasons guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ws).await;
    });

    let mut sock = harness(&url).await?;
    // Wait for the guest, rather than sleeping a guess.
    for _ in 0..100 {
        let v = request(&mut sock, 900, Method::ServersList, json!({}), None).await?;
        if let Outcome::Result(v) = v {
            if !serde_json::from_value::<ServersListResult>(v)?
                .servers
                .is_empty()
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let Outcome::Result(v) = request(&mut sock, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;

    let bind = serde_json::to_value(SessionBindServerParams {
        server_id: ServerId::new("openbot-workspace"),
    })?;
    request(&mut sock, 2, Method::SessionBindServer, bind, Some(&sid)).await?;

    // The call the gate will stop.
    let call = json!({
        "call_id": "call-1",
        "tool_id": "shell.exec",
        "args": { "command": "echo hello" },
    });
    let id = RpcId::Num(3);
    let req = Request::new(id.clone(), Method::ToolCall, Some(call)).in_session(sid.clone());
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;

    // Now answer the hub's approval request the wrong way, then read on until
    // the tool call's own response arrives.
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered the tool call"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed while waiting for the hub");
        };
        match Frame::decode(&t)? {
            Frame::Request(r) if r.parsed_method() == Some(Method::ApprovalRequest) => {
                let reply = match answer {
                    Answer::Unreadable => {
                        Response::ok(r.id.clone(), json!({ "verdict": "sure, go ahead" }))
                    }
                    Answer::Error => Response::err(
                        r.id.clone(),
                        -32000,
                        "the approver crashed rendering the card",
                    ),
                };
                sock.send(Message::Text(Frame::Response(reply).encode()))
                    .await?;
            }
            Frame::Response(r) if r.id == id => {
                return match r.outcome {
                    Outcome::Error(e) => Ok(e.message),
                    Outcome::Result(v) => {
                        anyhow::bail!("the call was allowed through the gate: {v}")
                    }
                }
            }
            _ => {}
        }
    }
}

async fn harness(url: &str) -> anyhow::Result<Sock> {
    let (mut sock, _) = connect_async(url).await?;
    sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => {
            let _: HelloAck = serde_json::from_str(&t)?;
        }
        other => anyhow::bail!("bad handshake reply: {other:?}"),
    }
    Ok(sock)
}

async fn request(
    sock: &mut Sock,
    id: i64,
    method: Method,
    params: serde_json::Value,
    session: Option<&SessionId>,
) -> anyhow::Result<Outcome> {
    let id = RpcId::Num(id);
    let mut req = Request::new(id.clone(), method, Some(params));
    if let Some(s) = session {
        req = req.in_session(s.clone());
    }
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out awaiting {method}"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed awaiting {method}");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == id {
                return Ok(r.outcome);
            }
        }
    }
}

#[tokio::test]
async fn an_approver_that_errors_is_not_reported_as_a_timeout() -> anyhow::Result<()> {
    let msg = denial_message(Answer::Error).await?;
    assert!(
        msg.contains("the approver returned an error"),
        "the denial does not say the approver failed: {msg}"
    );
    assert!(
        msg.contains("crashed rendering the card"),
        "the approver's own message was dropped: {msg}"
    );
    assert!(
        !msg.contains("in time"),
        "an answer that arrived at once is reported as a timeout: {msg}"
    );
    Ok(())
}

#[tokio::test]
async fn an_unreadable_decision_says_it_could_not_be_read() -> anyhow::Result<()> {
    let msg = denial_message(Answer::Unreadable).await?;
    assert!(
        msg.contains("could not be read"),
        "an unparseable decision is not described: {msg}"
    );
    assert!(
        !msg.contains("in time"),
        "an answer that arrived at once is reported as a timeout: {msg}"
    );
    Ok(())
}
