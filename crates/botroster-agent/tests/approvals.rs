//! The approval gate, end to end.
//!
//! The property under test is not "a prompt appears"; it is that the tool
//! never runs unless the hub was told yes. So every assertion here checks the
//! filesystem, not just the response: a gate that returns "denied" while the
//! write already happened is worse than no gate at all.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use botroster_agent::agent::AgentEvent;
use botroster_agent::providers::Scripted;
use botroster_agent::{Agent, AgentConfig, ApprovalHandler, HubClient};
use botroster_proto::approval::{ApprovalDecision, ApprovalRequestParams, Decision};
use botroster_proto::frames::ToolDescription;
use botrosterd::policy::{Action, Policy, Rule};
use serde_json::json;
use tokio::sync::mpsc;

// ── approvers ─────────────────────────────────────────────────────────

/// Records every card it was shown, and answers with a fixed decision.
struct Recorder {
    decision: Decision,
    seen: Mutex<Vec<ApprovalRequestParams>>,
}

impl Recorder {
    fn new(decision: Decision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            seen: Mutex::new(Vec::new()),
        })
    }
    fn cards(&self) -> Vec<ApprovalRequestParams> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for Recorder {
    async fn decide(&self, req: &ApprovalRequestParams) -> ApprovalDecision {
        self.seen.lock().unwrap().push(req.clone());
        ApprovalDecision {
            decision: self.decision,
            note: None,
        }
    }
}

/// Never answers. Stands in for a person who walked away.
struct Silent {
    asked: AtomicUsize,
}

#[async_trait::async_trait]
impl ApprovalHandler for Silent {
    async fn decide(&self, _req: &ApprovalRequestParams) -> ApprovalDecision {
        self.asked.fetch_add(1, Ordering::SeqCst);
        // Longer than any test is willing to wait; the hub must give up first.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        ApprovalDecision::allow_once()
    }
}

// ── rig ───────────────────────────────────────────────────────────────

struct Rig {
    hub: Arc<HubClient>,
    progress: Option<mpsc::UnboundedReceiver<botroster_proto::frames::ToolCallProgressFrame>>,
    tools: Vec<ToolDescription>,
    workspace: tempfile::TempDir,
}

async fn rig(policy: Policy, approver: Arc<dyn ApprovalHandler>) -> anyhow::Result<Rig> {
    rig_timeout(policy, approver, botrosterd::hub::DEFAULT_APPROVAL_TIMEOUT).await
}

async fn rig_timeout(
    policy: Policy,
    approver: Arc<dyn ApprovalHandler>,
    approval_timeout: Duration,
) -> anyhow::Result<Rig> {
    let hub_state =
        Arc::new(botrosterd::hub::Hub::with_policy(policy).with_approval_timeout(approval_timeout));
    let (listener, addr) = botrosterd::server::Server::bind("127.0.0.1:0").await?;
    let server = Arc::new(botrosterd::server::Server::new(hub_state));
    tokio::spawn(Arc::clone(&server).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let workspace = tempfile::tempdir()?;
    let ws = Arc::new(botroster_guest::Context::new(
        botroster_guest::Workspace::new(workspace.path(), true)?,
        workspace.path().join(".browser-profile"),
    ));
    let cfg = botroster_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "botroster-workspace".into(),
        description: "test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = botroster_guest::run(cfg, ws).await;
    });

    let (client, progress) = HubClient::connect_with(&url, approver).await?;
    client.open_session().await?;

    let mut tools = Vec::new();
    for _ in 0..100 {
        if !client.list_servers().await?.is_empty() {
            tools = client.bind_server("botroster-workspace").await?;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::ensure!(!tools.is_empty(), "guest never registered");

    Ok(Rig {
        hub: client,
        progress: Some(progress),
        tools,
        workspace,
    })
}

impl Rig {
    async fn run(
        &mut self,
        model: Arc<Scripted>,
        task: &str,
    ) -> (botroster_agent::AgentOutcome, Vec<AgentEvent>) {
        let agent = Agent::new(
            model,
            Arc::clone(&self.hub),
            AgentConfig {
                system: "test".into(),
                max_steps: 8,
                ..Default::default()
            },
        );
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let progress = self.progress.take().expect("rig consumed twice");
        let outcome = agent.run(task, self.tools.clone(), progress, ev_tx).await;
        let mut events = Vec::new();
        while let Ok(e) = ev_rx.try_recv() {
            events.push(e);
        }
        (outcome, events)
    }

    fn exists(&self, rel: &str) -> bool {
        self.workspace.path().join(rel).exists()
    }
}

fn write_then_report() -> Arc<Scripted> {
    Arc::new(
        Scripted::builder()
            .call("fs.write", json!({ "path": "gated.txt", "contents": "x" }))
            .say("finished")
            .build(),
    )
}

// ── tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_approved_call_runs_and_the_file_appears() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowOnce);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let (outcome, _) = rig.run(write_then_report(), "write a file").await;

    assert!(outcome.succeeded(), "{:?}", outcome.reason);
    assert!(rig.exists("gated.txt"), "the approved write did not happen");
    assert_eq!(approver.cards().len(), 1, "exactly one card expected");
    Ok(())
}

#[tokio::test]
async fn a_denied_call_never_touches_the_filesystem() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::Deny);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let (_outcome, events) = rig.run(write_then_report(), "write a file").await;

    // The tool did not run.
    assert!(
        !rig.exists("gated.txt"),
        "a denied write reached the filesystem"
    );
    assert_eq!(approver.cards().len(), 1);

    let failed = events.iter().any(|e| {
        matches!(e, AgentEvent::ToolCallFinished { ok: false, output, .. }
            if output.to_string().contains("denied"))
    });
    assert!(
        failed,
        "the denial was not surfaced to the session: {events:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_read_is_not_gated_but_a_write_is() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowOnce);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let model = Arc::new(
        Scripted::builder()
            .call("fs.list", json!({}))
            .call("fs.write", json!({ "path": "a.txt", "contents": "a" }))
            .say("done")
            .build(),
    );
    let (outcome, _) = rig.run(model, "look then write").await;
    assert!(outcome.succeeded(), "{:?}", outcome.reason);

    // Only the write should have produced a card.
    let cards = approver.cards();
    assert_eq!(cards.len(), 1, "expected only the write to be gated");
    assert_eq!(cards[0].tool_id.as_str(), "fs.write");
    Ok(())
}

#[tokio::test]
async fn the_card_shows_the_real_arguments_and_a_reason() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowOnce);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let model = Arc::new(
        Scripted::builder()
            .call("shell.exec", json!({ "command": "echo not-a-summary" }))
            .say("done")
            .build(),
    );
    rig.run(model, "run something").await;

    let card = approver.cards().into_iter().next().expect("a card");
    // A person cannot approve what they cannot see: the exact arguments must
    // be present, not a summary.
    assert_eq!(card.args["command"], "echo not-a-summary");
    assert_eq!(card.tool_id.as_str(), "shell.exec");
    assert!(
        card.reason.contains("shell command"),
        "unhelpful reason: {}",
        card.reason
    );
    assert!(card.timeout_secs > 0);
    Ok(())
}

#[tokio::test]
async fn allow_always_stops_asking_for_that_tool() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowAlways);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let model = Arc::new(
        Scripted::builder()
            .call("fs.write", json!({ "path": "one.txt", "contents": "1" }))
            .call("fs.write", json!({ "path": "two.txt", "contents": "2" }))
            .call("fs.write", json!({ "path": "three.txt", "contents": "3" }))
            .say("done")
            .build(),
    );
    let (outcome, _) = rig.run(model, "write three files").await;

    assert!(outcome.succeeded(), "{:?}", outcome.reason);
    assert!(rig.exists("one.txt") && rig.exists("two.txt") && rig.exists("three.txt"));
    assert_eq!(
        approver.cards().len(),
        1,
        "allow_always must lift the gate for later calls"
    );
    Ok(())
}

#[tokio::test]
async fn a_grant_does_not_leak_to_other_tools() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowAlways);
    let mut rig = rig(Policy::default(), approver.clone()).await?;

    let model = Arc::new(
        Scripted::builder()
            .call("fs.write", json!({ "path": "a.txt", "contents": "a" }))
            .call("shell.exec", json!({ "command": "echo hi" }))
            .say("done")
            .build(),
    );
    rig.run(model, "write then exec").await;

    let tools: Vec<_> = approver
        .cards()
        .iter()
        .map(|c| c.tool_id.as_str().to_owned())
        .collect();
    assert_eq!(tools, vec!["fs.write", "shell.exec"]);
    Ok(())
}

#[tokio::test]
async fn a_policy_deny_refuses_without_asking_anyone() -> anyhow::Result<()> {
    let approver = Recorder::new(Decision::AllowOnce);
    let policy = Policy {
        rules: vec![Rule::deny("shell.exec", "no shell on this account")],
        fallback: Action::Allow,
        grants: Default::default(),
    };
    let mut rig = rig(policy, approver.clone()).await?;

    let model = Arc::new(
        Scripted::builder()
            .call("shell.exec", json!({ "command": "echo nope" }))
            .say("done")
            .build(),
    );
    let (_outcome, events) = rig.run(model, "try the shell").await;

    // An outright deny is not a question. Nobody should have been interrupted.
    assert!(
        approver.cards().is_empty(),
        "a denied call must not prompt a person"
    );
    let refused = events.iter().any(|e| {
        matches!(e, AgentEvent::ToolCallFinished { ok: false, output, .. }
            if output.to_string().contains("no shell on this account"))
    });
    assert!(refused, "the policy reason was not surfaced: {events:?}");
    Ok(())
}

#[tokio::test]
async fn an_unanswered_approval_expires_closed() -> anyhow::Result<()> {
    // The hub's timeout is injectable precisely so this branch is reachable.
    // Wrapping the call in a test-side timeout would only prove the test gave
    // up first, leaving the hub's fail-closed path uncovered.
    let approver = Arc::new(Silent {
        asked: AtomicUsize::new(0),
    });
    let mut rig = rig_timeout(
        Policy::default(),
        approver.clone(),
        Duration::from_millis(300),
    )
    .await?;

    let (outcome, events) = rig.run(write_then_report(), "write a file").await;

    // The hub gave up and denied on its own; the run completed normally.
    assert!(
        !rig.exists("gated.txt"),
        "the write happened despite no approval"
    );
    assert_eq!(approver.asked.load(Ordering::SeqCst), 1);

    let expired = events.iter().any(|e| {
        matches!(e, AgentEvent::ToolCallFinished { ok: false, output, .. }
            if output.to_string().contains("in time"))
    });
    assert!(
        expired,
        "expiry was not surfaced as a denial: {events:?} (outcome {:?})",
        outcome.reason
    );
    Ok(())
}

#[tokio::test]
async fn losing_the_approver_mid_flight_denies_rather_than_hanging() -> anyhow::Result<()> {
    // A person closing their laptop mid-approval must not leave the call
    // pending until timeout, and certainly must not let it through.
    let approver = Arc::new(Silent {
        asked: AtomicUsize::new(0),
    });
    let mut rig = rig_timeout(
        Policy::default(),
        approver.clone(),
        Duration::from_millis(400),
    )
    .await?;

    let (_outcome, _events) = rig.run(write_then_report(), "write a file").await;
    assert!(!rig.exists("gated.txt"));
    Ok(())
}

#[tokio::test]
async fn with_no_approver_attached_everything_gated_is_denied() -> anyhow::Result<()> {
    // `connect` (rather than `connect_with`) attaches DenyAll. An unattended
    // process must not be able to approve on a person's behalf.
    let hub_state = Arc::new(botrosterd::hub::Hub::with_policy(Policy::default()));
    let (listener, addr) = botrosterd::server::Server::bind("127.0.0.1:0").await?;
    let server = Arc::new(botrosterd::server::Server::new(hub_state));
    tokio::spawn(Arc::clone(&server).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ws = Arc::new(botroster_guest::Context::new(
        botroster_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    tokio::spawn(async move {
        let _ = botroster_guest::run(
            botroster_guest::GuestConfig {
                hub_url: url.clone(),
                server_id: "botroster-workspace".into(),
                description: "t".into(),
                token: None,
            },
            ws,
        )
        .await;
    });

    let url = format!("ws://{addr}/v1/tools");
    let (client, progress) = HubClient::connect(&url).await?;
    client.open_session().await?;
    let mut tools = Vec::new();
    for _ in 0..100 {
        if !client.list_servers().await?.is_empty() {
            tools = client.bind_server("botroster-workspace").await?;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let agent = Agent::new(
        write_then_report(),
        client,
        AgentConfig {
            system: "test".into(),
            max_steps: 4,
            ..Default::default()
        },
    );
    let (ev_tx, _rx) = mpsc::unbounded_channel();
    agent.run("write a file", tools, progress, ev_tx).await;

    assert!(
        !dir.path().join("gated.txt").exists(),
        "an unattended run wrote a gated file"
    );
    Ok(())
}

/// An unattended run stops at the first gate instead of burning its budget.
///
/// A person answering "no" is a decision about one action, and the model
/// trying something else is reasonable (see the test above). Nobody being
/// there is an absence: it cannot change during the run, so every retry is
/// guaranteed to fail the same way. Left unhandled, a nightly `botroster routine
/// tick` on the default approve mode spends a full step budget every night
/// rediscovering that nobody is watching.
#[tokio::test]
async fn with_no_approver_the_run_stops_at_the_first_gate() -> anyhow::Result<()> {
    use botroster_agent::DenyAll;

    let mut rig = rig(Policy::default(), Arc::new(DenyAll)).await?;

    // Three attempts at a gated tool; a loop that treats denial as advice
    // would take all of them.
    let script = botroster_agent::providers::Scripted::builder()
        .call("fs.write", json!({ "path": "a.md", "contents": "one" }))
        .call("fs.write", json!({ "path": "b.md", "contents": "two" }))
        .call("fs.write", json!({ "path": "c.md", "contents": "three" }))
        .say("gave up")
        .build();

    let agent = Agent::new(
        Arc::new(script),
        Arc::clone(&rig.hub),
        AgentConfig {
            max_steps: 8,
            ..Default::default()
        },
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = agent
        .run(
            "write some notes",
            rig.tools.clone(),
            rig.progress.take().unwrap(),
            tx,
        )
        .await;

    match &outcome.reason {
        botroster_agent::FinishReason::NothingApproved { message, tool } => {
            assert!(!message.is_empty(), "no explanation for the operator");
            // The tool is carried so a client can give advice that fits what
            // was refused; an empty one would send every refusal down the
            // same branch.
            assert_eq!(tool, "fs.write", "the refused tool was not recorded");
        }
        other => panic!("expected the run to stop for want of an approver, got {other:?}"),
    }
    assert_eq!(outcome.steps, 1, "the agent kept asking with nobody there");

    // Nothing was written.
    for f in ["a.md", "b.md", "c.md"] {
        assert!(
            !rig.workspace.path().join(f).exists(),
            "{f} was written despite the denial"
        );
    }
    Ok(())
}
