//! A session id belongs to the connection that opened it.
//!
//! `session_close` has always checked the owner. `session_open` has not: it ends
//! with an unconditional `st.sessions.insert(...)` (`botrosterd/src/hub.rs`, the
//! insert after the replay block). So the one method that *creates* a session
//! was the one method that let anyone take one over, and the symptom was not an
//! error at all — it was a working session that silently changed hands.
//!
//! What a takeover actually costs, since that is what makes it worth a task:
//! the insert replaces the whole `Session`, so the new owner arrives with the
//! bound tool server gone, the tool catalogue empty, the session's policy grants
//! reset to the hub default, and any replay in flight discarded. A second
//! connection asking for an id it guessed — and `next("sess")` mints sequential
//! ids, so ids are guessable by construction — could take over a running
//! session and the original owner would see `not the owner of this session` on
//! everything afterwards, with nothing in the log saying how it happened.
//!
//! Two facts make this worse rather than better. `disconnect` deletes every
//! session the connection owned (`st.sessions.retain(|_, s| s.owner != *id)`), so
//! a takeover also hands the departing connection's cleanup to the thief: when
//! the *original* owner later disconnects it deletes nothing, because the
//! session no longer belongs to it, and the session outlives both of them. And
//! `principal.session_ids` is appended to unconditionally, so a takeover also
//! leaves the thief holding an authorisation for a session whose grants were
//! just reset.
//!
//! Tests 3 and 4 characterise behaviour that is **not** being changed here. They
//! are here so that a later change to them is a decision rather than a surprise.

use std::sync::Arc;
use std::time::Duration;

use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, ServerId, SessionId,
    ToolCallId, ToolCallParams, ToolId,
};
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A tool server that answers a bind and runs whatever it is asked to run.
///
/// The point of a real one rather than a stub is that "A's session still has its
/// server" is a claim about state the hub holds, and the only honest way to show
/// it survived a takeover attempt is for a call to still reach a server and come
/// back. A mocked bind would let a hub that dropped the server on the floor pass.
struct OneToolServer {
    server_id: ServerId,
    tool_id: ToolId,
}

impl OneToolServer {
    async fn start(url: &str, server_id: &str, tool_id: &str) -> Self {
        let id = ServerId::new(server_id);
        let tool_id = ToolId::new(tool_id);
        let hello = Hello::tool_server(id.as_str());
        let url = url.to_owned();
        let owned_tool = tool_id.clone();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = connect_async(url).await else {
                return;
            };
            if sock
                .send(Message::Text(serde_json::to_string(&hello).unwrap()))
                .await
                .is_err()
            {
                return;
            }
            if !matches!(sock.next().await, Some(Ok(Message::Text(_)))) {
                return;
            }
            while let Some(Ok(Message::Text(t))) = sock.next().await {
                let Ok(Frame::Request(r)) = Frame::decode(&t) else {
                    continue;
                };
                // Serialised to a `Value` rather than boxed as a trait object:
                // two different result types come back from one match, and the
                // hub reads them off the wire anyway, so this is the same value
                // with one less layer.
                let reply: Option<serde_json::Value> = match r.parsed_method() {
                    // The session comes from `params` and not from the envelope.
                    // A published server drops a bind that has neither, silently.
                    Some(Method::SessionBind) => {
                        let params = r.params.clone().unwrap_or(json!(null));
                        if params.get("session_id").and_then(|v| v.as_str()).is_none() {
                            continue;
                        }
                        Some(
                            serde_json::to_value(SessionBindResult {
                                tools: vec![ToolDescription::new(
                                    owned_tool.as_str(),
                                    "the one tool this server has",
                                    json!({ "type": "object", "properties": {} }),
                                )],
                                binary_version: Some("one-tool-0.1.0".to_owned()),
                            })
                            .unwrap(),
                        )
                    }
                    Some(Method::ToolCallRequest) => {
                        let params: ToolCallRequestParams =
                            serde_json::from_value(r.params.clone().unwrap_or(json!(null)))
                                .unwrap_or_else(|_| ToolCallRequestParams {
                                    tool_id: owned_tool.clone(),
                                    call_id: ToolCallId::new("unknown"),
                                    args: json!({}),
                                });
                        Some(
                            serde_json::to_value(ToolCallResult {
                                call_id: params.call_id,
                                output: json!({ "ran": params.tool_id.as_str() }),
                            })
                            .unwrap(),
                        )
                    }
                    _ => None,
                };
                let Some(result) = reply else { continue };
                let out = Response::ok(r.id.clone(), result);
                if sock
                    .send(Message::Text(Frame::Response(out).encode()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            server_id: id,
            tool_id,
        }
    }
}

/// `allow_all` so a call reaches the tool server. The gate under test is
/// ownership, and a policy that asked about the tool would park every call on an
/// approval this file has no way to answer.
async fn hub() -> anyhow::Result<String> {
    let hub = Hub::with_policy(Policy::allow_all())
        .with_call_timeout(Duration::from_secs(10))
        .binding_within(Duration::from_secs(5));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::new(hub))).serve(listener));
    Ok(format!("ws://{addr}/v1/tools"))
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

/// Open a session, naming the id in `params`, on the envelope, or not at all.
///
/// The plan requires all three forms to be refused, because a client of this
/// project's protocol puts the id in `params` and a published one puts it on the
/// envelope, so a check that only looked in one place would look correct against
/// half the protocol.
async fn open(sock: &mut Sock, id: &str, where_: Carriage) -> anyhow::Result<Outcome> {
    let (params, envelope) = match where_ {
        Carriage::Params => (json!({ "session_id": id }), None),
        Carriage::Envelope => (json!({}), Some(SessionId::new(id))),
        Carriage::Unspecified => (json!({}), None),
    };
    let mut req = Request::new(RpcId::Num(1), Method::SessionOpen, Some(params));
    if let Some(s) = envelope {
        req = req.in_session(s);
    }
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    reply_to(&mut *sock, Method::SessionOpen).await
}

#[derive(Clone, Copy)]
enum Carriage {
    Params,
    Envelope,
    Unspecified,
}

async fn bind(sock: &mut Sock, sid: &SessionId, server_id: &ServerId) -> anyhow::Result<Outcome> {
    let req = Request::new(
        RpcId::Num(2),
        Method::SessionBindServer,
        Some(json!({ "server_id": server_id.as_str() })),
    )
    .in_session(sid.clone());
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    reply_to(sock, Method::SessionBindServer).await
}

async fn call(sock: &mut Sock, sid: &SessionId, tool: &ToolId) -> anyhow::Result<Outcome> {
    let params = ToolCallParams {
        tool_id: tool.clone(),
        call_id: ToolCallId::new("call-1"),
        args: json!({}),
    };
    let req = Request::new(
        RpcId::Num(3),
        Method::ToolCall,
        Some(serde_json::to_value(&params).unwrap()),
    )
    .in_session(sid.clone());
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    reply_to(sock, Method::ToolCall).await
}

async fn close(sock: &mut Sock, sid: &SessionId) -> anyhow::Result<Outcome> {
    let req =
        Request::new(RpcId::Num(9), Method::SessionClose, Some(json!({}))).in_session(sid.clone());
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    reply_to(sock, Method::SessionClose).await
}

async fn reply_to(sock: &mut Sock, method: Method) -> anyhow::Result<Outcome> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered {method:?}"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed awaiting {method:?}");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            // Only our own replies: a tool server on another connection may be
            // answering something on this socket's hub.
            if matches!(
                r.id,
                RpcId::Num(1) | RpcId::Num(2) | RpcId::Num(3) | RpcId::Num(9)
            ) {
                return Ok(r.outcome);
            }
        }
    }
}

fn sid_of(o: Outcome) -> anyhow::Result<SessionId> {
    let Outcome::Result(v) = o else {
        anyhow::bail!("expected a session id, got an error")
    };
    Ok(serde_json::from_value::<SessionOpenResult>(v)?.session_id)
}

fn err_of(o: Outcome) -> Option<(i32, String)> {
    match o {
        Outcome::Error(e) => Some((e.code, e.message)),
        Outcome::Result(_) => None,
    }
}

/// Test 1. A second connection asking for an id another connection opened is
/// refused, and the first connection's session is intact afterwards.
///
/// The consequence half matters as much as the refusal: "refused" would be a
/// cheap way to pass this test by dropping the session on the floor, which is
/// what the takeover already does. So the assertion is that A's call still
/// reaches a tool server and returns, and that B's does not.
#[tokio::test]
async fn a_session_id_belongs_to_the_connection_that_opened_it() -> anyhow::Result<()> {
    let url = hub().await?;
    let server = OneToolServer::start(&url, "tool-1", "peer:echo").await;
    let mut a = harness(&url).await?;
    let mut b = harness(&url).await?;

    let s1 = sid_of(open(&mut a, "s1", Carriage::Params).await?)?;
    assert!(matches!(
        bind(&mut a, &s1, &server.server_id).await?,
        Outcome::Result(_)
    ));

    let taken = open(&mut b, "s1", Carriage::Params).await?;
    let (code, message) =
        err_of(taken).expect("a second connection was allowed to open a session another one owns");
    assert_eq!(
        code,
        botroster_proto::codes::FORBIDDEN,
        "the refusal should be FORBIDDEN, and it must say who owns it: {message}"
    );
    assert!(
        message.contains("s1"),
        "the refusal does not name the session it is about: {message}"
    );

    // A's session survived intact: still owns it, still has its tool server.
    assert!(
        matches!(
            call(&mut a, &s1, &server.tool_id).await?,
            Outcome::Result(_)
        ),
        "the first connection lost its own session to a refused takeover"
    );
    // And B did not gain one.
    assert!(
        err_of(call(&mut b, &s1, &server.tool_id).await?).is_some(),
        "the connection that was refused a session could still act on it"
    );
    Ok(())
}

/// Test 2. The same, for the other carriage form.
///
/// A client of this project's protocol names the session in `params`; a
/// published one names it on the envelope, because the published `session_open`
/// params have no session field at all. A check that read only one of them would
/// look right against half the protocol and be a no-op against the other half.
#[tokio::test]
async fn neither_carriage_of_the_session_id_lets_a_second_connection_in() -> anyhow::Result<()> {
    let url = hub().await?;
    let mut a = harness(&url).await?;
    let mut b = harness(&url).await?;
    sid_of(open(&mut a, "s1", Carriage::Params).await?)?;

    let on_envelope = open(&mut b, "s1", Carriage::Envelope).await?;
    assert!(
        err_of(on_envelope).is_some(),
        "a session named on the request envelope could be taken over, and the \
         published protocol names it there"
    );
    Ok(())
}

/// Test 3. Characterisation, not a fix: reopening **your own** session replaces
/// it, dropping the bound server and the tool catalogue.
///
/// **Is that intended? No, and it is left alone here on purpose.** It is the same
/// defect with the same owner, so the narrowest correct fix — refuse only a
/// *different* owner — leaves it in place. But it is the behaviour a reconnect
/// would hit, and PLAN-proto-match-2 N5.3 wants exactly that case to keep its
/// server, tools and grants. So this test is written to fail loudly when N5.3
/// changes it, rather than to be quietly edited. See the note in `session_open`
/// about where the exception belongs.
#[tokio::test]
async fn reopening_your_own_session_replaces_it_and_that_is_still_the_case() -> anyhow::Result<()> {
    let url = hub().await?;
    let server = OneToolServer::start(&url, "tool-1", "peer:echo").await;
    let mut a = harness(&url).await?;

    let s1 = sid_of(open(&mut a, "s1", Carriage::Params).await?)?;
    assert!(matches!(
        bind(&mut a, &s1, &server.server_id).await?,
        Outcome::Result(_)
    ));

    // The same connection, the same id: allowed, and the session is rebuilt.
    let again = open(&mut a, "s1", Carriage::Params).await?;
    assert_eq!(
        sid_of(again)?,
        s1,
        "the hub changed the id it was asked for"
    );

    let outcome = call(&mut a, &s1, &server.tool_id).await?;
    assert!(
        err_of(outcome).is_some(),
        "reopening your own session now KEEPS the bound server. That is the \
         change N5.3 intends - update this test deliberately, do not just make \
         it pass."
    );
    Ok(())
}

/// Test 4. Characterisation, also not a fix: a session outlives the connection
/// that opened it only for as long as that connection is alive.
///
/// `disconnect` retains away every session whose owner is going away, so after a
/// disconnect the id is free and another connection may have it. This is the
/// other half of the takeover problem: a thief makes the departing owner's
/// cleanup a no-op, so a session can outlive both connections. Recorded so that
/// changing the retention — which N5.3 will want to, to give a reconnect a grace
/// period — is a decision.
#[tokio::test]
async fn a_disconnected_sessions_id_is_free_for_the_next_connection() -> anyhow::Result<()> {
    let url = hub().await?;
    let mut a = harness(&url).await?;
    let mut b = harness(&url).await?;
    sid_of(open(&mut a, "s1", Carriage::Params).await?)?;

    // Closing the session is the owner's own business and is checked.
    assert!(
        matches!(
            close(&mut a, &SessionId::new("s1")).await?,
            Outcome::Result(_)
        ),
        "the owner could not close its own session"
    );
    sid_of(open(&mut a, "s1", Carriage::Params).await?)?;

    // Dropping the socket is what the retain runs on.
    drop(a);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let reclaimed = open(&mut b, "s1", Carriage::Params).await?;
    sid_of(reclaimed)?;
    Ok(())
}

/// Test 5. A minted id skips ids already in use.
///
/// `next("sess")` hands out sequential ids from a counter, so they are guessable,
/// and a client is allowed to name its own. A client that names the id the hub is
/// about to mint gets it; the next client then gets the same one, and the second
/// open replaces the first session — the takeover, arrived at from the other
/// direction. The fix is a mint loop that skips ids in use.
///
/// Deterministic without reading the counter: the same connection opens a session
/// and gets `sess-<n>`; an explicit id draws nothing, so the next mint on that
/// connection would be `sess-<n+1>` unless something already holds it.
#[tokio::test]
async fn a_minted_session_id_skips_ids_already_in_use() -> anyhow::Result<()> {
    let url = hub().await?;
    let mut a = harness(&url).await?;

    let first = sid_of(open(&mut a, "unused", Carriage::Unspecified).await?)?;
    let n: u32 = first
        .as_str()
        .strip_prefix("sess-")
        .expect("the hub mints ids as sess-<n>")
        .parse()?;
    let would_be_next = format!("sess-{}", n + 1);

    // A client takes the id the hub is about to hand out.
    sid_of(open(&mut a, &would_be_next, Carriage::Params).await?)?;

    let after = sid_of(open(&mut a, "unused", Carriage::Unspecified).await?)?;
    assert_ne!(
        after.as_str(),
        would_be_next,
        "the hub minted `{would_be_next}`, which a client already holds, so the \
         second open replaced that client's session"
    );
    Ok(())
}
