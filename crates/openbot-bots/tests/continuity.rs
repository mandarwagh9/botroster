//! Continuity: what makes a Bot a teammate instead of a chat box.
//!
//! The property is not "messages are on disk" but that the model receives
//! them on the next run, and that persisting is incremental so re-running
//! never duplicates what it was seeded with. Both are asserted against the
//! scripted provider's record of what it was sent.

use std::sync::Arc;
use std::time::Duration;

use openbot_agent::model::Content;
use openbot_agent::providers::Scripted;
use openbot_agent::{Agent, AgentConfig, AllowAll, HubClient};
use openbot_bots::{BotStore, MAX_BOTS};
use openbot_proto::frames::ToolDescription;
use serde_json::json;
use tokio::sync::mpsc;

struct Rig {
    url: String,
    tools: Vec<ToolDescription>,
    _ws: tempfile::TempDir,
}

async fn rig() -> anyhow::Result<Rig> {
    let hub = Arc::new(openbotd::hub::Hub::with_policy(
        openbotd::policy::Policy::allow_all(),
    ));
    let (listener, addr) = openbotd::server::Server::bind("127.0.0.1:0").await?;
    let server = Arc::new(openbotd::server::Server::new(hub));
    tokio::spawn(Arc::clone(&server).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let ws_dir = tempfile::tempdir()?;
    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(ws_dir.path(), true)?,
        ws_dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "openbot-workspace".into(),
        description: "t".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ctx).await;
    });

    let (probe, _p) = HubClient::connect(&url).await?;
    probe.open_session().await?;
    let mut tools = Vec::new();
    for _ in 0..100 {
        if !probe.list_servers().await?.is_empty() {
            tools = probe.bind_server("openbot-workspace").await?;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::ensure!(!tools.is_empty(), "guest never registered");
    Ok(Rig {
        url,
        tools,
        _ws: ws_dir,
    })
}

/// One run of a Bot, exactly as the CLI does it: seed from stored history,
/// run, then persist only what the run added.
async fn run_as_bot(
    rig: &Rig,
    bots: &BotStore,
    bot: &openbot_bots::Bot,
    task: &str,
    model: Arc<Scripted>,
) -> anyhow::Result<()> {
    let (hub, progress) = HubClient::connect_with(&rig.url, Arc::new(AllowAll)).await?;
    hub.open_session().await?;
    hub.bind_server("openbot-workspace").await?;

    let prior = bots.history(&bot.id, Some(40))?;
    let agent = Agent::new(
        model,
        hub,
        AgentConfig {
            system: bot.system_prompt("BASE"),
            max_steps: 6,
            ..Default::default()
        },
    )
    .with_history(prior);
    let started_from = agent.history_len();

    let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
    let outcome = agent.run(task, rig.tools.clone(), progress, ev_tx).await;

    let fresh = &outcome.transcript[started_from.min(outcome.transcript.len())..];
    bots.append(&bot.id, fresh)?;
    Ok(())
}

#[tokio::test]
async fn a_bot_carries_its_conversation_into_the_next_run() -> anyhow::Result<()> {
    let rig = rig().await?;
    let home = tempfile::tempdir()?;
    let bots = BotStore::open(home.path())?;
    let bot = bots.create("Piper", "Product performance", "Never change production.")?;

    // First run.
    let m1 = Arc::new(
        Scripted::builder()
            .say("Noted: the checkout p99 is 900ms.")
            .build(),
    );
    run_as_bot(&rig, &bots, &bot, "What is the checkout latency?", m1).await?;
    let after_first = bots.message_count(&bot.id)?;
    assert_eq!(after_first, 2, "expected the task and the reply");

    // Second run, a fresh process would look exactly like this.
    let m2 = Arc::new(Scripted::builder().say("Still 900ms.").build());
    run_as_bot(&rig, &bots, &bot, "And now?", Arc::clone(&m2)).await?;

    // The model must have received the earlier exchange, not merely had it
    // sitting on disk.
    let seen = m2.requests();
    let first = &seen[0];
    assert_eq!(
        first.messages.len(),
        3,
        "the second run did not carry the first run's history: {:?}",
        first.messages
    );
    assert_eq!(first.messages[0].text(), "What is the checkout latency?");
    assert_eq!(
        first.messages[1].text(),
        "Noted: the checkout p99 is 900ms."
    );
    assert_eq!(first.messages[2].text(), "And now?");

    // And the standing brief rode along in the system prompt.
    assert!(first.system.contains("Never change production."));
    assert!(first.system.contains("You are **Piper**"));

    assert_eq!(bots.message_count(&bot.id)?, 4);
    Ok(())
}

#[tokio::test]
async fn re_running_never_duplicates_the_history_it_was_seeded_with() -> anyhow::Result<()> {
    let rig = rig().await?;
    let home = tempfile::tempdir()?;
    let bots = BotStore::open(home.path())?;
    let bot = bots.create("Scout", "", "")?;

    // Persisting the whole transcript instead of the new tail would make this
    // grow quadratically: 2, 6, 14, 30…
    for i in 0..5 {
        let m = Arc::new(Scripted::builder().say(&format!("reply {i}")).build());
        run_as_bot(&rig, &bots, &bot, &format!("task {i}"), m).await?;
    }
    assert_eq!(
        bots.message_count(&bot.id)?,
        10,
        "history grew faster than the runs that produced it"
    );

    let h = bots.history(&bot.id, None)?;
    assert_eq!(h[0].text(), "task 0");
    assert_eq!(h[9].text(), "reply 4");
    Ok(())
}

#[tokio::test]
async fn tool_use_is_preserved_in_the_history_not_flattened_to_text() -> anyhow::Result<()> {
    let rig = rig().await?;
    let home = tempfile::tempdir()?;
    let bots = BotStore::open(home.path())?;
    let bot = bots.create("Writer", "", "")?;

    let m = Arc::new(
        Scripted::builder()
            .call("fs.write", json!({"path": "note.md", "contents": "hi"}))
            .say("saved")
            .build(),
    );
    run_as_bot(&rig, &bots, &bot, "save a note", m).await?;

    let h = bots.history(&bot.id, None)?;
    // A history that dropped tool calls would leave the model unable to see
    // what it already did, and it would redo the work next turn.
    let has_use = h
        .iter()
        .flat_map(|m| &m.content)
        .any(|c| matches!(c, Content::ToolUse { .. }));
    let has_result = h
        .iter()
        .flat_map(|m| &m.content)
        .any(|c| matches!(c, Content::ToolResult { .. }));
    assert!(has_use, "the tool call was lost from the history");
    assert!(has_result, "the tool result was lost from the history");
    Ok(())
}

#[tokio::test]
async fn two_bots_keep_separate_conversations_on_one_computer() -> anyhow::Result<()> {
    let rig = rig().await?;
    let home = tempfile::tempdir()?;
    let bots = BotStore::open(home.path())?;
    let a = bots.create("Sales", "Outbound", "Never send without approval.")?;
    let b = bots.create("Support", "Escalations", "Always cite the ticket.")?;

    let m = Arc::new(Scripted::builder().say("ok").build());
    run_as_bot(&rig, &bots, &a, "a secret only Sales knows", m).await?;

    let m2 = Arc::new(Scripted::builder().say("ok").build());
    run_as_bot(&rig, &bots, &b, "Support's own task", Arc::clone(&m2)).await?;

    // Conversations are per-Bot even though the computer is shared.
    let seen = m2.requests();
    let text: String = seen[0]
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !text.contains("only Sales knows"),
        "one Bot's conversation leaked into another: {text}"
    );
    assert!(seen[0].system.contains("Always cite the ticket."));
    assert!(!seen[0].system.contains("Never send without approval."));
    Ok(())
}

#[tokio::test]
async fn a_bot_that_is_forgotten_starts_clean_but_keeps_its_brief() -> anyhow::Result<()> {
    let rig = rig().await?;
    let home = tempfile::tempdir()?;
    let bots = BotStore::open(home.path())?;
    let bot = bots.create("Piper", "Perf", "Never change production.")?;

    let m = Arc::new(Scripted::builder().say("first").build());
    run_as_bot(&rig, &bots, &bot, "old context", m).await?;
    bots.clear_history(&bot.id)?;

    let m2 = Arc::new(Scripted::builder().say("second").build());
    run_as_bot(&rig, &bots, &bot, "fresh start", Arc::clone(&m2)).await?;

    let seen = m2.requests();
    assert_eq!(seen[0].messages.len(), 1, "forgotten history came back");
    // The brief is not history and must survive.
    assert!(seen[0].system.contains("Never change production."));
    Ok(())
}

#[test]
fn the_roster_limit_is_the_documented_one() {
    // Matches the observed product limit.
    assert_eq!(MAX_BOTS, 50);
}

/// Total bytes under a directory. The log's exact path is the store's
/// business, not this test's.
fn walk_size(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            total += if p.is_dir() {
                walk_size(&p)
            } else {
                std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
            };
        }
    }
    total
}

/// Loading recent history must not cost the whole history.
///
/// Every run replays a Bot's last messages. Parsing the entire log to do it
/// would make a Bot used nightly for a year pay for a year of messages on
/// every task, in time and in memory, to look at the last forty.
#[test]
fn reading_recent_history_does_not_read_the_whole_log() {
    let d = tempfile::tempdir().unwrap();
    let s = openbot_bots::BotStore::open(d.path()).unwrap();
    let b = s.create("Veteran", "", "").unwrap();

    // A year of nightly runs, roughly.
    let big = "x".repeat(2_000);
    for i in 0..4_000 {
        s.append(
            &b.id,
            &[openbot_agent::model::Message::user(format!(
                "task {i}: {big}"
            ))],
        )
        .unwrap();
    }

    let bytes = walk_size(d.path());
    assert!(bytes > 4_000_000, "the log should be large: {bytes}");

    let started = std::time::Instant::now();
    let recent = s.history(&b.id, Some(40)).unwrap();
    let took = started.elapsed();

    assert_eq!(recent.len(), 40);
    assert!(recent[39].text().contains("task 3999"), "wrong messages");
    // Reading the tail of an 8MB file should not take anything like as long as
    // parsing all of it. Generous so a loaded machine does not fail it, but far
    // below what a full parse costs.
    eprintln!("reading 40 of 4000 messages from an {bytes}-byte log took {took:?}");
    assert!(
        took < std::time::Duration::from_millis(150),
        "reading 40 messages took {took:?}: the whole log is being parsed"
    );
}

/// Listing Bots must not parse every message on the account.
///
/// `bot ls` shows a message count per Bot. Computing it by deserialising
/// every message, including every tool result, would make the most casual
/// command the most expensive one.
#[test]
fn counting_messages_does_not_deserialise_them() {
    let d = tempfile::tempdir().unwrap();
    let s = openbot_bots::BotStore::open(d.path()).unwrap();
    let big = "x".repeat(2_000);

    for n in 0..5 {
        let b = s.create(&format!("Bot {n}"), "", "").unwrap();
        for i in 0..1_000 {
            s.append(
                &b.id,
                &[openbot_agent::model::Message::user(format!("{i}: {big}"))],
            )
            .unwrap();
        }
    }

    let started = std::time::Instant::now();
    let mut total = 0;
    for b in s.list(true).unwrap() {
        total += s.message_count(&b.id).unwrap();
    }
    let took = started.elapsed();

    assert_eq!(total, 5_000);
    eprintln!("counting 5000 messages across 5 bots took {took:?}");
    assert!(
        took < std::time::Duration::from_millis(120),
        "counting took {took:?}: every message is being parsed"
    );
}

/// A webhook endpoint must not get slower every time it is used.
#[test]
fn checking_a_delivery_id_does_not_scan_every_past_delivery() {
    let d = tempfile::tempdir().unwrap();
    let s = openbot_bots::BotStore::open(d.path()).unwrap();

    for i in 0..30_000 {
        s.remember_event(&format!("delivery-{i}")).unwrap();
    }

    let started = std::time::Instant::now();
    // The common case: a fresh id, which is the one that has to scan.
    assert!(!s.event_seen("delivery-brand-new").unwrap());
    let took = started.elapsed();
    eprintln!("checking one id against 30000 deliveries took {took:?}");

    // Recent ids are still recognised, which is the point of remembering.
    assert!(s.event_seen("delivery-29999").unwrap());
    assert!(
        took < std::time::Duration::from_millis(60),
        "checking took {took:?}: the whole history is being read"
    );
}

/// A replayed window never opens on a tool result whose call it left behind.
///
/// A tool-using Bot's log is a repeating pair: an assistant message asking for
/// a tool, then a user message carrying the result. Taking the last N *lines*
/// of that cuts between the two whenever N lands wrong, and the window then
/// starts with a `tool_result` referring to a `tool_use` that is no longer in
/// the request. Both vendors reject that: Anthropic emits the block
/// unconditionally, and the OpenAI dialect emits a bare `role:"tool"` message.
///
/// The suite already understands this hazard from the other end —
/// `agent_loop.rs` says a transcript with unanswered tool calls "does not break
/// the run that produced it; it breaks the next run on that Bot, on another
/// day, with a 400 nobody would trace back". This is the mirror: an orphaned
/// *result* at the head of the window, which was unguarded.
///
/// Every window size is checked rather than one, because the bug is a parity
/// property — it appears only for the N that cut a pair, so a single N proves
/// nothing about the rest.
#[test]
fn a_replayed_window_never_starts_with_an_orphaned_tool_result() {
    let home = tempfile::tempdir().unwrap();
    let bots = BotStore::open(home.path()).unwrap();
    let bot = bots.create("Tessa", "", "").unwrap();

    // Twenty tool-using turns: ask, use, result. The shape a Bot that actually
    // does work leaves behind, not a chat log.
    for i in 0..20 {
        let id = openbot_agent::model::ToolUseId::new(format!("t{i}"));
        bots.append(
            &bot.id,
            &[
                openbot_agent::model::Message::user(format!("task {i}")),
                openbot_agent::model::Message::assistant(vec![Content::ToolUse {
                    id: id.clone(),
                    name: "fs.read".into(),
                    input: json!({ "path": "notes.md" }),
                }]),
                openbot_agent::model::Message {
                    role: openbot_agent::model::Role::User,
                    content: vec![Content::ToolResult {
                        id,
                        content: format!("contents {i}"),
                        is_error: false,
                    }],
                },
            ],
        )
        .unwrap();
    }

    let mut orphaned = Vec::new();
    for n in 1..=60 {
        let window = bots.history(&bot.id, Some(n)).unwrap();
        // Every id answered inside the window must have been asked inside it.
        let mut asked = std::collections::HashSet::new();
        for m in &window {
            for c in &m.content {
                match c {
                    Content::ToolUse { id, .. } => {
                        asked.insert(id.clone());
                    }
                    Content::ToolResult { id, .. } if !asked.contains(id) => {
                        orphaned.push((n, id.clone()));
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(
        orphaned.is_empty(),
        "{} of 60 window sizes replay a tool result whose call was cut off. Both vendors 400 \
         this, and it heals on the next run - so it is a routine losing a firing, recorded as \
         retryable: false, that nobody can reproduce. Sizes: {:?}",
        orphaned.len(),
        orphaned.iter().map(|(n, _)| *n).collect::<Vec<_>>()
    );
}
