//! The agent loop, end to end: scripted model, hub, real guest, real files.
//!
//! Nothing is mocked below the model. The hub is a real listener, the guest is
//! the real daemon, the tools touch a real temp workspace. The only thing
//! scripted is the model, because a live one is non-deterministic and CI must
//! not be.

use std::sync::Arc;
use std::time::Duration;

use botroster_agent::agent::AgentEvent;
use botroster_agent::model::{Content, Role};
use botroster_agent::providers::Scripted;
use botroster_agent::{Agent, AgentConfig, AllowAll, ApprovalHandler, FinishReason, HubClient};
use botroster_proto::frames::ToolDescription;
use botrosterd::policy::Policy;
use serde_json::json;
use tokio::sync::mpsc;

struct Rig {
    hub: Arc<HubClient>,
    progress: Option<mpsc::UnboundedReceiver<botroster_proto::frames::ToolCallProgressFrame>>,
    tools: Vec<ToolDescription>,
    workspace: tempfile::TempDir,
}

/// Boot a hub, a guest, and a connected harness with a bound session.
///
/// Defaults to an open policy so loop tests exercise the loop; the approval
/// suite passes its own.
async fn rig() -> anyhow::Result<Rig> {
    rig_with(Policy::allow_all(), Arc::new(AllowAll)).await
}

async fn rig_with(policy: Policy, approver: Arc<dyn ApprovalHandler>) -> anyhow::Result<Rig> {
    let hub_state = Arc::new(botrosterd::hub::Hub::with_policy(policy));
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

    // Wait for the guest to register rather than sleeping blind.
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
        max_steps: u32,
    ) -> (botroster_agent::AgentOutcome, Vec<AgentEvent>) {
        let agent = Agent::new(
            model,
            Arc::clone(&self.hub),
            AgentConfig {
                system: "test".into(),
                max_steps,
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

    /// Any model, with a stop button, for the ones that are not `Scripted`,
    /// such as a model that never answers at all.
    async fn run_model(
        &mut self,
        model: Arc<dyn botroster_agent::model::Model>,
        task: &str,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> botroster_agent::AgentOutcome {
        let agent = Agent::new(
            model,
            Arc::clone(&self.hub),
            AgentConfig {
                system: "test".into(),
                max_steps: 10,
                ..Default::default()
            },
        )
        .with_cancel(cancel);
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        let progress = self.progress.take().expect("rig consumed twice");
        agent.run(task, self.tools.clone(), progress, ev_tx).await
    }

    /// A Scripted model, with a stop button.
    async fn run_cancellable(
        &mut self,
        model: Arc<Scripted>,
        task: &str,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> botroster_agent::AgentOutcome {
        let agent = Agent::new(
            model,
            Arc::clone(&self.hub),
            AgentConfig {
                system: "test".into(),
                max_steps: 10,
                ..Default::default()
            },
        )
        .with_cancel(cancel);
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        let progress = self.progress.take().expect("rig consumed twice");
        agent.run(task, self.tools.clone(), progress, ev_tx).await
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace.path().join(rel)).ok()
    }
}

#[tokio::test]
async fn the_agent_does_real_work_through_the_hub() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    let model = Arc::new(
        Scripted::builder()
            .say_and_call(
                "I'll save the note first.",
                "fs.write",
                json!({ "path": "notes/plan.md", "contents": "# Plan\n- ship P1\n" }),
            )
            .call("fs.read", json!({ "path": "notes/plan.md" }))
            .say("Saved and verified the plan.")
            .build(),
    );

    let (outcome, events) = rig
        .run(Arc::clone(&model), "Write a plan and check it", 10)
        .await;

    assert!(
        outcome.succeeded(),
        "expected completion, got {:?}",
        outcome.reason
    );
    assert_eq!(outcome.steps, 3);
    assert_eq!(outcome.text, "Saved and verified the plan.");

    // The file must exist on disk.
    assert_eq!(
        rig.read("notes/plan.md").as_deref(),
        Some("# Plan\n- ship P1\n")
    );

    // The model must have been shown the tool result, not just told it ran.
    let last = model.requests().last().cloned().expect("a third request");
    let results: Vec<_> = last
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        results.iter().any(|r| r.contains("# Plan")),
        "the read result never reached the model: {results:?}"
    );

    // Events a UI needs, in order.
    assert!(matches!(events.first(), Some(AgentEvent::Started { .. })));
    assert!(matches!(events.last(), Some(AgentEvent::Finished { .. })));
    let started = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallStarted { .. }))
        .count();
    let finished = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .count();
    assert_eq!((started, finished), (2, 2));
    Ok(())
}

#[tokio::test]
async fn tool_schemas_reach_the_model() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(Scripted::builder().say("nothing to do").build());
    let (_out, _ev) = rig.run(Arc::clone(&model), "idle", 4).await;

    let req = model.requests().first().cloned().expect("one request");
    let names: Vec<_> = req
        .tools
        .iter()
        .map(|t| t.tool_id.as_str().to_owned())
        .collect();
    for t in ["fs.read", "fs.write", "fs.list", "shell.exec"] {
        assert!(names.contains(&t.to_string()), "{t} missing from {names:?}");
    }
    // A schema with no properties is useless to a model.
    let write = req
        .tools
        .iter()
        .find(|t| t.tool_id.as_str() == "fs.write")
        .unwrap();
    assert!(write.input_schema["properties"]["contents"].is_object());
    Ok(())
}

#[tokio::test]
async fn a_failing_tool_is_fed_back_and_the_agent_recovers() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    let model = Arc::new(
        Scripted::builder()
            // Escapes the workspace: the guest refuses it.
            .call("fs.write", json!({ "path": "../pwned", "contents": "x" }))
            // The model should read the error and try somewhere legal.
            .call(
                "fs.write",
                json!({ "path": "ok.txt", "contents": "recovered" }),
            )
            .say("Recovered after the refused write.")
            .build(),
    );

    let (outcome, events) = rig.run(Arc::clone(&model), "write a file", 10).await;

    assert!(
        outcome.succeeded(),
        "a refused tool must not kill the run: {:?}",
        outcome.reason
    );
    assert_eq!(rig.read("ok.txt").as_deref(), Some("recovered"));

    // The failure must be visible in the event stream...
    let failed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallFinished { ok, .. } => Some(*ok),
            _ => None,
        })
        .collect();
    assert_eq!(failed, vec![false, true]);

    // ...and marked as an error to the model, so it knows to change course.
    let second = &model.requests()[1];
    let errored = second
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|c| matches!(c, Content::ToolResult { is_error: true, .. }));
    assert!(
        errored,
        "the tool failure was not marked is_error for the model"
    );
    Ok(())
}

#[tokio::test]
async fn a_runaway_loop_is_stopped_by_the_step_budget() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    // A model that never stops asking for tools.
    let mut b = Scripted::builder();
    for _ in 0..50 {
        b = b.call("fs.list", json!({}));
    }
    let model = Arc::new(b.build());

    let (outcome, _) = rig.run(Arc::clone(&model), "loop forever", 5).await;
    assert_eq!(outcome.reason, FinishReason::StepLimit { max_steps: 5 });
    assert_eq!(outcome.steps, 5);
    assert_eq!(
        model.turns_taken(),
        5,
        "the budget must bound model calls, not just iterations"
    );
    Ok(())
}

#[tokio::test]
async fn a_provider_claiming_tool_use_with_no_tools_fails_loudly() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(Scripted::builder().claims_tool_use_but_sends_none().build());

    let (outcome, _) = rig.run(model, "do something", 5).await;
    // Silently treating this as completion would drop the task on the floor.
    match outcome.reason {
        FinishReason::ModelFailed { message, .. } => {
            assert!(
                message.contains("no tool_use block"),
                "unhelpful message: {message}"
            )
        }
        other => panic!("expected ModelFailed, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn a_truncated_turn_is_not_reported_as_success() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(Scripted::builder().truncated("I was cut off mid-").build());

    let (outcome, _) = rig.run(model, "long answer", 5).await;
    assert_eq!(outcome.reason, FinishReason::Truncated);
    assert!(!outcome.succeeded());
    Ok(())
}

#[tokio::test]
async fn parallel_tool_calls_in_one_turn_all_execute() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    let model = Arc::new(
        Scripted::builder()
            .calls(&[
                ("fs.write", json!({ "path": "a.txt", "contents": "A" })),
                ("fs.write", json!({ "path": "b.txt", "contents": "B" })),
                ("fs.write", json!({ "path": "c.txt", "contents": "C" })),
            ])
            .say("all three written")
            .build(),
    );

    let (outcome, events) = rig.run(Arc::clone(&model), "write three files", 6).await;
    assert!(outcome.succeeded());
    assert_eq!(rig.read("a.txt").as_deref(), Some("A"));
    assert_eq!(rig.read("b.txt").as_deref(), Some("B"));
    assert_eq!(rig.read("c.txt").as_deref(), Some("C"));

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallFinished { ok: true, .. }))
            .count(),
        3
    );

    // Each result must be correlated back to its own tool_use id, or the model
    // cannot tell which write succeeded.
    let second = &model.requests()[1];
    let result_ids: Vec<_> = second
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult { id, .. } => Some(id.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(result_ids.len(), 3);
    let mut uniq = result_ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "tool result ids collided: {result_ids:?}");
    Ok(())
}

#[tokio::test]
async fn shell_progress_reaches_the_event_stream() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(
        Scripted::builder()
            .call("shell.exec", json!({ "command": "echo streamed" }))
            .say("done")
            .build(),
    );

    let (outcome, events) = rig.run(model, "run a command", 6).await;
    assert!(outcome.succeeded());

    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolProgress { payload, .. } => {
                Some(payload["stage"].as_str().unwrap_or(""))
            }
            _ => None,
        })
        .collect();
    assert!(
        progress.contains(&"starting") && progress.contains(&"finished"),
        "progress did not reach the agent event stream: {progress:?}"
    );
    Ok(())
}

#[tokio::test]
async fn history_accumulates_across_steps() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(
        Scripted::builder()
            .call("fs.list", json!({}))
            .call("fs.list", json!({}))
            .say("done")
            .build(),
    );
    let (outcome, _) = rig.run(Arc::clone(&model), "look twice", 6).await;
    assert!(outcome.succeeded());

    let reqs = model.requests();
    assert_eq!(reqs.len(), 3);
    // Each turn must see strictly more history than the last.
    assert!(reqs[0].messages.len() < reqs[1].messages.len());
    assert!(reqs[1].messages.len() < reqs[2].messages.len());
    // And the first message is always the user's task.
    assert_eq!(reqs[2].messages[0].role, Role::User);
    assert_eq!(reqs[2].messages[0].text(), "look twice");
    Ok(())
}

#[tokio::test]
async fn a_provider_having_a_bad_minute_is_marked_worth_repeating() -> anyhow::Result<()> {
    // A 429 on turn three of an eight-turn task ends the run: the work did
    // not happen, and it is nobody's fault. Whether that costs a routine its
    // whole day depends on this flag, and it is set here because this is the
    // last place the typed error exists: by the time a routine reads the
    // reason it is a string, and searching a string for "429" is unreliable.
    let mut rig = rig().await?;
    let model = Arc::new(
        Scripted::builder()
            .call(
                "fs.write",
                json!({ "path": "notes.md", "contents": "part one" }),
            )
            .fails(botroster_agent::ModelError::Overloaded(
                "HTTP 429: slow down".into(),
            ))
            .build(),
    );

    let (outcome, _) = rig.run(model, "write the notes", 5).await;
    match outcome.reason {
        FinishReason::ModelFailed { transient, message } => {
            assert!(
                transient,
                "a rate limit was treated as permanent: {message}"
            );
        }
        other => panic!("expected ModelFailed, got {other:?}"),
    }
    // The work it did get through is still in the transcript, so the run
    // that follows starts from there rather than from nothing.
    assert!(
        outcome.transcript.len() > 1,
        "the partial conversation was thrown away"
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_key_is_not_worth_repeating() -> anyhow::Result<()> {
    // The counterpart: retrying this every ten minutes spends money to be
    // told the same thing, and buries the real problem under a log of
    // identical failures.
    let mut rig = rig().await?;
    let model = Arc::new(
        Scripted::builder()
            .fails(botroster_agent::ModelError::Rejected(
                "HTTP 401: incorrect api key".into(),
            ))
            .build(),
    );

    let (outcome, _) = rig.run(model, "write the notes", 5).await;
    match outcome.reason {
        FinishReason::ModelFailed { transient, message } => {
            assert!(!transient, "a bad key was treated as an outage: {message}");
        }
        other => panic!("expected ModelFailed, got {other:?}"),
    }
    Ok(())
}

/// A stop button that stops nothing is worse than no stop button: the person
/// believes the work has ended, and it has not.
#[tokio::test]
async fn a_cancelled_run_touches_nothing() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    // This script would write a file. Cancelled before it starts, it must not.
    let model = Arc::new(
        Scripted::builder()
            .say_and_call(
                "Saving that now.",
                "fs.write",
                json!({ "path": "should-not-exist.md", "contents": "written anyway" }),
            )
            .say("Saved.")
            .build(),
    );

    let (_stop, stopped) = tokio::sync::watch::channel(true);
    let outcome = rig.run_cancellable(model, "write the file", stopped).await;

    assert!(
        matches!(outcome.reason, FinishReason::Cancelled { .. }),
        "a cancelled run reported {:?}, so a client is told the wrong thing about why it ended",
        outcome.reason
    );
    assert_eq!(
        outcome.steps, 0,
        "the loop took a step after being asked to stop"
    );
    // Catches a cancellation check placed after the work instead of before
    // it: the file is the observable consequence of not stopping.
    assert_eq!(
        rig.read("should-not-exist.md"),
        None,
        "the run was cancelled and wrote the file anyway"
    );
    Ok(())
}

/// Cancelling is not failing. A routine that retries a cancellation fights the
/// person who cancelled it.
#[tokio::test]
async fn a_cancelled_run_is_not_reported_as_a_success_either() -> anyhow::Result<()> {
    let mut rig = rig().await?;
    let model = Arc::new(Scripted::builder().say("all done").build());
    let (_stop, stopped) = tokio::sync::watch::channel(true);
    let outcome = rig.run_cancellable(model, "anything", stopped).await;
    assert!(
        !outcome.succeeded(),
        "a cancelled run claimed success; a routine would mark the work done"
    );
    Ok(())
}

/// A model that never answers, so the only way out is the stop button.
struct NeverAnswers;

#[async_trait::async_trait]
impl botroster_agent::model::Model for NeverAnswers {
    async fn turn(
        &self,
        _req: &botroster_agent::model::TurnRequest,
    ) -> Result<botroster_agent::model::TurnResponse, botroster_agent::model::ModelError> {
        // Far longer than the test's patience. Reaching the end of this sleep
        // means the run was not interrupted.
        tokio::time::sleep(Duration::from_secs(600)).await;
        unreachable!("the model turn should have been abandoned")
    }
    fn name(&self) -> &str {
        "never-answers"
    }
}

/// Cancelling during a model turn, which is where a person actually presses
/// stop: the turn is the slow part, and a stop button that waits for it is one
/// people press twice and then stop trusting.
#[tokio::test]
async fn cancelling_mid_turn_does_not_wait_for_the_model() -> anyhow::Result<()> {
    let mut rig = rig().await?;

    let (stop, stopped) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        // Long enough that the run is genuinely inside `model.turn`, short
        // enough that the test is quick.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = stop.send(true);
    });

    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        rig.run_model(Arc::new(NeverAnswers), "wait forever", stopped),
    )
    .await
    .expect("the run ignored the stop button and sat on the model call");

    assert!(
        matches!(outcome.reason, FinishReason::Cancelled { .. }),
        "interrupted mid-turn but reported {:?}",
        outcome.reason
    );
    Ok(())
}

/// Every `tool_use` must be answered by a `tool_result` in the same
/// transcript.
///
/// A vendor rejects a request whose tool calls are unanswered, so a
/// transcript that breaks this does not break the run that produced it; it
/// breaks the next run on that Bot, on another day, with a 400 nobody would
/// trace back to a cancellation.
fn unanswered_calls(transcript: &[botroster_agent::model::Message]) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    for m in transcript {
        for c in &m.content {
            match c {
                Content::ToolUse { id, .. } => open.push(id.as_str().to_owned()),
                Content::ToolResult { id, .. } => open.retain(|o| o != id.as_str()),
                Content::Text { .. } => {}
            }
        }
    }
    open
}

/// Presses stop the moment it is asked to approve something.
struct StopsWhenAsked {
    stop: tokio::sync::watch::Sender<bool>,
}

#[async_trait::async_trait]
impl ApprovalHandler for StopsWhenAsked {
    async fn decide(
        &self,
        _req: &botroster_proto::approval::ApprovalRequestParams,
    ) -> botroster_proto::approval::ApprovalDecision {
        let _ = self.stop.send(true);
        botroster_proto::approval::ApprovalDecision::allow_once()
    }
}

/// Cancelling with a tool call already in flight: the ordinary case of
/// someone pressing stop while an approval is on screen, and the one moment
/// the conversation could be left with a question nobody answered.
#[tokio::test]
async fn a_run_cancelled_around_a_tool_call_leaves_a_usable_conversation() -> anyhow::Result<()> {
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut rig = rig_with(Policy::default(), Arc::new(StopsWhenAsked { stop })).await?;

    let model = Arc::new(
        Scripted::builder()
            .say_and_call(
                "Writing that now.",
                "fs.write",
                json!({ "path": "notes/cancel.md", "contents": "half a thought" }),
            )
            .say("and here is the rest")
            .build(),
    );

    let outcome = rig.run_cancellable(model, "write it", stopped).await;

    assert!(
        matches!(outcome.reason, FinishReason::Cancelled { .. }),
        "expected a cancellation, got {:?}",
        outcome.reason
    );
    // Without this the test is vacuous: a cancellation that fired before any
    // tool call leaves nothing unanswered and passes for the wrong reason.
    let calls = outcome
        .transcript
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|c| matches!(c, Content::ToolUse { .. }))
        .count();
    assert_eq!(
        calls, 1,
        "the run was cancelled before it made the tool call this test is about"
    );

    let open = unanswered_calls(&outcome.transcript);
    assert!(
        open.is_empty(),
        "the cancelled turn left {} tool call(s) unanswered: {open:?}; this Bot's next \
         conversation would be rejected by the provider",
        open.len()
    );
    Ok(())
}

/// Sets the stop flag while producing its turn, so the run is already
/// cancelled at the instant its tool call is about to be executed.
///
/// That is the exact window in which a transcript can be left broken: the
/// assistant message carrying the `tool_use` is already in the conversation,
/// and the matching `tool_result` has not been written yet.
struct CancelsWhileThinking {
    stop: tokio::sync::watch::Sender<bool>,
    inner: Arc<Scripted>,
}

#[async_trait::async_trait]
impl botroster_agent::model::Model for CancelsWhileThinking {
    async fn turn(
        &self,
        req: &botroster_agent::model::TurnRequest,
    ) -> Result<botroster_agent::model::TurnResponse, botroster_agent::model::ModelError> {
        let out = self.inner.turn(req).await;
        let _ = self.stop.send(true);
        out
    }
    fn name(&self) -> &str {
        "cancels-while-thinking"
    }
}

/// Guards the invariant against a tempting change.
///
/// "Check cancellation more often" sounds like an improvement, but a check
/// placed between the tool call and its result would stop the run holding a
/// question nobody ever answers. The next prompt to that Bot would then be
/// rejected by the provider, days later, for a reason nothing points back to.
#[tokio::test]
async fn stopping_never_leaves_a_tool_call_hanging() -> anyhow::Result<()> {
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut rig = rig().await?;

    let scripted = Arc::new(
        Scripted::builder()
            .say_and_call(
                "One moment.",
                "fs.write",
                json!({ "path": "notes/hanging.md", "contents": "…" }),
            )
            .say("done")
            .build(),
    );
    let model = Arc::new(CancelsWhileThinking {
        stop,
        inner: scripted,
    });

    let outcome = rig.run_model(model, "write it", stopped).await;

    // The call must have been made, otherwise there is nothing to hang.
    let calls = outcome
        .transcript
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|c| matches!(c, Content::ToolUse { .. }))
        .count();
    assert_eq!(
        calls, 1,
        "no tool call was made, so the invariant was not exercised"
    );

    let open = unanswered_calls(&outcome.transcript);
    assert!(
        open.is_empty(),
        "stopping left {open:?} unanswered: the conversation now ends on a question, and the \
         provider will reject this Bot's next turn"
    );
    Ok(())
}

// ---- redirects ----

/// Run a turn that somebody interrupts, and report what the transcript holds.
impl Rig {
    async fn run_interrupted(
        &mut self,
        model: Arc<Scripted>,
        task: &str,
        redirects: botroster_agent::agent::Redirects,
    ) -> (botroster_agent::AgentOutcome, Vec<AgentEvent>) {
        let agent = Agent::new(
            model,
            Arc::clone(&self.hub),
            AgentConfig {
                system: "test".into(),
                max_steps: 6,
                ..Default::default()
            },
        )
        .with_redirects(redirects);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let progress = self.progress.take().expect("rig consumed twice");
        let outcome = agent.run(task, self.tools.clone(), progress, ev_tx).await;
        let mut events = Vec::new();
        while let Ok(e) = ev_rx.try_recv() {
            events.push(e);
        }
        (outcome, events)
    }
}

/// A direct message can redirect the current turn. botroster runs one turn at a
/// time, and the answer is not to remove that lock (two turns answering one
/// conversation is exactly what it exists to prevent) but to let a running
/// turn be told something.
#[tokio::test(flavor = "multi_thread")]
async fn something_said_mid_turn_joins_the_conversation() {
    let mut rig = rig().await.expect("rig");
    let redirects = botroster_agent::agent::Redirects::new();
    redirects.send("actually, check the invoices first");

    let model = Scripted::builder().say("Noted.").build();
    let (outcome, events) = rig
        .run_interrupted(Arc::new(model), "review the account", redirects)
        .await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Redirected { text } if text.contains("invoices")
        )),
        "the interruption is not in the transcript, so the Bot's next answer would arrive with no visible reason for changing direction: {events:?}"
    );

    // It joins the conversation as the person speaking, beside the original
    // task rather than replacing it: "check the invoices first" only means
    // anything next to what was being done.
    let said: Vec<String> = outcome
        .transcript
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| {
            m.content.iter().find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect();
    assert!(
        said.iter().any(|t| t.contains("review the account")),
        "the original task left the conversation: {said:?}"
    );
    assert!(
        said.iter().any(|t| t.contains("invoices")),
        "the interruption never reached the conversation: {said:?}"
    );
}

/// Blank interruptions are not interruptions. An empty user message in the
/// conversation is a turn the model has to interpret and a row in the
/// transcript nobody wrote.
#[tokio::test(flavor = "multi_thread")]
async fn a_blank_interruption_changes_nothing() {
    let mut rig = rig().await.expect("rig");
    let redirects = botroster_agent::agent::Redirects::new();
    redirects.send("   ");
    let model = Scripted::builder().say("Noted.").build();
    let (outcome, events) = rig
        .run_interrupted(Arc::new(model), "review the account", redirects)
        .await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Redirected { .. })),
        "a blank interruption was announced: {events:?}"
    );
    let users = outcome
        .transcript
        .iter()
        .filter(|m| m.role == Role::User)
        .count();
    assert_eq!(users, 1, "an empty message joined the conversation");
}

/// Stop outranks "instead, do this". Both are things a person does to a
/// running turn, and if they arrive together the turn must end rather than
/// take one more instruction: the person who pressed stop is not asking for
/// more work.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_beats_redirecting() {
    let mut rig = rig().await.expect("rig");
    let redirects = botroster_agent::agent::Redirects::new();
    redirects.send("actually, check the invoices first");

    let (stop, stopped) = tokio::sync::watch::channel(false);
    stop.send(true).expect("stop");

    let agent = Agent::new(
        Arc::new(Scripted::builder().say("Noted.").build())
            as Arc<dyn botroster_agent::model::Model>,
        Arc::clone(&rig.hub),
        AgentConfig {
            system: "test".into(),
            max_steps: 6,
            ..Default::default()
        },
    )
    .with_redirects(redirects)
    .with_cancel(stopped);
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let progress = rig.progress.take().expect("rig consumed twice");
    let outcome = agent
        .run("review the account", rig.tools.clone(), progress, ev_tx)
        .await;

    assert!(
        matches!(outcome.reason, FinishReason::Cancelled { .. }),
        "a stopped turn took another instruction instead of stopping: {:?}",
        outcome.reason
    );
    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(e);
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Redirected { .. })),
        "the interruption was delivered to a turn that was already stopping: {events:?}"
    );
}

/// Two tool calls in one turn that cannot be told apart end the turn.
///
/// A result is paired back to its call by id: by the provider on the next
/// turn, and by this loop, which builds every `ToolCallId` as `<id>-<step>`.
/// Two uses sharing an id produce two results carrying that id and two calls
/// carrying one `ToolCallId`, so the model can be handed the wrong answer to
/// the wrong question, and the transcript shows one call where there were two.
///
/// In practice it reaches the loop as an empty id rather than a repeated one:
/// both wire dialects read the id with `unwrap_or_default`, so a block without
/// one becomes `""`, and a turn asking for two tools gets `""` twice. Anthropic
/// and OpenAI always send ids; a gateway or local server answering in their
/// shape (the `--base-url` case) may not.
#[tokio::test]
async fn two_tool_calls_sharing_an_id_end_the_turn() -> anyhow::Result<()> {
    use botroster_agent::model::{StopReason, ToolUseId, TurnResponse, Usage};

    let mut rig = rig().await?;
    let call = |id: &str, path: &str| Content::ToolUse {
        id: ToolUseId::new(id),
        name: "fs.read".into(),
        input: json!({ "path": path }),
    };
    let model = Arc::new(Scripted::new(vec![TurnResponse {
        content: vec![call("", "a.txt"), call("", "b.txt")],
        stop_reason: StopReason::ToolUse,
        usage: Some(Usage::default()),
    }]));

    let (out, _ev) = rig.run(Arc::clone(&model), "read both", 4).await;
    match out.reason {
        FinishReason::ModelFailed { message, transient } => {
            assert!(
                message.contains("no id at all") && message.contains("fs.read"),
                "the failure does not say what was ambiguous: {message}"
            );
            assert!(
                !transient,
                "a provider that answers this way answers the same on a retry"
            );
        }
        other => panic!("expected the turn to end, got {other:?}"),
    }
    // Nothing ran: telling a Bot two things happened when the loop cannot say
    // which is which would be worse than stopping.
    assert_eq!(
        model.turns_taken(),
        1,
        "the loop went on asking after an answer it could not use"
    );
    Ok(())
}

/// A tool call with no name is refused by name, not mistaken for something.
///
/// Both wire dialects read the tool name with `unwrap_or_default`, so a call
/// arriving without one becomes `ToolUse { name: "" }`, the same leniency
/// behind the shared-id case above. Here it is harmless, and this test pins
/// that: the hub answers `unknown tool \`\``, which says the name was empty
/// instead of blaming the arguments or the workspace, and the model can act
/// on it.
///
/// The parser could refuse a nameless call outright, but the error already
/// produced is accurate and the run continues, so the behaviour is kept.
#[tokio::test]
async fn a_tool_call_with_no_name_is_refused_by_name() -> anyhow::Result<()> {
    use botroster_agent::model::{StopReason, ToolUseId, TurnResponse, Usage};

    let mut rig = rig().await?;
    let model = Arc::new(Scripted::new(vec![
        TurnResponse {
            content: vec![Content::ToolUse {
                id: ToolUseId::new("t1"),
                name: String::new(),
                input: json!({ "path": "a.txt" }),
            }],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
        },
        TurnResponse {
            content: vec![Content::Text {
                text: "I could not name the tool".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
        },
    ]));

    let (out, _ev) = rig.run(Arc::clone(&model), "do it", 4).await;
    assert!(
        matches!(out.reason, FinishReason::Completed),
        "the run should carry on after a refused call, not end: {:?}",
        out.reason
    );

    let second = model
        .requests()
        .get(1)
        .cloned()
        .expect("the loop asked again after the refusal");
    let told: Vec<String> = second
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| match c {
            Content::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(told.len(), 1, "expected one tool result, got {told:?}");
    assert!(
        told[0].contains("unknown tool"),
        "the model was told something other than that the tool was not known: {}",
        told[0]
    );
    Ok(())
}
