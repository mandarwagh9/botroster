//! Every forwarded tool call ends, one way or another.
//!
//! A `tool.call` used to have exactly one ending: the tool server answering it.
//! If the server died or simply stopped replying, the relay stayed in the hub's
//! map forever and the harness was told nothing at all. The Bot stopped
//! mid-task, the transcript ended without an error, `botroster computer status`
//! showed a healthy reconnected guest, and the only recovery was killing the
//! run.
//!
//! That is not an exotic failure. The guest drives a browser, and `docs/SPEC.md`
//! §5 spends a section on browsers dying underneath it.
//!
//! `Hub::inflight_calls` is documented in `hub.rs` as the indicator that
//! routing state has leaked. This was a leak it reported and nothing acted on,
//! and `WorkspaceUnavailable` — with a `Disconnect` reason and an
//! `InFlightCancelled` phase — was defined in the protocol for precisely this
//! and had no uses anywhere outside `botroster-proto`.
//!
//! Both tests here hang forever against the code they were written for.

use std::sync::Arc;
use std::time::Duration;

use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, ServerId, SessionId,
    ToolCallId, ToolId, WorkspaceGonePhase, WorkspaceGoneReason, WorkspaceUnavailable,
    WORKSPACE_UNAVAILABLE_CODE,
};
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

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

/// A tool server that binds, reports the call it was given, and then nothing.
///
/// Returning the socket to the caller is the point: a test that needs the
/// server to *die* has to be able to drop it at a chosen moment, and one that
/// needs it merely silent has to be able to keep it alive.
async fn silent_server(
    url: &str,
    saw_call: tokio::sync::mpsc::UnboundedSender<RpcId>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let url = url.to_owned();
    Ok(tokio::spawn(async move {
        let (mut sock, _) = connect_async(&url).await.expect("connect");
        let hello = Hello::tool_server("silent").with_description("answers nothing");
        sock.send(Message::Text(serde_json::to_string(&hello).unwrap()))
            .await
            .expect("hello");
        while let Some(Ok(Message::Text(t))) = sock.next().await {
            let Ok(Frame::Request(r)) = Frame::decode(&t) else {
                continue;
            };
            match r.parsed_method() {
                Some(Method::SessionBind) => {
                    let result = SessionBindResult {
                        tools: vec![ToolDescription::new(
                            "slow.thing",
                            "never answers",
                            json!({ "type": "object", "properties": {} }),
                        )],
                        binary_version: None,
                    };
                    sock.send(Message::Text(
                        Frame::Response(Response::ok(r.id.clone(), result)).encode(),
                    ))
                    .await
                    .expect("bind reply");
                }
                Some(Method::ToolCallRequest) => {
                    let _ = saw_call.send(r.id.clone());
                }
                _ => {}
            }
        }
    }))
}

/// Open a session and bind the silent server to it.
async fn bound_session(agent: &mut Sock, url: &str) -> anyhow::Result<SessionId> {
    // Wait for the server to have registered, rather than sleeping a guess.
    for _ in 0..100 {
        let mut probe = harness(url).await?;
        let out = one(&mut probe, 900, Method::ServersList, json!({}), None).await?;
        if let Outcome::Result(v) = out {
            if !serde_json::from_value::<ServersListResult>(v)?
                .servers
                .is_empty()
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let Outcome::Result(v) = one(agent, 1, Method::SessionOpen, json!({}), None).await? else {
        anyhow::bail!("session.open failed");
    };
    let sid = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    let bind = serde_json::to_value(SessionBindServerParams {
        server_id: ServerId::new("silent"),
    })?;
    let out = one(agent, 2, Method::SessionBindServer, bind, Some(&sid)).await?;
    if let Outcome::Error(e) = out {
        anyhow::bail!("session_bind_server failed: {e:?}");
    }
    Ok(sid)
}

/// Send one request and wait for its reply.
async fn one(
    sock: &mut Sock,
    id: i64,
    method: Method,
    params: serde_json::Value,
    session: Option<&SessionId>,
) -> anyhow::Result<Outcome> {
    let mut req = Request::new(RpcId::Num(id), method, Some(params));
    if let Some(s) = session {
        req = req.in_session(s.clone());
    }
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    await_reply(sock, id, Duration::from_secs(15)).await
}

/// Wait for the reply to `id`, ignoring anything else that arrives.
async fn await_reply(sock: &mut Sock, id: i64, budget: Duration) -> anyhow::Result<Outcome> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            anyhow::bail!("nothing answered id {id} within {budget:?}");
        }
        let Ok(msg) = tokio::time::timeout(left, sock.next()).await else {
            anyhow::bail!("nothing answered id {id} within {budget:?}");
        };
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed while waiting for id {id}");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == RpcId::Num(id) {
                return Ok(r.outcome);
            }
        }
    }
}

/// The `WorkspaceUnavailable` payload on an error, if it carries one.
fn workspace_gone(outcome: &Outcome) -> Option<WorkspaceUnavailable> {
    let Outcome::Error(e) = outcome else {
        return None;
    };
    if e.code != WORKSPACE_UNAVAILABLE_CODE {
        return None;
    }
    e.data
        .clone()
        .and_then(|d| serde_json::from_value::<WorkspaceUnavailable>(d).ok())
}

/// A tool server that dies mid-call ends the call, rather than leaving it.
///
/// `disconnect` cleaned up `calls` and `relays` by *origin*, which releases what
/// a departing harness was waiting on. A relay whose **target** disappeared
/// matched neither filter and stayed in the map with nobody coming to answer it.
#[tokio::test]
async fn a_tool_server_that_dies_mid_call_fails_it_rather_than_hanging() -> anyhow::Result<()> {
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    let (saw_call, mut calls) = tokio::sync::mpsc::unbounded_channel();
    let server = silent_server(&url, saw_call).await?;

    let mut agent = harness(&url).await?;
    let sid = bound_session(&mut agent, &url).await?;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("slow.thing"),
        call_id: ToolCallId::new("c1"),
        args: json!({}),
    })?;
    agent
        .send(Message::Text(
            Frame::Request(
                Request::new(RpcId::Num(10), Method::ToolCall, Some(params))
                    .in_session(sid.clone()),
            )
            .encode(),
        ))
        .await?;

    // The server has it. Now it dies, which is what a crashed guest looks like
    // from here.
    tokio::time::timeout(Duration::from_secs(10), calls.recv())
        .await
        .map_err(|_| anyhow::anyhow!("the tool server never received the call"))?
        .expect("the channel is open");
    server.abort();

    let outcome = await_reply(&mut agent, 10, Duration::from_secs(10)).await?;
    let gone = workspace_gone(&outcome).ok_or_else(|| {
        anyhow::anyhow!(
            "expected a WorkspaceUnavailable error naming the disconnect, got {outcome:?}"
        )
    })?;
    assert_eq!(gone.reason, WorkspaceGoneReason::Disconnect);
    assert_eq!(
        gone.phase,
        WorkspaceGonePhase::InFlightCancelled,
        "the call was in flight when the server went away, and saying so is how a client tells \
         this apart from a call that never found a route"
    );

    // And the routing state is back to where it started. This is the assertion
    // `Hub::inflight_calls` exists for.
    assert_eq!(hub.inflight_calls().await, 0, "the in-flight call leaked");
    assert_eq!(hub.pending_relays().await, 0, "the relay leaked");
    Ok(())
}

/// A server that stays connected and stops answering is given a deadline.
///
/// The disconnect path cannot help here: nothing closes. This is a wedged
/// browser or a guest deadlocked on its own lock, and without a deadline it is
/// the same forever-hang by a different route.
#[tokio::test]
async fn a_tool_server_that_goes_quiet_is_not_waited_on_forever() -> anyhow::Result<()> {
    // Two seconds, because an hour is not a test. The shipped default is long
    // on purpose — `shell.exec` takes a caller-supplied timeout and advertises
    // a maximum of an hour, so a tight deadline here would cancel legitimate
    // work and be worse than the hang it replaces.
    let hub =
        Arc::new(Hub::with_policy(Policy::allow_all()).with_call_timeout(Duration::from_secs(2)));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    let (saw_call, _calls) = tokio::sync::mpsc::unbounded_channel();
    let _server = silent_server(&url, saw_call).await?;

    let mut agent = harness(&url).await?;
    let sid = bound_session(&mut agent, &url).await?;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("slow.thing"),
        call_id: ToolCallId::new("c2"),
        args: json!({}),
    })?;
    agent
        .send(Message::Text(
            Frame::Request(
                Request::new(RpcId::Num(20), Method::ToolCall, Some(params))
                    .in_session(sid.clone()),
            )
            .encode(),
        ))
        .await?;

    let outcome = await_reply(&mut agent, 20, Duration::from_secs(10)).await?;
    let gone = workspace_gone(&outcome).ok_or_else(|| {
        anyhow::anyhow!("expected a WorkspaceUnavailable error from the deadline, got {outcome:?}")
    })?;
    assert_eq!(
        gone.reason,
        WorkspaceGoneReason::IdleTimeout,
        "a server that is still connected and has stopped answering is not a disconnect, and a \
         client that retries on one and not the other needs them apart"
    );
    assert_eq!(hub.inflight_calls().await, 0, "the in-flight call leaked");
    assert_eq!(hub.pending_relays().await, 0, "the relay leaked");
    Ok(())
}

/// A call that is answered normally is not touched by any of this.
///
/// The anti-vacuity test. Both assertions above are satisfied by a hub that
/// fails every call the moment it is made, which would be a far worse product
/// than the hang. This is the one that says the ordinary path still works, and
/// that the deadline does not fire early on a call that is merely slow.
#[tokio::test]
async fn a_call_that_is_answered_is_left_alone() -> anyhow::Result<()> {
    let hub =
        Arc::new(Hub::with_policy(Policy::allow_all()).with_call_timeout(Duration::from_secs(30)));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    // A server that answers, but takes its time about it: long enough that a
    // deadline armed at the wrong scale would have fired.
    let server_url = url.clone();
    tokio::spawn(async move {
        let (mut sock, _) = connect_async(&server_url).await.expect("connect");
        let hello = Hello::tool_server("slow").with_description("answers eventually");
        sock.send(Message::Text(serde_json::to_string(&hello).unwrap()))
            .await
            .expect("hello");
        while let Some(Ok(Message::Text(t))) = sock.next().await {
            let Ok(Frame::Request(r)) = Frame::decode(&t) else {
                continue;
            };
            match r.parsed_method() {
                Some(Method::SessionBind) => {
                    let result = SessionBindResult {
                        tools: vec![ToolDescription::new(
                            "slow.thing",
                            "answers eventually",
                            json!({ "type": "object", "properties": {} }),
                        )],
                        binary_version: None,
                    };
                    sock.send(Message::Text(
                        Frame::Response(Response::ok(r.id.clone(), result)).encode(),
                    ))
                    .await
                    .expect("bind reply");
                }
                Some(Method::ToolCallRequest) => {
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    sock.send(Message::Text(
                        Frame::Response(Response::ok(r.id.clone(), json!({ "did": "the thing" })))
                            .encode(),
                    ))
                    .await
                    .expect("tool reply");
                }
                _ => {}
            }
        }
    });

    let mut agent = harness(&url).await?;
    // `bound_session` binds whatever registered; here that is the slow server.
    let sid = {
        for _ in 0..100 {
            let mut probe = harness(&url).await?;
            if let Outcome::Result(v) =
                one(&mut probe, 900, Method::ServersList, json!({}), None).await?
            {
                if !serde_json::from_value::<ServersListResult>(v)?
                    .servers
                    .is_empty()
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let Outcome::Result(v) = one(&mut agent, 1, Method::SessionOpen, json!({}), None).await?
        else {
            anyhow::bail!("session.open failed");
        };
        let sid = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
        let bind = serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("slow"),
        })?;
        let out = one(&mut agent, 2, Method::SessionBindServer, bind, Some(&sid)).await?;
        if let Outcome::Error(e) = out {
            anyhow::bail!("session_bind_server failed: {e:?}");
        }
        sid
    };

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("slow.thing"),
        call_id: ToolCallId::new("c3"),
        args: json!({}),
    })?;
    let outcome = one(&mut agent, 30, Method::ToolCall, params, Some(&sid)).await?;
    match &outcome {
        Outcome::Result(v) => assert_eq!(v["did"], "the thing"),
        Outcome::Error(e) => panic!("a slow but healthy call was failed: {e:?}"),
    }
    assert_eq!(hub.inflight_calls().await, 0);
    assert_eq!(hub.pending_relays().await, 0);
    Ok(())
}
