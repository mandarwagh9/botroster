//! `bot.send` as a tool the model itself calls, mid-run.
//!
//! Serving it from the hub rather than the client means it passes the same
//! approval gate as any other action. The load-bearing test is therefore not
//! "a handoff arrives" but that a denied handoff arrives nowhere, which holds
//! only because the check is somewhere the caller cannot skip.

use std::sync::Arc;
use std::time::Duration;

use botroster_agent::providers::Scripted;
use botroster_agent::{Agent, AgentConfig, AllowAll, ApprovalHandler, DenyAll, HubClient};
use botroster_bots::BotStore;
use botroster_proto::frames::ToolDescription;
use botrosterd::bot_tools::BotTools;
use botrosterd::policy::Policy;
use serde_json::json;
use tokio::sync::mpsc;

struct Rig {
    url: String,
    tools: Vec<ToolDescription>,
    bots: Arc<BotStore>,
    _home: tempfile::TempDir,
    _ws: tempfile::TempDir,
}

async fn rig(policy: Policy) -> anyhow::Result<Rig> {
    let home = tempfile::tempdir()?;
    let bots = Arc::new(BotStore::open(home.path())?);

    let hub = Arc::new(
        botrosterd::hub::Hub::with_policy(policy)
            .with_internal_tools(Arc::new(BotTools::new(Arc::clone(&bots)))),
    );
    let (listener, addr) = botrosterd::server::Server::bind("127.0.0.1:0").await?;
    let server = Arc::new(botrosterd::server::Server::new(hub));
    tokio::spawn(Arc::clone(&server).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let ws_dir = tempfile::tempdir()?;
    let ctx = Arc::new(botroster_guest::Context::new(
        botroster_guest::Workspace::new(ws_dir.path(), true)?,
        ws_dir.path().join(".browser-profile"),
    ));
    let cfg = botroster_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "botroster-workspace".into(),
        description: "t".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = botroster_guest::run(cfg, ctx).await;
    });

    let (probe, _p) = HubClient::connect(&url).await?;
    probe.open_session().await?;
    let mut tools = Vec::new();
    for _ in 0..100 {
        if !probe.list_servers().await?.is_empty() {
            tools = probe.bind_server("botroster-workspace").await?;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::ensure!(!tools.is_empty(), "guest never registered");

    Ok(Rig {
        url,
        tools,
        bots,
        _home: home,
        _ws: ws_dir,
    })
}

async fn run_as(
    rig: &Rig,
    as_bot: &str,
    task: &str,
    model: Arc<Scripted>,
    approver: Arc<dyn ApprovalHandler>,
) -> anyhow::Result<botroster_agent::AgentOutcome> {
    let (hub, progress) = HubClient::connect_with(&rig.url, approver).await?;
    hub.open_session_as(Some(as_bot)).await?;
    let tools = hub.bind_server("botroster-workspace").await?;

    let agent = Agent::new(
        model,
        hub,
        AgentConfig {
            system: "test".into(),
            max_steps: 6,
            ..Default::default()
        },
    );
    let (ev_tx, _rx) = mpsc::unbounded_channel();
    Ok(agent.run(task, tools, progress, ev_tx).await)
}

#[tokio::test]
async fn the_hub_advertises_bot_tools_alongside_the_guests() -> anyhow::Result<()> {
    let rig = rig(Policy::allow_all()).await?;
    let ids: Vec<_> = rig
        .tools
        .iter()
        .map(|t| t.tool_id.as_str().to_owned())
        .collect();

    // Guest tools and hub tools arrive in one catalogue; the model should not
    // have to know which side of the wire a tool lives on.
    assert!(ids.contains(&"fs.write".to_string()));
    assert!(ids.contains(&"bot.send".to_string()));
    assert!(ids.contains(&"bot.list".to_string()));
    Ok(())
}

#[tokio::test]
async fn a_bot_hands_work_over_mid_run() -> anyhow::Result<()> {
    let rig = rig(Policy::allow_all()).await?;
    rig.bots.create("Researcher", "Sources", "")?;
    let writer = rig.bots.create("Writer", "Drafts", "")?;

    let model = Arc::new(
        Scripted::builder()
            .say_and_call(
                "Passing this to the Writer.",
                "bot.send",
                json!({ "to": "Writer", "message": "sources are in /workspace/refs" }),
            )
            .say("handed over")
            .build(),
    );
    let outcome = run_as(
        &rig,
        "researcher",
        "research then hand off",
        model,
        Arc::new(AllowAll),
    )
    .await?;
    assert!(outcome.succeeded(), "{:?}", outcome.reason);

    let waiting = rig.bots.inbox(&writer.id)?;
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].text.contains("/workspace/refs"));
    // Attribution comes from the session's identity, not from the model.
    assert_eq!(waiting[0].from.as_ref().unwrap().as_str(), "researcher");
    Ok(())
}

#[tokio::test]
async fn a_denied_handoff_delivers_nothing() -> anyhow::Result<()> {
    // The reason bot.send lives in the hub: a client-side implementation
    // could simply not ask.
    let rig = rig(Policy::default()).await?;
    rig.bots.create("Researcher", "", "")?;
    let writer = rig.bots.create("Writer", "", "")?;

    let model = Arc::new(
        Scripted::builder()
            .call("bot.send", json!({ "to": "Writer", "message": "do this" }))
            .say("done")
            .build(),
    );
    run_as(&rig, "researcher", "hand off", model, Arc::new(DenyAll)).await?;

    assert!(
        rig.bots.inbox(&writer.id)?.is_empty(),
        "a refused handoff still reached the recipient"
    );
    Ok(())
}

#[tokio::test]
async fn handing_off_to_someone_who_does_not_exist_fails_the_call() -> anyhow::Result<()> {
    let rig = rig(Policy::allow_all()).await?;
    rig.bots.create("Researcher", "", "")?;

    let model = Arc::new(
        Scripted::builder()
            .call("bot.send", json!({ "to": "Ghost", "message": "hello" }))
            // The model gets the error and can say so rather than believing
            // the work was passed on.
            .say("nobody by that name")
            .build(),
    );
    let outcome = run_as(&rig, "researcher", "hand off", model, Arc::new(AllowAll)).await?;
    assert!(outcome.succeeded());

    let failed = outcome.transcript.iter().flat_map(|m| &m.content).any(|c| {
        matches!(
            c,
            botroster_agent::Content::ToolResult { is_error: true, .. }
        )
    });
    assert!(failed, "a handoff into the void was reported as success");
    Ok(())
}

#[tokio::test]
async fn a_bot_cannot_hand_work_to_itself_through_the_tool() -> anyhow::Result<()> {
    let rig = rig(Policy::allow_all()).await?;
    let me = rig.bots.create("Researcher", "", "")?;

    let model = Arc::new(
        Scripted::builder()
            .call(
                "bot.send",
                json!({ "to": "Researcher", "message": "again" }),
            )
            .say("cannot")
            .build(),
    );
    run_as(&rig, "researcher", "loop", model, Arc::new(AllowAll)).await?;
    assert!(
        rig.bots.inbox(&me.id)?.is_empty(),
        "a bot queued work for itself, which is a loop"
    );
    Ok(())
}

#[tokio::test]
async fn bot_list_does_not_offer_the_caller_itself() -> anyhow::Result<()> {
    let rig = rig(Policy::allow_all()).await?;
    rig.bots.create("Researcher", "Sources", "")?;
    rig.bots.create("Writer", "Drafts", "")?;

    let model = Arc::new(
        Scripted::builder()
            .call("bot.list", json!({}))
            .say("saw the roster")
            .build(),
    );
    let outcome = run_as(
        &rig,
        "researcher",
        "who else is here",
        model,
        Arc::new(AllowAll),
    )
    .await?;

    let results: String = outcome
        .transcript
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            botroster_agent::Content::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        results.contains("writer"),
        "the roster was empty: {results}"
    );
    assert!(
        !results.contains("researcher"),
        "a Bot was offered itself as a handoff target: {results}"
    );
    Ok(())
}
