//! Starting a computer, against the shipped binary.
//!
//! An installed window whose answer to "no computer" is "open a terminal and
//! type `botroster up`" is not an application. The window starts one itself, and
//! these are the properties that has to hold: it serves when the call returns,
//! it stops when the handle drops, and a failure is reported rather than
//! waited out.

mod common;

use agent_client_protocol::schema::v1::{
    RequestPermissionOutcome, SelectedPermissionOutcome, StopReason,
};
use botroster_desktop::engine::{Config, Engine};
use botroster_desktop::hub;
use std::time::Duration;

/// A free port, so a test never fights the default one or another test.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port()
}

/// The call returns only once the computer answers.
///
/// The window connects immediately afterwards, so returning early would race
/// the daemon's startup and report "refused" about a computer that was seconds
/// from being ready.
#[tokio::test]
async fn a_started_computer_is_serving_when_the_call_returns() {
    let dir = tempfile::tempdir().unwrap();
    let hub_url = format!("ws://127.0.0.1:{}/v1/tools", free_port());

    let started = hub::start(
        &common::up::botroster(),
        &dir.path().join("home"),
        &hub_url,
        Duration::from_secs(90),
    )
    .await
    .expect("a computer starts");

    // Asked, not assumed: the point of waiting is that this answers.
    let reach = hub::reach(&common::up::botroster(), &hub_url, &dir.path().join("home"))
        .await
        .expect("the binary runs");
    assert!(
        reach.is_serving(),
        "start returned but nothing is serving at {hub_url}: {reach:?}"
    );
    drop(started);
}

/// Dropping the handle stops the computer.
///
/// The window drops it on disconnect and on close. A child that outlived the
/// window would hold the workspace lock, and the next launch would fail to
/// start one with nothing on screen explaining why.
#[tokio::test]
async fn dropping_the_handle_stops_the_computer() {
    let dir = tempfile::tempdir().unwrap();
    let hub_url = format!("ws://127.0.0.1:{}/v1/tools", free_port());

    let started = hub::start(
        &common::up::botroster(),
        &dir.path().join("home"),
        &hub_url,
        Duration::from_secs(90),
    )
    .await
    .expect("a computer starts");
    drop(started);

    // The kill is a signal, not an instant: poll for the port to close rather
    // than sleeping an interval that is either flaky or slow.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let reach = hub::reach(&common::up::botroster(), &hub_url, &dir.path().join("home"))
            .await
            .expect("the binary runs");
        if !reach.is_serving() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the computer is still serving at {hub_url} after its handle was dropped"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A binary that is not there fails immediately and says so.
///
/// The window offers a path to the runtime that a person can edit. A wrong one
/// must report itself rather than spending the whole patience window polling a
/// hub that nothing is ever going to serve.
#[tokio::test]
async fn a_missing_binary_is_reported_rather_than_waited_out() {
    let dir = tempfile::tempdir().unwrap();
    let began = std::time::Instant::now();
    let err = hub::start(
        &dir.path().join("not-botroster"),
        &dir.path().join("home"),
        &format!("ws://127.0.0.1:{}/v1/tools", free_port()),
        Duration::from_secs(60),
    )
    .await
    .expect_err("a binary that does not exist cannot start a computer");

    assert!(
        began.elapsed() < Duration::from_secs(10),
        "waited {:?} for a binary that does not exist",
        began.elapsed()
    );
    let said = err.to_string();
    assert!(
        said.contains("not-botroster"),
        "the error does not name the binary it could not run: {said}"
    );
}

/// A port that accepts and then says nothing does not hang the window.
///
/// "Refused" is not the only way for a hub to be absent. Anything else already
/// listening on the port accepts the connection and never completes the
/// handshake, and an unbounded ask would leave the window sitting on Connect
/// with no error and no computer for as long as a person was willing to watch
/// it. A wrong port is far more likely to find some other service than to find
/// nothing.
#[tokio::test]
async fn a_port_that_accepts_and_never_answers_is_not_waited_on_forever() {
    // Held for the whole test and never accepted from, so a connection to it
    // establishes and then stalls: the black hole this guards against.
    let black_hole = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to hold");
    let port = black_hole.local_addr().expect("its address").port();

    let began = std::time::Instant::now();
    // No hub was ever started, so there is no home holding a token; any path
    // does, and an empty one is the honest shape of "nothing here".
    let nowhere = tempfile::tempdir().expect("a temp dir");
    let reach = hub::reach(
        &common::up::botroster(),
        &format!("ws://127.0.0.1:{port}/v1/tools"),
        nowhere.path(),
    )
    .await
    .expect("the binary runs");

    assert!(
        !reach.is_serving(),
        "a socket that never answers was reported as a computer: {reach:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(40),
        "waited {:?} on a port that was never going to answer",
        began.elapsed()
    );
}

/// A hub that refuses this window is reported, not started over.
///
/// The window takes a hub URL and a home as independent inputs — `Config` has
/// both, and Connect accepts both — so pointing it at a hub started on someone
/// else's home is a shape the product supports. The token lives in the *hub's*
/// home, so that window is refused.
///
/// What must not then happen is the window deciding nothing is there and
/// starting its own: the first hub is holding the port, so the second fails on
/// the bind, and the person is shown "address in use" about a computer that is
/// running perfectly well and simply does not accept them. `botroster up`'s
/// `hub_or_start` had exactly this bug and it was fixed there first; this test
/// exists because the fix was in one of the two places a person meets it.
#[tokio::test]
async fn a_hub_that_refuses_this_window_is_reported_rather_than_started_over() {
    let Some(up) = common::up::Up::start() else {
        return;
    };
    let elsewhere = tempfile::tempdir().expect("a home holding no token for that hub");

    let reach = hub::reach(&common::up::botroster(), &up.hub, elsewhere.path())
        .await
        .expect("the binary runs");

    let hub::Reach::Refused(why) = reach else {
        panic!(
            "a hub that refused this window came back as {reach:?}. `Unreachable` sends the \
             caller to `hub::start`, which binds the port this hub is already holding — so the \
             person is told \"address in use\" about a computer that is running."
        );
    };
    assert!(
        why.contains(botroster_proto::HUB_TOKEN_FILE),
        "the refusal reached the window without the sentence that says what to fix: {why}"
    );
}

/// The anti-vacuity half: the same call, with the home the hub was started on,
/// is serving. Without it, a `reach` that answered `Refused` to everything
/// would satisfy the test above and break every real window.
#[tokio::test]
async fn the_home_the_hub_was_started_on_reaches_it() {
    let Some(up) = common::up::Up::start() else {
        return;
    };
    let reach = hub::reach(&common::up::botroster(), &up.hub, &up.home)
        .await
        .expect("the binary runs");
    assert!(
        reach.is_serving(),
        "the window could not reach a hub started on the home it was given: {reach:?}"
    );
}

/// A window that starts its own computer can then use it.
///
/// The ordering no other test here exercises, and the one every real first
/// launch takes: the window spawns `botroster acp` **first**, then looks for a
/// computer, then starts one. Every other live test in this workspace has a hub
/// already running before the engine connects, so all of them missed what
/// shipped in 0.5.0 — the agent was handed the token that existed at spawn
/// time, `botroster up` then minted a fresh one two seconds later, and since
/// `hub_token` reads the environment before the file the agent went on
/// presenting the dead one until the window was restarted. Every turn came back
/// "the hub refused this connection".
///
/// So this drives the real sequence and then does something that needs the hub.
/// A handshake alone would have passed throughout: the agent reaches the hub
/// lazily, per turn, which is exactly why the failure only appeared on use.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_spawned_before_the_computer_can_still_reach_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let home = dir.path().join("home");
    let hub_url = format!("ws://127.0.0.1:{}/v1/tools", free_port());

    // A token from a hub that is no longer running — the ordinary state of any
    // home a window has been opened on before, since `botroster up` writes one
    // every start and nothing removes it on exit.
    //
    // **This is the whole mechanism.** Written first, this test passed with the
    // defect restored: with an *empty* home the agent is handed nothing, falls
    // back to `--home`, and works by accident. The bug needs a stale value to
    // capture, which is why a real install hit it and an empty temp dir did
    // not.
    const STALE: &str = "a token from a hub that has already stopped";
    botroster_proto::write_hub_token(&home, STALE).expect("seed a previous hub's token");

    // The agent first, as `botroster-app`'s `connect` does it.
    let mut engine = Engine::connect(Config {
        botroster: common::up::botroster(),
        home: home.clone(),
        hub: hub_url.clone(),
        demo: false,
        demo_tools: true,
        demo_secret: false,
        bot: None,
        api_key: None,
    })
    .await
    .expect("the engine could not connect");

    // Then the computer, which mints a token the agent could not have seen.
    let _started = hub::start(
        &common::up::botroster(),
        &home,
        &hub_url,
        Duration::from_secs(90),
    )
    .await
    .expect("a computer starts");

    let minted = botroster_desktop::hub::token_at(&home)
        .expect("`botroster up` writes a token into the home it was started on");
    assert_ne!(
        minted, STALE,
        "`botroster up` reused the token already in the home, so nothing went stale"
    );

    // A turn that actually calls a tool. The scripted tool demo writes a file
    // and reads it back, so it cannot succeed without the hub admitting the
    // agent.
    let session = engine
        .new_session(dir.path().to_string_lossy().as_ref())
        .await
        .expect("the agent could not open a session against the computer it was given");

    // Driven the way the shell drives it: start the turn, then drain updates
    // and answer the dialogs it blocks on. The scripted tool demo calls
    // `fs.write` and `shell.exec`, which the default policy asks about, and an
    // unanswered ask is indistinguishable from a hub that never replied.
    let mut turn = engine
        .prompt_start(&session, "prove it")
        .await
        .expect("the turn did not start");

    let mut updates = 0usize;
    let stop = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            while engine.next_update().is_some() {
                updates += 1;
            }
            while let Some(mut pending) = engine.next_permission() {
                assert!(
                    pending.answer(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new("allow-once"),
                    )),
                    "the approval this turn is blocked on refused the answer"
                );
            }
            match turn.try_recv() {
                Ok(reply) => {
                    return reply.unwrap_or_else(|e| {
                        panic!(
                            "the agent could not use the computer the window started for it: \
                             {e}\nIf this says the hub refused the connection, the agent is \
                             presenting a token it read before `botroster up` replaced it — see \
                             the comment in `engine.rs` about why this child is given `--home` \
                             and no token."
                        )
                    })
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    panic!("`botroster acp` is gone")
                }
            }
        }
    })
    .await
    .expect("the turn did not finish in time");

    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the scripted tool demo completes when its tools are allowed, so anything else means \
         the turn broke"
    );
    assert!(
        updates > 0,
        "the turn produced no updates, so it proves nothing about reaching the computer"
    );
}
