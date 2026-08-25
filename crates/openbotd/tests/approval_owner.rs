//! Only the connection that was asked may answer an approval.
//!
//! The gate is the product's central security control: policy is evaluated in
//! the hub, not in the caller, because a check the caller evaluates is a check
//! the caller can delete (SPEC §6.0). That guarantee is only as strong as the
//! answer it waits for.
//!
//! Hub request ids come from one counter (`hub-0`, `hub-1`, ...), so if
//! `on_response` matched a reply by id alone, any connected client could
//! answer a request it was never sent: approve another session's
//! `shell.exec`, or answer `allow_always` and take that tool off the gate for
//! the rest of the session.
//!
//! These tests use two connections and have the second one answer, forge, or
//! observe the first's traffic.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::approval::{ApprovalDecision, Decision};
use openbot_proto::frames::*;
use openbot_proto::{
    Frame, Hello, HelloAck, Method, Notification, Outcome, Request, Response, RpcId, ServerId,
    SessionId, ToolCallId,
};
use openbotd::hub::Hub;
use openbotd::policy::{Policy, Rule};
use openbotd::server::Server;
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

async fn request(
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
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered {method:?}"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == RpcId::Num(id) {
                return Ok(r.outcome);
            }
        }
    }
}

/// A connection that was not asked must not be able to answer.
///
/// The second socket here has its own session and no relationship to the
/// first's tool call. It replies to the hub's approval request id anyway,
/// which is guessable because the hub numbers them from one counter.
#[tokio::test]
async fn an_approval_cannot_be_answered_by_a_bystander() -> anyhow::Result<()> {
    // A short deadline, because the point of this test is what happens when
    // the real approver never answers: the hub must give up and refuse rather
    // than take a stranger's word for it.
    let hub = Arc::new(
        Hub::with_policy(Policy {
            rules: vec![Rule::ask("shell.exec", "running a command")],
            ..Policy::allow_all()
        })
        .with_approval_timeout(Duration::from_secs(3)),
    );
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
        description: "bystander test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ws).await;
    });

    let mut agent = harness(&url).await?;
    for _ in 0..100 {
        if let Outcome::Result(v) =
            request(&mut agent, 900, Method::ServersList, json!({}), None).await?
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

    let Outcome::Result(v) = request(&mut agent, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    request(
        &mut agent,
        2,
        Method::SessionBindServer,
        serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("openbot-workspace"),
        })?,
        Some(&sid),
    )
    .await?;

    // Somebody else, with no part in any of this.
    let mut bystander = harness(&url).await?;

    // The agent asks for something the gate stops.
    let call_id = RpcId::Num(3);
    let req = Request::new(
        call_id.clone(),
        Method::ToolCall,
        Some(json!({
            "call_id": "call-1",
            "tool_id": "shell.exec",
            "args": { "command": "echo hello" },
        })),
    )
    .in_session(sid.clone());
    agent
        .send(Message::Text(Frame::Request(req).encode()))
        .await?;

    // The approval goes to the agent, which does not answer it. The bystander
    // answers instead, with the most generous decision there is.
    let mut asked_id = None;
    for _ in 0..40 {
        let msg = tokio::time::timeout(Duration::from_secs(15), agent.next()).await;
        let Ok(Some(Ok(Message::Text(t)))) = msg else {
            break;
        };
        if let Frame::Request(r) = Frame::decode(&t)? {
            if r.parsed_method() == Some(Method::ApprovalRequest) {
                asked_id = Some(r.id.clone());
                break;
            }
        }
    }
    let asked_id = asked_id.expect("the hub never asked for approval");

    let forged = Response::ok(
        asked_id,
        serde_json::to_value(ApprovalDecision {
            decision: Decision::AllowAlways,
            note: Some("not mine to give".into()),
        })?,
    );
    bystander
        .send(Message::Text(Frame::Response(forged).encode()))
        .await?;

    // The call must not go through on a stranger's say-so. It should end the
    // way an unanswered approval ends: refused when the hub gives up.
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), agent.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered the tool call"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed while waiting for the hub");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == call_id {
                match r.outcome {
                    Outcome::Error(_) => return Ok(()),
                    Outcome::Result(v) => {
                        anyhow::bail!("a connection that was never asked approved this call: {v}")
                    }
                }
            }
        }
    }
}

/// A tool's result may only come from the server it was sent to.
///
/// Hub-to-server requests are keyed `fwd-0`, `fwd-1`, ... from the same
/// counter. If `Relay` recorded who asked but not who was asked, a connected
/// client could answer a forwarded call in the guest's place: a read that
/// never happened, a write reported as done, a command whose output it chose.
///
/// Made deterministic with a tool server that answers the handshake and then
/// says nothing: the call is definitely still in flight while the forgery is
/// sent, and the server reports the exact id it was given, so nothing is
/// guessed. No approval is involved; the policy allows the tool, and what is
/// forged is the answer rather than the permission.
#[tokio::test]
async fn a_tool_result_cannot_be_forged_by_a_bystander() -> anyhow::Result<()> {
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    // A tool server that takes the call and never answers it.
    let (got_fwd_id, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<RpcId>();
    let server_url = url.clone();
    tokio::spawn(async move {
        let (mut sock, _) = connect_async(&server_url).await.expect("connect");
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
                    let reply = Response::ok(r.id.clone(), result);
                    sock.send(Message::Text(Frame::Response(reply).encode()))
                        .await
                        .expect("bind reply");
                }
                Some(Method::ToolCallRequest) => {
                    // The one thing this server does: tell the test which id
                    // it was asked on, and then nothing at all.
                    let _ = got_fwd_id.send(r.id.clone());
                }
                _ => {}
            }
        }
    });

    let mut agent = harness(&url).await?;
    for _ in 0..100 {
        if let Outcome::Result(v) =
            request(&mut agent, 900, Method::ServersList, json!({}), None).await?
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

    let Outcome::Result(v) = request(&mut agent, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    request(
        &mut agent,
        2,
        Method::SessionBindServer,
        serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("silent"),
        })?,
        Some(&sid),
    )
    .await?;

    let call_id = RpcId::Num(3);
    let req = Request::new(
        call_id.clone(),
        Method::ToolCall,
        Some(json!({
            "call_id": "call-1",
            "tool_id": "slow.thing",
            "args": {},
        })),
    )
    .in_session(sid.clone());
    agent
        .send(Message::Text(Frame::Request(req).encode()))
        .await?;

    // The exact id the hub used, straight from the server it went to.
    let fwd_id = tokio::time::timeout(Duration::from_secs(15), fwd_rx.recv())
        .await
        .map_err(|_| anyhow::anyhow!("the hub never forwarded the call"))?
        .expect("an id");

    let mut bystander = harness(&url).await?;
    let forged = Response::ok(
        fwd_id,
        json!({ "call_id": "call-1", "output": "a lie the model would have believed" }),
    );
    bystander
        .send(Message::Text(Frame::Response(forged).encode()))
        .await?;

    // Nothing should come back: the only connection that may answer this is
    // the one still holding it. A forgery arriving instead must be ignored,
    // not delivered.
    let waited = tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(Ok(Message::Text(t))) = agent.next().await {
            if let Ok(Frame::Response(r)) = Frame::decode(&t) {
                if r.id == call_id {
                    return Some(format!("{:?}", r.outcome));
                }
            }
        }
        None
    })
    .await;

    if let Ok(Some(said)) = waited {
        anyhow::bail!("the model was handed a result a bystander wrote: {said}");
    }
    Ok(())
}

/// Progress may only come from the server doing the work.
///
/// `tool_call_progress` is relayed to whichever harness owns the call, looked
/// up by `call_id`, and `call_id` is chosen by the caller (agents number them
/// `call-1`, `c0` and the like), so it is not a secret. Without a check on
/// the sender, a bystander could write into somebody else's record of what
/// their computer was doing: `stage` is rendered verbatim by `openbot run` and
/// by the desktop client.
#[tokio::test]
async fn progress_cannot_be_injected_by_a_bystander() -> anyhow::Result<()> {
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));
    let url = format!("ws://{addr}/v1/tools");

    let (got_fwd_id, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<RpcId>();
    let server_url = url.clone();
    tokio::spawn(async move {
        let (mut sock, _) = connect_async(&server_url).await.expect("connect");
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
                    let _ = got_fwd_id.send(r.id.clone());
                }
                _ => {}
            }
        }
    });

    let mut agent = harness(&url).await?;
    for _ in 0..100 {
        if let Outcome::Result(v) =
            request(&mut agent, 900, Method::ServersList, json!({}), None).await?
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

    let Outcome::Result(v) = request(&mut agent, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    request(
        &mut agent,
        2,
        Method::SessionBindServer,
        serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("silent"),
        })?,
        Some(&sid),
    )
    .await?;

    let req = Request::new(
        RpcId::Num(3),
        Method::ToolCall,
        Some(json!({
            "call_id": "call-1",
            "tool_id": "slow.thing",
            "args": {},
        })),
    )
    .in_session(sid.clone());
    agent
        .send(Message::Text(Frame::Request(req).encode()))
        .await?;

    // The call is in flight and the server is holding it.
    tokio::time::timeout(Duration::from_secs(15), fwd_rx.recv())
        .await
        .map_err(|_| anyhow::anyhow!("the hub never forwarded the call"))?
        .expect("an id");

    let mut bystander = harness(&url).await?;
    let lie = Notification::new(
        Method::ToolCallProgress,
        &ToolCallProgressFrame {
            call_id: ToolCallId::new("call-1"),
            payload: json!({ "stage": "reading your private notes" }),
        },
    );
    bystander
        .send(Message::Text(Frame::Notification(lie).encode()))
        .await?;

    let leaked = tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(Ok(Message::Text(t))) = agent.next().await {
            if let Ok(Frame::Notification(n)) = Frame::decode(&t) {
                if n.parsed_method() == Some(Method::ToolCallProgress) {
                    return Some(format!("{:?}", n.params));
                }
            }
        }
        None
    })
    .await;

    if let Ok(Some(said)) = leaked {
        anyhow::bail!("a bystander wrote into someone else's transcript: {said}");
    }
    Ok(())
}

/// An approval is put to the connection that asked for it, and to nobody
/// else.
///
/// The three above guard the answer: a stranger must not decide somebody
/// else's call. This guards the question, which fails differently. The hub
/// sends the approval to `owner` alone. A hub that broadcast it instead would
/// still be safe from forgery, because `on_response` checks the owner, but it
/// would leak: `ApprovalRequestParams` carries the tool's `args` verbatim, so
/// every connected client would read every other session's commands, paths
/// and message bodies while deciding nothing.
///
/// It is also the assertion underneath `openbot watch`. `ViewerApprovals`
/// decides on the tool's name: while somebody is driving it allows
/// `browser.type` without asking, and it cannot tell that person's keystroke
/// from a Bot typing at the same moment. It does not need to, only because
/// the question is never put to it. This test holds that assumption for it.
#[tokio::test]
async fn an_approval_is_never_shown_to_a_bystander() -> anyhow::Result<()> {
    // Distinctive enough that finding it anywhere on the wrong socket is
    // proof rather than coincidence.
    const NEEDLE: &str = "leak-canary-9f3a7c";

    let hub = Arc::new(
        Hub::with_policy(Policy {
            rules: vec![Rule::ask("shell.exec", "running a command")],
            ..Policy::allow_all()
        })
        .with_approval_timeout(Duration::from_secs(3)),
    );
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
        description: "bystander visibility test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ws).await;
    });

    let mut agent = harness(&url).await?;
    for _ in 0..100 {
        if let Outcome::Result(v) =
            request(&mut agent, 900, Method::ServersList, json!({}), None).await?
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

    let Outcome::Result(v) = request(&mut agent, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    request(
        &mut agent,
        2,
        Method::SessionBindServer,
        serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("openbot-workspace"),
        })?,
        Some(&sid),
    )
    .await?;

    // Connected before the call, so a broadcast would reach it.
    let mut bystander = harness(&url).await?;

    let req = Request::new(
        RpcId::Num(3),
        Method::ToolCall,
        Some(json!({
            "call_id": "call-1",
            "tool_id": "shell.exec",
            "args": { "command": format!("echo {NEEDLE}") },
        })),
    )
    .in_session(sid.clone());
    agent
        .send(Message::Text(Frame::Request(req).encode()))
        .await?;

    // First prove the question was asked at all, and that the needle really
    // does travel in it. Without this the silence below would be the silence
    // of a call that was never gated, which is no evidence of anything.
    let mut asked = None;
    for _ in 0..40 {
        let msg = tokio::time::timeout(Duration::from_secs(15), agent.next()).await;
        let Ok(Some(Ok(Message::Text(t)))) = msg else {
            break;
        };
        if let Frame::Request(r) = Frame::decode(&t)? {
            if r.parsed_method() == Some(Method::ApprovalRequest) {
                asked = Some((r.id.clone(), t.clone()));
                break;
            }
        }
    }
    let (asked_id, asked_text) = asked.expect("the hub never asked the caller for approval");
    assert!(
        asked_text.contains(NEEDLE),
        "the approval the caller was shown does not carry the tool's arguments, \
         so this test is not watching for anything"
    );

    // The caller has it. A broadcast copy would already be queued.
    let mut seen = Vec::new();
    while let Ok(Some(Ok(Message::Text(t)))) =
        tokio::time::timeout(Duration::from_millis(1500), bystander.next()).await
    {
        seen.push(t);
    }
    for t in &seen {
        if let Ok(Frame::Request(r)) = Frame::decode(t) {
            assert_ne!(
                r.parsed_method(),
                Some(Method::ApprovalRequest),
                "a connection with no part in this call was asked to approve it"
            );
        }
        assert!(
            !t.contains(NEEDLE),
            "another session's tool arguments reached an unrelated connection: {t}"
        );
    }

    // Answered so the hub is not left waiting out its timeout.
    let deny = Response::ok(
        asked_id,
        serde_json::to_value(ApprovalDecision {
            decision: Decision::Deny,
            note: None,
        })?,
    );
    agent
        .send(Message::Text(Frame::Response(deny).encode()))
        .await?;
    Ok(())
}
