//! Takeover: a person at the keyboard locks the agent out of the computer.
//!
//! The feature exists for the moment someone is typing a one-time code into a
//! page the agent opened. If "takeover" is only a state in whatever is drawing
//! the screen, the agent keeps clicking while they type, which is the failure
//! the feature is supposed to prevent. So the lock lives in the hub, and these
//! tests drive it over real sockets.
//!
//! The central one is [`a_dropped_viewer_gives_the_computer_back`]. A lock
//! that is only released on a clean shutdown means a Ctrl-C'd viewer locks an
//! agent out of its own computer forever, with no command to clear it.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::*;
use openbot_proto::{
    codes, Frame, Hello, Method, Outcome, Request, RpcId, ServerId, SessionId, ToolCallId,
    ToolCallParams, ToolId,
};
use openbotd::hub::Hub;
use openbotd::policy::Policy;
use openbotd::server::Server;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Harness {
    sock: Sock,
    next_id: i64,
}

impl Harness {
    async fn connect(url: &str) -> anyhow::Result<Self> {
        let (mut sock, _) = connect_async(url).await?;
        sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
            .await?;
        let _ack = sock.next().await;
        Ok(Self { sock, next_id: 1 })
    }

    async fn call(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<Outcome> {
        let id = RpcId::Num(self.next_id);
        self.next_id += 1;
        let mut req = Request::new(id.clone(), method, Some(params));
        if let Some(s) = session {
            req = req.in_session(s.clone());
        }
        self.sock
            .send(Message::Text(Frame::Request(req).encode()))
            .await?;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), self.sock.next())
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

    async fn ok(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<serde_json::Value> {
        match self.call(method, params, session).await? {
            Outcome::Result(v) => Ok(v),
            Outcome::Error(e) => anyhow::bail!("{method} failed: [{}] {}", e.code, e.message),
        }
    }

    /// Open a session and bind the guest.
    async fn session(&mut self) -> anyhow::Result<SessionId> {
        let v = self.ok(Method::SessionOpen, json!({}), None).await?;
        let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
        self.ok(
            Method::SessionBindServer,
            serde_json::to_value(SessionBindServerParams {
                server_id: ServerId::new("openbot-workspace"),
            })?,
            Some(&sid),
        )
        .await?;
        Ok(sid)
    }

    async fn take_over(&mut self, sid: &SessionId, why: &str) -> anyhow::Result<Outcome> {
        self.call(
            Method::ComputerTakeover,
            serde_json::to_value(ComputerTakeoverParams {
                server_id: ServerId::new("openbot-workspace"),
                reason: why.into(),
            })?,
            Some(sid),
        )
        .await
    }

    async fn release(&mut self, sid: &SessionId) -> anyhow::Result<Outcome> {
        self.call(
            Method::ComputerRelease,
            serde_json::to_value(ComputerReleaseParams {
                server_id: ServerId::new("openbot-workspace"),
            })?,
            Some(sid),
        )
        .await
    }

    /// A cheap, always-permitted tool call, used to ask "can I still act?".
    async fn touch(&mut self, sid: &SessionId, n: &str) -> anyhow::Result<Outcome> {
        self.call(
            Method::ToolCall,
            serde_json::to_value(ToolCallParams {
                call_id: ToolCallId::new(n),
                tool_id: ToolId::new("fs.list"),
                args: json!({ "path": "." }),
            })?,
            Some(sid),
        )
        .await
    }
}

async fn boot(policy: Policy) -> anyhow::Result<(String, tempfile::TempDir)> {
    let hub = Arc::new(Hub::with_policy(policy));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "openbot-workspace".into(),
        description: "test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ctx).await;
    });

    let mut probe = Harness::connect(&url).await?;
    for _ in 0..100 {
        let v = probe.ok(Method::ServersList, json!({}), None).await?;
        if !serde_json::from_value::<ServersListResult>(v)?
            .servers
            .is_empty()
        {
            return Ok((url, dir));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("guest never registered")
}

fn code(o: &Outcome) -> i32 {
    match o {
        Outcome::Error(e) => e.code,
        Outcome::Result(v) => panic!("expected an error, got {v}"),
    }
}

#[tokio::test]
async fn while_a_person_holds_the_computer_the_agent_cannot_act() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;

    let mut agent = Harness::connect(&url).await?;
    let agent_sid = agent.session().await?;
    // Working normally before anyone takes over.
    assert!(matches!(
        agent.touch(&agent_sid, "before").await?,
        Outcome::Result(_)
    ));

    let mut viewer = Harness::connect(&url).await?;
    let viewer_sid = viewer.session().await?;
    let claimed: ComputerTakeoverResult = serde_json::from_value(
        viewer
            .ok(
                Method::ComputerTakeover,
                serde_json::to_value(ComputerTakeoverParams {
                    server_id: ServerId::new("openbot-workspace"),
                    reason: "entering a 2FA code".into(),
                })?,
                Some(&viewer_sid),
            )
            .await?,
    )?;
    assert!(claimed.claimed);

    let refused = agent.touch(&agent_sid, "during").await?;
    assert_eq!(
        code(&refused),
        codes::TAKEN_OVER,
        "the agent kept its computer while a person was typing into it"
    );
    if let Outcome::Error(e) = &refused {
        // The agent has to be able to say what happened, so the reason travels.
        assert!(
            e.message.contains("2FA"),
            "unhelpful refusal: {}",
            e.message
        );
    }

    // The person holding it can still drive.
    assert!(matches!(
        viewer.touch(&viewer_sid, "person").await?,
        Outcome::Result(_)
    ));

    viewer.release(&viewer_sid).await?;
    assert!(matches!(
        agent.touch(&agent_sid, "after").await?,
        Outcome::Result(_)
    ));
    Ok(())
}

/// The viewer's socket dies without a release (Ctrl-C, a closed laptop, a
/// crashed tab). If the lock outlives it, the agent is locked out of its own
/// computer permanently and there is no command to clear it.
#[tokio::test]
async fn a_dropped_viewer_gives_the_computer_back() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;

    let mut agent = Harness::connect(&url).await?;
    let agent_sid = agent.session().await?;

    {
        let mut viewer = Harness::connect(&url).await?;
        let sid = viewer.session().await?;
        viewer.take_over(&sid, "paying for something").await?;
        assert_eq!(
            code(&agent.touch(&agent_sid, "locked").await?),
            codes::TAKEN_OVER
        );
        // No release. Just gone.
        drop(viewer);
    }

    // The hub notices on its own schedule; poll rather than sleep blind.
    for _ in 0..100 {
        if let Outcome::Result(_) = agent.touch(&agent_sid, "recovered").await? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("the computer stayed locked after the viewer vanished")
}

/// The lock has to survive the policy path, not just the routing path; every
/// other test here runs under `allow_all`, which skips it entirely.
#[tokio::test]
async fn the_lock_holds_under_the_default_policy() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::default()).await?;

    let mut agent = Harness::connect(&url).await?;
    let agent_sid = agent.session().await?;
    // `fs.list` is allow-listed by default, so this is a clean read of the
    // gate rather than an approval prompt.
    assert!(matches!(
        agent.touch(&agent_sid, "before").await?,
        Outcome::Result(_)
    ));

    let mut viewer = Harness::connect(&url).await?;
    let sid = viewer.session().await?;
    viewer.take_over(&sid, "signing in").await?;

    assert_eq!(
        code(&agent.touch(&agent_sid, "during").await?),
        codes::TAKEN_OVER,
        "an allow-listed tool ran while a person held the computer"
    );
    Ok(())
}

#[tokio::test]
async fn two_people_cannot_hold_one_computer() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;

    let mut first = Harness::connect(&url).await?;
    let a = first.session().await?;
    first.take_over(&a, "first").await?;

    let mut second = Harness::connect(&url).await?;
    let b = second.session().await?;
    let refused = second.take_over(&b, "second").await?;
    assert_eq!(code(&refused), codes::TAKEN_OVER);

    // And the second cannot release what it does not hold; otherwise an agent
    // could drop a person's lock mid-password.
    assert_eq!(code(&second.release(&b).await?), codes::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn claiming_twice_from_one_session_is_idempotent() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;
    let mut viewer = Harness::connect(&url).await?;
    let sid = viewer.session().await?;

    let first: ComputerTakeoverResult =
        serde_json::from_value(match viewer.take_over(&sid, "why").await? {
            Outcome::Result(v) => v,
            Outcome::Error(e) => anyhow::bail!("{}", e.message),
        })?;
    assert!(first.claimed);

    // A viewer that reconnects and re-claims should not have to track whether
    // it already holds the lock.
    let again: ComputerTakeoverResult =
        serde_json::from_value(match viewer.take_over(&sid, "why").await? {
            Outcome::Result(v) => v,
            Outcome::Error(e) => anyhow::bail!("re-claiming failed: {}", e.message),
        })?;
    assert!(!again.claimed, "a re-claim reported as a fresh claim");
    Ok(())
}

#[tokio::test]
async fn releasing_a_computer_nobody_holds_is_not_an_error() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;
    let mut h = Harness::connect(&url).await?;
    let sid = h.session().await?;

    // The caller wanted the computer free. It is.
    let r: ComputerReleaseResult = serde_json::from_value(match h.release(&sid).await? {
        Outcome::Result(v) => v,
        Outcome::Error(e) => anyhow::bail!("{}", e.message),
    })?;
    assert!(!r.released);
    Ok(())
}

#[tokio::test]
async fn a_takeover_names_a_computer_that_exists() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;
    let mut h = Harness::connect(&url).await?;
    let sid = h.session().await?;

    let out = h
        .call(
            Method::ComputerTakeover,
            serde_json::to_value(ComputerTakeoverParams {
                server_id: ServerId::new("not-a-computer"),
                reason: "typo".into(),
            })?,
            Some(&sid),
        )
        .await?;
    assert_eq!(code(&out), codes::NO_SERVER_BOUND);
    Ok(())
}

/// Hub-served tools do not touch the computer, so a takeover must not stop a
/// Bot handing work to another Bot.
#[tokio::test]
async fn a_takeover_does_not_freeze_the_rest_of_the_account() -> anyhow::Result<()> {
    let (url, _d) = boot(Policy::allow_all()).await?;
    let mut viewer = Harness::connect(&url).await?;
    let vs = viewer.session().await?;
    viewer.take_over(&vs, "at the keyboard").await?;

    let mut other = Harness::connect(&url).await?;
    let os = other.session().await?;
    // Sessions, listings and the rest of the control plane keep working.
    let v = other.ok(Method::ServersList, json!({}), None).await?;
    assert!(!serde_json::from_value::<ServersListResult>(v)?
        .servers
        .is_empty());
    assert!(other
        .ok(Method::ToolsList, json!({}), Some(&os))
        .await
        .is_ok());
    Ok(())
}

/// An agent that hits a takeover must stop, not spend its whole budget.
///
/// If the refusal arrived as an ordinary failed tool, the model would rephrase
/// and try again until the step limit. A routine at 6am would burn a full
/// run's tokens on a computer somebody was using and then report `StepLimit`,
/// which reads as "the agent could not do it" rather than "a person was at the
/// keyboard".
#[tokio::test]
async fn an_agent_stops_when_a_person_takes_the_computer() -> anyhow::Result<()> {
    use openbot_agent::agent::{AgentConfig, FinishReason};
    use openbot_agent::providers::Scripted;
    use openbot_agent::{Agent, AllowAll};

    let (url, _d) = boot(Policy::allow_all()).await?;

    // A person is already driving.
    let mut viewer = Harness::connect(&url).await?;
    let vs = viewer.session().await?;
    viewer.take_over(&vs, "signing in to the bank").await?;

    // The agent asks for the same tool over and over, as a model would when
    // told its last attempt failed.
    let script = Scripted::builder()
        .call("fs.list", serde_json::json!({ "path": "." }))
        .call("fs.list", serde_json::json!({ "path": "." }))
        .call("fs.list", serde_json::json!({ "path": "." }))
        .say("I gave up.")
        .build();

    let (hub, progress) =
        openbot_agent::HubClient::connect_with(&url, std::sync::Arc::new(AllowAll)).await?;
    hub.open_session().await?;
    hub.bind_server("openbot-workspace").await?;

    let agent = Agent::new(
        std::sync::Arc::new(script),
        std::sync::Arc::clone(&hub),
        AgentConfig {
            max_steps: 8,
            ..Default::default()
        },
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = agent.run("list the workspace", vec![], progress, tx).await;

    match &outcome.reason {
        FinishReason::ComputerBusy { message } => {
            assert!(
                message.contains("signing in to the bank"),
                "the reason did not travel: {message}"
            );
        }
        other => panic!("expected the run to stop for a takeover, got {other:?}"),
    }
    // One attempt, not a budget's worth.
    assert_eq!(outcome.steps, 1, "the agent kept trying");
    Ok(())
}

/// A `PreToolUse` hook denies a call in the hub, not in the client.
///
/// The point of running it here is that no client cooperation is involved: a
/// harness cannot skip the hook by not calling it, because it never called it.
#[tokio::test]
async fn a_hook_refuses_a_call_and_the_tool_never_runs() -> anyhow::Result<()> {
    use openbotd::hooks::{HookVerdict, PreToolUse};

    struct NoWrites;
    #[async_trait::async_trait]
    impl PreToolUse for NoWrites {
        async fn check(&self, _s: &SessionId, tool: &str, _a: &serde_json::Value) -> HookVerdict {
            if tool == "fs.write" {
                HookVerdict::Deny("writes are frozen during the incident".into())
            } else {
                HookVerdict::NoObjection
            }
        }
    }

    // `allow_all`, so nothing but the hook can be responsible for a refusal.
    let hub =
        Arc::new(Hub::with_policy(Policy::allow_all()).with_hooks(std::sync::Arc::new(NoWrites)));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    tokio::spawn(async move {
        let _ = openbot_guest::run(
            openbot_guest::GuestConfig {
                hub_url: url.clone(),
                server_id: "openbot-workspace".into(),
                description: "hook test guest".into(),
                token: None,
            },
            ctx,
        )
        .await;
    });

    let ws = format!("ws://{addr}/v1/tools");
    let mut h = Harness::connect(&ws).await?;
    let sid = loop {
        match h.session().await {
            Ok(s) => break s,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };

    let refused = h
        .call(
            Method::ToolCall,
            serde_json::to_value(ToolCallParams {
                call_id: ToolCallId::new("w1"),
                tool_id: ToolId::new("fs.write"),
                args: json!({ "path": "notes.md", "contents": "hello" }),
            })?,
            Some(&sid),
        )
        .await?;

    match &refused {
        Outcome::Error(e) => {
            assert_eq!(e.code, codes::APPROVAL_DENIED);
            assert!(
                e.message.contains("frozen during the incident"),
                "the hook's reason did not reach the caller: {}",
                e.message
            );
        }
        other => panic!("the hook did not stop the call: {other:?}"),
    }

    // The file is the assertion that matters: a gate that reports a refusal
    // after the write landed is worse than no gate.
    assert!(
        !dir.path().join("notes.md").exists(),
        "the hook refused the call and the write happened anyway"
    );

    // And a tool the hook does not object to still runs.
    assert!(matches!(
        h.touch(&sid, "allowed").await?,
        Outcome::Result(_)
    ));
    Ok(())
}
