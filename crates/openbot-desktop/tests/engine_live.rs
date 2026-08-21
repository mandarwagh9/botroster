//! The OPENBOT engine against the shipped binary, on a live stack.
//!
//! The engine is the layer the Tauri shell renders, so it is tested the way
//! the adapter it talks to is tested: as shipped, over stdio, with the whole
//! hub and guest underneath. `acp_sdk_live.rs` in openbot-cli proves the SDK
//! can drive the agent; this proves the engine's own API, the one the UI
//! calls, can drive it too.

mod common;

use openbot_desktop::engine::{Config, Engine, Who};
use std::time::Duration;

use common::up::Up;

/// The one scripted reply `--demo` gives, ready for a person to read.
const DEMO_REPLY: &str = "Done.";

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_opens_a_session_and_hears_a_whole_turn() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session("/tmp/openbot-project")
        .await
        .expect("session/new");
    assert!(
        session.0.starts_with("openbot-"),
        "a session id should say whose it is, got {session}"
    );

    let (stop, updates) =
        tokio::time::timeout(Duration::from_secs(90), engine.prompt(&session, "prove it"))
            .await
            .expect("the turn did not finish in time")
            .expect("the engine could not complete the turn");

    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the demo script completes, so anything else is a regression"
    );

    // Streaming is the point of a chat surface: the words must reach the
    // engine during the turn, not be assembled by the adapter and handed over
    // when it ends. The engine's own prompt drains the stream, and the
    // scripted reply must be in it.
    assert!(
        updates.iter().any(|u| carries(u, DEMO_REPLY)),
        "the scripted reply never reached the engine: {updates:?}"
    );
}

/// The session the engine opened is bound to a Bot on disk (the adapter's
/// contract, visible from the client side). The engine does not create Bots
/// itself; the agent does, for the directory the session was opened on.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_created_through_the_engine_lands_on_a_bot() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    engine
        .new_session("/tmp/ledger")
        .await
        .expect("session/new");

    let bots = openbot_bots::BotStore::open(agent_home.path()).unwrap();
    let names: Vec<String> = bots
        .list(true)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "ledger"),
        "expected a Bot named for the working directory, found {names:?}"
    );
}

/// `engine.cancel` must be wired to the wire: the SDK's CancelNotification is
/// what the adapter answers with `cancelled`. The demo turn is too fast to
/// race reliably, so this only asserts the plumbing reaches a live agent
/// without error; the turn itself is covered by the adapter's own tests.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_reaches_the_agent_without_error() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session("/tmp/ledger")
        .await
        .expect("session/new");

    // Nothing is running, so the cancel is a no-op at the agent, but the
    // notification must still round-trip cleanly (the agent answers it with
    // a stop of `cancelled` when a turn is running).
    engine.cancel(&session).await.expect("cancel");
}

/// `--demo-tools` plays the tool script instead of one reply, so the hub's
/// approval gate fires for real: `fs.write` and `shell.exec` go to the client
/// as `session/request_permission`, and the turn waits on the answer. This
/// proves the engine surfaces the ask and the answer lands, which is the
/// point of the desktop client being in the loop (SPEC §9).
#[tokio::test(flavor = "multi_thread")]
async fn approvals_reach_the_engine_and_answers_land() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: false,
        demo_tools: true,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session("/tmp/openbot-approvals")
        .await
        .expect("session/new");

    // Drive the turn the way the shell does: start it, then keep draining
    // the streams and answering dialogs until the stop reason arrives.
    let mut turn = engine
        .prompt_start(&session, "run the demo")
        .await
        .expect("the turn did not start");

    let mut asked = Vec::new();
    let mut updates = Vec::new();
    let stop = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            while let Some((sid, update)) = engine.next_update() {
                if sid == session {
                    updates.push(update);
                }
            }
            while let Some(mut pending) = engine.next_permission() {
                let options: Vec<String> = pending
                    .options()
                    .iter()
                    .map(|o| o.option_id.0.to_string())
                    .collect();
                assert!(
                    options.iter().any(|o| o == "allow-once"),
                    "the client is offered the standard options, got {options:?}"
                );
                asked.push(pending.tool_call().fields.title.clone().unwrap_or_default());
                // Asserted, not ignored: the turn is waiting on this one, so a
                // decision that does not reach it is a failure.
                assert!(
                    pending.answer(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new("allow-once"),
                    )),
                    "the approval this turn is blocked on refused the answer"
                );
            }
            match turn.try_recv() {
                Ok(reply) => return reply.expect("the turn failed"),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    panic!("openbot acp is gone");
                }
            }
        }
    })
    .await
    .expect("the turn did not finish in time");

    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the demo script completes when its tools are allowed"
    );

    // fs.write and shell.exec are the two tools the default policy asks the
    // person about; fs.read and fs.list are allowed without asking.
    assert_eq!(asked.len(), 2, "expected two approval asks, got {asked:?}");
    assert!(
        asked[0].starts_with("fs.write"),
        "the first ask should be fs.write, got {:?}",
        asked[0]
    );
    assert!(
        asked[1].starts_with("shell.exec"),
        "the second ask should be shell.exec, got {:?}",
        asked[1]
    );

    // The tool script ends with a plain say, which must have streamed like
    // the one-reply demo's did.
    assert!(
        updates.iter().any(|u| carries(u, "That is the loop")),
        "the demo's final reply never reached the engine: {updates:?}"
    );
}

/// A refused approval must not hang the client: the ask is answered
/// `Cancelled`, the tool call is denied by the hub, and the turn still ends.
/// The demo script plays through regardless; what is being proven is that
/// the engine fails closed without wedging the connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_approval_answers_cancelled_and_the_turn_still_ends() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: false,
        demo_tools: true,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session("/tmp/openbot-refusals")
        .await
        .expect("session/new");

    let mut turn = engine
        .prompt_start(&session, "run the demo")
        .await
        .expect("the turn did not start");

    let mut refused = 0;
    let stop = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            while let Some((_sid, _update)) = engine.next_update() {}
            while let Some(mut pending) = engine.next_permission() {
                refused += 1;
                assert!(
                    pending.answer(RequestPermissionOutcome::Cancelled),
                    "the approval this turn is blocked on refused the refusal"
                );
            }
            match turn.try_recv() {
                Ok(reply) => return reply.expect("the turn failed"),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    panic!("openbot acp is gone");
                }
            }
        }
    })
    .await
    .expect("the turn did not finish in time");

    assert_eq!(
        refused, 2,
        "both scripted tool calls should ask, got {refused}"
    );
    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the scripted demo finishes even when every tool is refused"
    );
}

/// openbot's premise is that a person talks to the same teammate tomorrow and
/// it remembers. A client that opens a folder and shows an empty transcript
/// makes that invisible: the Bot remembers, but the window never asks.
/// `session/load` is how it asks, and this drives it end to end: one real
/// turn against a live stack, a second engine that has never seen that
/// session, and the words of the first conversation arriving in the second.
#[tokio::test(flavor = "multi_thread")]
async fn a_reconnecting_client_is_shown_the_conversation_it_missed() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");
    let cwd = "/tmp/openbot-remembers";

    // One conversation, had and finished.
    {
        let mut engine = Engine::connect(Config {
            openbot: common::up::openbot(),
            home: agent_home.path().to_path_buf(),
            hub: up.hub.clone(),
            demo: true,
            demo_tools: false,
            demo_secret: false,
            bot: None,
            api_key: None,
        })
        .await
        .expect("the engine could not connect");
        let session = engine.new_session(cwd).await.expect("session/new");
        tokio::time::timeout(
            Duration::from_secs(90),
            engine.prompt(&session, "remember this"),
        )
        .await
        .expect("the turn did not finish in time")
        .expect("the turn failed");
    }

    // A different process entirely, as far as the agent is concerned.
    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not reconnect");

    // The id is the client's own; it was never handed out by this agent and
    // could not have been, since the first `openbot acp` is gone. The
    // conversation is resolved from the directory, which is the durable key.
    let session = SessionId::new("openbot-restored");
    engine
        .load_session(&session, cwd)
        .await
        .expect("session/load");

    let mut replayed = Vec::new();
    while let Some((sid, update)) = engine.next_update() {
        assert_eq!(
            sid, session,
            "a replayed update must name the session asked for"
        );
        replayed.push(update);
    }

    assert!(
        replayed.iter().any(|u| spoken(u, "remember this")),
        "what the person said last time is missing: {replayed:?}"
    );
    assert!(
        replayed.iter().any(|u| spoken(u, DEMO_REPLY)),
        "what the Bot said last time is missing: {replayed:?}"
    );

    // Order is preserved: the question precedes the answer.
    let asked = replayed
        .iter()
        .position(|u| spoken(u, "remember this"))
        .expect("the question");
    let answered = replayed
        .iter()
        .position(|u| spoken(u, DEMO_REPLY))
        .expect("the answer");
    assert!(
        asked < answered,
        "the reply was replayed before the message it answered"
    );
}

/// A folder nobody has worked in yet replays nothing, and that is not an
/// error.
#[tokio::test(flavor = "multi_thread")]
async fn loading_a_conversation_that_never_happened_is_not_an_error() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = SessionId::new("openbot-fresh");
    engine
        .load_session(&session, "/tmp/openbot-never-used")
        .await
        .expect("loading an empty conversation must succeed");
    assert!(engine.next_update().is_none(), "nothing to replay");

    // The session is usable afterwards; a loaded session is a session.
    let (stop, _said) = tokio::time::timeout(
        Duration::from_secs(90),
        engine.prompt(&session, "first task"),
    )
    .await
    .expect("the turn did not finish in time")
    .expect("a loaded session must accept a prompt");
    assert_eq!(stop, StopReason::EndTurn);
}

/// A roster, not a folder picker. A desktop client opens a Bot from a sidebar
/// by name, while ACP addresses a session by working directory, which is the
/// right shape for an editor and the wrong one here. `_meta` is the
/// protocol's own extension point, and this proves a client can name the
/// teammate it wants and reach the same one again later.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_can_open_a_bot_by_name_and_come_back_to_it() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");
    // Intentionally unrelated to the Bot's name: if the directory leaked into
    // the naming, this is where it would show.
    let cwd = "/tmp/some-unrelated-folder";

    let engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session_for(Some(Who::Bot("Talent Scout".into())), cwd)
        .await
        .expect("session/new for a named Bot");

    let bots = openbot_bots::BotStore::open(agent_home.path()).unwrap();
    let names: Vec<String> = bots
        .list(true)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "Talent Scout"),
        "the client asked for a Bot by name and got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("unrelated")),
        "the directory should not have named a Bot the client named itself: {names:?}"
    );

    let mut engine = engine;
    tokio::time::timeout(
        Duration::from_secs(90),
        engine.prompt(&session, "find me a candidate"),
    )
    .await
    .expect("the turn did not finish in time")
    .expect("the turn failed");

    // Later: same name, same teammate, conversation intact.
    let restored = SessionId::new("openbot-roster");
    engine
        .load_session_for(&restored, Some(Who::Bot("Talent Scout".into())), cwd)
        .await
        .expect("session/load for a named Bot");

    let mut replayed = Vec::new();
    while let Some((_sid, update)) = engine.next_update() {
        replayed.push(update);
    }
    assert!(
        replayed.iter().any(|u| spoken(u, "find me a candidate")),
        "reopening a Bot by name lost its conversation: {replayed:?}"
    );
}

/// An operator who pinned this process to one Bot with `--bot` decides above
/// the client. A client asking for a different one by name must not bypass
/// that.
#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_agent_ignores_a_client_asking_for_someone_else() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: Some("Pinned".to_owned()),
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    engine
        .new_session_for(Some(Who::Bot("Somebody Else".into())), "/tmp/whatever")
        .await
        .expect("session/new");

    let bots = openbot_bots::BotStore::open(agent_home.path()).unwrap();
    let names: Vec<String> = bots
        .list(true)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(
        names,
        vec!["Pinned".to_owned()],
        "a client talked past the operator's --bot and reached {names:?}"
    );
}

/// A client can open a group and take a turn in it. A group puts several Bots
/// on one thread so the handoff between them is visible in one conversation;
/// ACP addresses a Bot, so a group session names the group in `_meta` and the
/// agent decides per message who answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_can_open_a_group_and_the_turn_lands_in_its_thread() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");
    let home = agent_home.path();

    let openbot = common::up::openbot();
    let run = |args: Vec<String>| {
        let out = std::process::Command::new(&openbot)
            .args(&args)
            .arg("--home")
            .arg(home)
            .env("NO_COLOR", "1")
            .env_remove("OPENBOT_HOME")
            .env_remove("OPENBOT_HUB_URL")
            .output()
            .expect("openbot");
        assert!(
            out.status.success(),
            "`openbot {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(vec!["bot".into(), "new".into(), "Researcher".into()]);
    run(vec!["bot".into(), "new".into(), "Writer".into()]);
    run(vec![
        "group".into(),
        "new".into(),
        "Launch".into(),
        "--members".into(),
        "researcher,writer".into(),
    ]);

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: home.to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("connect");

    let session = engine
        .new_session_for(Some(Who::Group("Launch".into())), "/tmp/launch")
        .await
        .expect("session/new for a group");

    let (stop, _said) = tokio::time::timeout(
        Duration::from_secs(90),
        engine.prompt(&session, "@writer draft the announcement"),
    )
    .await
    .expect("the turn did not finish in time")
    .expect("the turn failed");
    assert_eq!(stop, StopReason::EndTurn);

    let bots = openbot_bots::BotStore::open(home).unwrap();
    let group = bots.resolve_group("Launch").expect("the group");
    let thread = bots.group_history(&group.id, None).expect("the thread");
    assert_eq!(thread.len(), 2, "the exchange should be in the thread");

    // The member who answered kept its own conversation separate.
    let writer = bots.resolve("Writer").expect("the member");
    assert!(
        bots.history(&writer.id, None).expect("its log").is_empty(),
        "a group turn filled the member's own log"
    );

    // Reopening the group replays the thread, not a Bot's history.
    let restored = SessionId::new("openbot-group");
    engine
        .load_session_for(&restored, Some(Who::Group("Launch".into())), "/tmp/launch")
        .await
        .expect("session/load for a group");
    let mut replayed = Vec::new();
    while let Some((_sid, u)) = engine.next_update() {
        replayed.push(u);
    }
    assert!(
        replayed.iter().any(|u| spoken(u, "draft the announcement")),
        "reopening the group lost its thread: {replayed:?}"
    );
}

/// A group that does not exist is an error. `session/new` invents a Bot for an
/// unused directory, which is right for a teammate and wrong for a group:
/// groups have members, and an empty one is a thread with nobody to answer.
#[tokio::test(flavor = "multi_thread")]
async fn opening_a_group_that_does_not_exist_is_refused() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home");
    let engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: true,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("connect");

    let err = engine
        .new_session_for(Some(Who::Group("No Such Group".into())), "/tmp/x")
        .await
        .expect_err("an unknown group should not be invented");
    assert!(
        format!("{err:#}").contains("No Such Group"),
        "the error should name what was asked for: {err}"
    );
}

use agent_client_protocol::schema::v1::{
    ContentBlock, RequestPermissionOutcome, SelectedPermissionOutcome, SessionId, SessionUpdate,
    StopReason,
};

/// Whether this update carries these words, whoever said them.
///
/// Separate from [`carries`], which asks whether the Bot said something; a
/// replay has to prove both halves of a conversation came back.
fn spoken(maybe: &SessionUpdate, text: &str) -> bool {
    let block = match maybe {
        SessionUpdate::UserMessageChunk(chunk) | SessionUpdate::AgentMessageChunk(chunk) => {
            &chunk.content
        }
        _ => return false,
    };
    matches!(block, ContentBlock::Text(t) if t.text.contains(text))
}

/// Whether this update carries the words of the scripted reply.
fn carries(maybe: &SessionUpdate, text: &str) -> bool {
    match maybe {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => t.text.contains(text),
            _ => false,
        },
        _ => false,
    }
}

/// A credential request reaches the window, and the value reaches the hub.
///
/// Each layer has its own test; this covers the join. `openbot acp` marks the
/// request in `_meta`, the engine decodes it, the shell renders an input, and
/// this checks that the thing sent is the thing decoded.
///
/// It also pins the shape of the ask. ACP has no free-text prompt, so this
/// arrives as a `session/request_permission`, and a client that read only
/// `options` would draw allow/deny buttons for a question that wants a value.
/// `secret_request()` being `Some` is what tells them apart.
///
/// Driven with `--demo-secret`, because the alternative is a real model
/// deciding it wants a token.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_request_reaches_the_client_and_the_value_is_stored() {
    let up = Up::start().expect("openbot up");
    let agent_home = tempfile::tempdir().expect("a home for the agent's bots");

    let mut engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: up.hub.clone(),
        demo: false,
        demo_tools: false,
        demo_secret: true,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    let session = engine
        .new_session("/tmp/openbot-secret")
        .await
        .expect("session/new");
    let mut turn = engine
        .prompt_start(&session, "get the token")
        .await
        .expect("the turn did not start");

    const VALUE: &str = "sk-live-FROM-THE-WINDOW-3d1f";
    let mut asked = Vec::new();
    let mut gated = Vec::new();
    let stop = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            while engine.next_update().is_some() {}
            while let Some(mut pending) = engine.next_permission() {
                match pending.secret_request() {
                    Some(ask) => {
                        asked.push((ask.name.clone(), ask.why.clone()));
                        assert!(
                            pending.supply(VALUE),
                            "the credential this turn is blocked on was refused"
                        );
                    }
                    // Answered rather than treated as a failure, and then
                    // asserted to have never happened. `secret.request` is a
                    // tool call; if it fell through to the `RequireApproval`
                    // fallback, this turn would ask twice for one decision (an
                    // approval card, then the box that names the credential).
                    // The shipped policy allows it, so this arm should not
                    // run; keeping it answerable means a regression reports
                    // the double prompt rather than hanging until the timeout.
                    None => {
                        gated.push(pending.tool_call().fields.title.clone().unwrap_or_default());
                        assert!(
                            pending.answer(RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new("allow-once"),
                            )),
                            "the approval this turn is blocked on refused the answer"
                        );
                    }
                }
            }
            match turn.try_recv() {
                Ok(reply) => return reply.expect("the turn failed"),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    panic!("openbot acp is gone");
                }
            }
        }
    })
    .await
    .expect("the turn did not finish in time");

    // The question arrived, decoded, with both halves: the name to store it
    // under and the reason a person needs in order to answer.
    assert_eq!(
        asked.len(),
        1,
        "asked for a credential {} times, expected once",
        asked.len()
    );
    // One decision, one prompt. Anything here is an approval card in front of
    // the credential box, asking whether the Bot may ask: strictly less
    // informative than the box, and a second dialog trains people to click
    // through both.
    assert!(
        gated.is_empty(),
        "a credential request cost a second prompt: {gated:?}"
    );
    assert_eq!(asked[0].0, "demo-token");
    assert!(
        asked[0].1.contains("credential request"),
        "the reason did not survive the trip: {:?}",
        asked[0].1
    );
    assert_eq!(format!("{stop:?}"), "EndTurn", "the turn did not finish");

    // The value went into the store, under that name, reachable by a
    // connector, and not back into the conversation.
    let held = openbot_desktop::secrets::list(&common::up::openbot(), &up.home)
        .await
        .expect("secret ls");
    let stored = held
        .iter()
        .find(|s| s.name == "demo-token")
        .expect("the credential the person typed was not stored");
    assert!(!stored.fingerprint.is_empty());
}

/// The first thing a person meets after installing is a home with no
/// `config.toml` in it, and `openbot acp` refuses to start there. It says why,
/// on stderr, in a sentence that names the fix:
///
/// ```text
/// Error: no usable model: no model configured.
/// Set one once:  openbot config set --model grok-4-5
/// ```
///
/// The engine used to abort the task that held that message and report
/// "openbot acp ended before the handshake" — a protocol event standing in for
/// a configuration fact, offering nothing to act on. Every other test in this
/// file passes `demo: true`, which is precisely why none of them ever walked
/// this path.
///
/// No hub is needed: `openbot acp` reaches one lazily per turn, and with no
/// model it exits before any of that.
#[tokio::test(flavor = "multi_thread")]
async fn a_home_with_no_model_says_so_instead_of_blaming_the_handshake() {
    let agent_home = tempfile::tempdir().expect("a home with nothing configured in it");

    // Matched rather than `expect_err`: `Engine` has no `Debug`, and giving it
    // one to satisfy a test would put the command channel and the join handle
    // in reach of anything that formats it.
    let err = match Engine::connect(Config {
        openbot: common::up::openbot(),
        home: agent_home.path().to_path_buf(),
        hub: "ws://127.0.0.1:8443/v1/tools".to_owned(),
        demo: false,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    {
        Ok(_) => panic!("a home with no model configured cannot produce a working agent"),
        Err(e) => e,
    };

    let text = format!("{err:#}");
    assert!(
        text.contains("no model configured") || text.contains("no usable model"),
        "the error should carry what the agent actually said: {text}"
    );
    assert!(
        text.contains("config set"),
        "the error should carry the fix the agent named: {text}"
    );
    assert!(
        !text.contains("ended before the handshake"),
        "a missing model is not a protocol problem: {text}"
    );
}

/// A key typed into the window reaches the agent, and does not end up on disk.
///
/// The runtime reads the key with `std::env::var` under the name `config.toml`
/// records, and that file holds the name and never the value — a key in a
/// config file ends up in a backup, a screen share or a repository. So the only
/// place a window that collects a key can put it is the environment of the
/// process it spawns, and this checks all three halves of that: without the key
/// the agent refuses and names the variable, with it the agent starts, and the
/// file it wrote never contains the value.
///
/// The variable is deliberately one nothing sets, so a key leaking in from the
/// developer's own environment cannot make this pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_from_the_window_reaches_the_agent_and_is_never_written_down() {
    const VAR: &str = "OPENBOT_TEST_KEY_THAT_NOTHING_SETS";
    const VALUE: &str = "not-a-real-key-9f2a";
    assert!(
        std::env::var(VAR).is_err(),
        "{VAR} is set in this environment, so this test would pass without proving anything"
    );

    let home = tempfile::tempdir().expect("a home");
    openbot_desktop::settings::save_model(
        &common::up::openbot(),
        home.path(),
        "grok-4-5",
        Some("openai"),
        Some("https://api.x.ai/v1"),
        Some(VAR),
    )
    .await
    .expect("config set");

    let cfg = |api_key: Option<(String, String)>| Config {
        openbot: common::up::openbot(),
        home: home.path().to_path_buf(),
        hub: "ws://127.0.0.1:8443/v1/tools".to_owned(),
        demo: false,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key,
    };

    // Without it, the agent refuses and says which variable it wanted.
    let err = match Engine::connect(cfg(None)).await {
        Ok(_) => panic!("an unset key should not produce a working agent"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        err.contains(VAR),
        "the refusal should name the variable it looked for: {err}"
    );

    // With it, the agent starts.
    let engine = Engine::connect(cfg(Some((VAR.to_owned(), VALUE.to_owned()))))
        .await
        .expect("the key the window collected should have reached the agent");
    drop(engine);

    // And the file records the name, never the value.
    let toml = std::fs::read_to_string(home.path().join("config.toml")).expect("config.toml");
    assert!(
        !toml.contains(VALUE),
        "the key must never be written to config.toml, found it in:\n{toml}"
    );
    assert!(
        toml.contains(VAR),
        "the file should record the variable's name:\n{toml}"
    );
}

/// A model on this computer starts with no credential anywhere in the picture.
///
/// This is the whole first-run claim in one test. Somebody who has just
/// installed the app, has no account with any vendor and has exported nothing
/// picks a local provider in the window, and the agent starts. Every earlier
/// layer treats an empty string as "not filled in", so the failure this guards
/// against is any one of them helpfully dropping the field: the window, the
/// `config set` call, or the settings merge. Drop it in any of those and the
/// stored key variable survives instead, the runtime looks it up, and a local
/// model is refused for want of a key that was never needed.
///
/// Driven through `save_model` rather than by writing `config.toml` directly,
/// because the passthrough is the thing under test and a hand-written file
/// would skip it.
#[tokio::test(flavor = "multi_thread")]
async fn a_local_model_starts_with_no_key_configured_anywhere() {
    let home = tempfile::tempdir().expect("a home");
    openbot_desktop::settings::save_model(
        &common::up::openbot(),
        home.path(),
        "qwen3:1.7b",
        Some("openai"),
        Some("http://localhost:11434/v1"),
        // The empty name is the message: this endpoint wants no credential.
        Some(""),
    )
    .await
    .expect("config set");

    let toml = std::fs::read_to_string(home.path().join("config.toml")).expect("config.toml");
    assert!(
        toml.contains(r#"api_key_env = """#),
        "the `no key wanted` choice did not survive the trip to disk:\n{toml}"
    );

    // And the agent starts on it, with nothing supplying a key. Reaching the
    // handshake is the assertion: it is the point the runtime has resolved a
    // model, which is where a missing key fails.
    let engine = Engine::connect(Config {
        openbot: common::up::openbot(),
        home: home.path().to_path_buf(),
        hub: "ws://127.0.0.1:8443/v1/tools".to_owned(),
        demo: false,
        demo_tools: false,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("a local model needs no key, so the agent should start without one");
    drop(engine);
}
