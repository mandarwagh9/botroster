//! The OPENBOT shell's command layer, driven the way the window drives it.
//!
//! `tauri::test`'s mock runtime builds the same commands the binary ships and
//! invokes them over the same IPC path, with no window, so this is the
//! shipped shell talking to a shipped `openbot`, with a real hub and guest
//! underneath. The engine below is covered by its own tests and `main.rs`
//! above is not reachable from a test binary; this file covers the command
//! layer between them.

// The same harness the engine's tests use, included rather than copied.
// `openbot-cli` and `openbot` already keep one copy each because test support
// cannot cross package boundaries; a third would be a third thing to keep in
// step. openbot-app depends on openbot already, so the coupling is not new.
#[path = "../../openbot-desktop/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openbot_app::{shell, Mode};
use serde::Deserialize;
use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Listener, WebviewWindow, WebviewWindowBuilder};

use common::up::Up;

/// The first thing the scripted tool demo says, before it calls `fs.write`.
/// The script is `say_and_call`, so these words exist on the wire before
/// the approval the turn then blocks on, which is what makes them a probe
/// for streaming rather than a probe for the demo working.
const FIRST_WORDS: &str = "Saving a note to the workspace";

/// The last thing it says, after every tool has run.
const LAST_WORDS: &str = "Demo complete";

/// What the window receives on a `chunk` event.
///
/// Declared here rather than reusing `openbot_app::Chunk`, so the test pins
/// the JSON the page actually parses. A shared struct would agree with itself
/// no matter what it was renamed to.
#[derive(Debug, Deserialize)]
struct GotChunk {
    session: String,
    kind: String,
    text: String,
}

/// What the window receives on a `permission-request` event.
#[derive(Clone, Debug, Deserialize)]
struct GotAsk {
    id: String,
    session: String,
    tool: String,
    fields: Vec<GotField>,
    options: Vec<GotOption>,
}

#[derive(Clone, Debug, Deserialize)]
struct GotField {
    name: String,
    value: String,
    long: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GotOption {
    id: String,
    /// What the button says. The page renders this verbatim.
    name: String,
    /// `allow_once`, `allow_always`, `reject_once`: the protocol's own kinds.
    kind: String,
}

/// How a turn ended, as the window is told.
#[derive(Debug, Deserialize)]
struct GotTurn {
    stop: String,
    note: String,
}

/// A shell with its event streams tapped, exactly as built for the window.
struct Shell {
    webview: WebviewWindow<MockRuntime>,
    chunks: Arc<Mutex<Vec<GotChunk>>>,
    asks: Arc<Mutex<Vec<GotAsk>>>,
    // The app owns the runtime the commands run on; dropping it ends them.
    _app: tauri::App<MockRuntime>,
}

impl Shell {
    fn build(mode: Mode) -> Self {
        let app = shell(mock_builder(), mode)
            .build(tauri::generate_context!())
            .expect("the shell could not be built");
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("no webview");

        let chunks: Arc<Mutex<Vec<GotChunk>>> = Arc::default();
        let asks: Arc<Mutex<Vec<GotAsk>>> = Arc::default();
        app.listen("chunk", {
            let chunks = chunks.clone();
            move |event| {
                let got = serde_json::from_str(event.payload()).expect("a chunk the page can read");
                chunks.lock().unwrap().push(got);
            }
        });
        app.listen("permission-request", {
            let asks = asks.clone();
            move |event| {
                let got = serde_json::from_str(event.payload()).expect("an ask the page can read");
                asks.lock().unwrap().push(got);
            }
        });

        Self {
            webview,
            chunks,
            asks,
            _app: app,
        }
    }

    /// Invoke a command the way the page does.
    fn call(&self, cmd: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
        call(&self.webview, cmd, args)
    }

    fn said(&self) -> Vec<String> {
        self.chunks
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }
}

fn call(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request = InvokeRequest {
        cmd: cmd.to_owned(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: if cfg!(any(windows, target_os = "android")) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        }
        .parse()
        .unwrap(),
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(webview, request)
        .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        .map_err(|e| e.to_string())
}

/// Poll until `f` produces something, or give up.
fn wait_for<T>(mut f: impl FnMut() -> Option<T>, within: Duration) -> Option<T> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(got) = f() {
            return Some(got);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Connect a shell to a live stack and open a session on it.
fn connected(mode: Mode, cwd: &str) -> (Up, tempfile::TempDir, Shell, String) {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(mode);

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    let session = shell
        .call("new_session", json!({ "cwd": cwd }))
        .expect("new_session");
    let session = session.as_str().expect("a session id").to_owned();
    (up, home, shell, session)
}

/// The streaming test.
///
/// The scripted demo says "Saving a note to the workspace." and then calls
/// `fs.write`, which the default policy asks about, so the turn blocks with
/// those words already spoken. If the shell hands the conversation over only
/// when the turn ends, the window has been told nothing at this moment, and
/// a person is watching a spinner while the agent waits on them.
///
/// A `prompt` that accumulated every chunk into a `Vec` and returned it at
/// the end would fail here and nowhere else: the layer below streams
/// correctly and its test asserts membership in a drained channel, which is
/// true whether the words arrived during the turn or all at once after it.
#[test]
fn the_words_reach_the_window_while_the_turn_is_still_running() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-stream");

    let turn = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    let ask = wait_for(
        || shell.asks.lock().unwrap().first().cloned(),
        Duration::from_secs(120),
    )
    .expect("the agent never asked to write the file");

    // Without this the test could pass on a turn that had already finished,
    // which would say nothing about streaming at all.
    assert!(
        !turn.is_finished(),
        "the turn ended before the approval arrived, so nothing was blocked \
         and this proves nothing about when the words arrived"
    );

    let said = shell.said();
    assert!(
        said.iter().any(|t| t.contains(FIRST_WORDS)),
        "the turn is paused on an approval and the window has been told \
         nothing; the shell is buffering the conversation instead of \
         streaming it. Seen so far: {said:?}"
    );
    assert_eq!(
        ask.session, session,
        "an ask must say which conversation it belongs to"
    );
    assert!(
        ask.tool.starts_with("fs.write"),
        "the first ask should be the write, got {:?}",
        ask.tool
    );
    // The docs: review the target, the scope and the values. The target is
    // the first thing on the card, not something to find inside a blob.
    let target = ask.fields.first().expect("an approval with no arguments");
    assert_eq!(
        target.name, "path",
        "the file being written should lead the card, got {:?}",
        ask.fields
    );
    assert_eq!(target.value, "openbot-demo.md");
    assert!(!target.long, "a filename belongs on a line");
    // Nothing is hidden: the contents are there too, in full.
    let contents = ask
        .fields
        .iter()
        .find(|f| f.name == "contents")
        .expect("the file contents");
    assert!(
        contents.long && contents.value.contains("If you can read this"),
        "an approval that hides what it would write is not a decision: {contents:?}"
    );
    assert!(
        ask.options.iter().any(|o| o.id == "allow-once"),
        "the standard options were not offered: {:?}",
        ask.options
    );
    // The page puts `name` on the button. An id is not a label.
    for option in &ask.options {
        assert!(
            !option.name.is_empty() && option.name != option.id,
            "a button reading {:?} is the protocol talking, not the shell",
            option.name
        );
        // The page styles allow and deny differently, so it matches on this.
        // "not empty" is too weak: it lets a JSON-encoded `"\"allow_once\""`,
        // quotes and all, through to the window.
        assert!(
            ["allow_once", "allow_always", "reject_once", "reject_always"]
                .contains(&option.kind.as_str()),
            "the page cannot tell allow from deny given kind {:?}",
            option.kind
        );
    }
    assert!(
        !ask.options.iter().any(|o| o.id == "reject-always"),
        "openbot has no durable deny, so a never-allow button would be a lie: {:?}",
        ask.options
    );
    // The page offers no refusal of its own when the agent already offers
    // one, so there must be exactly one way to say no; two buttons that
    // both mean "no" make a person wonder what the difference is.
    assert_eq!(
        ask.options
            .iter()
            .filter(|o| o.kind.starts_with("reject"))
            .count(),
        1,
        "expected exactly one way to decline: {:?}",
        ask.options
    );

    // Let it finish: answer every ask as it arrives until the turn returns.
    let mut answered = 0;
    while !turn.is_finished() {
        let waiting: Vec<GotAsk> = shell.asks.lock().unwrap().drain(..).collect();
        for ask in waiting {
            shell
                .call(
                    "answer_permission",
                    json!({ "id": ask.id, "optionId": "allow-once" }),
                )
                .expect("answer_permission");
            answered += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let turn: GotTurn = serde_json::from_value(turn.join().unwrap().expect("the turn failed"))
        .expect("a turn the page can read");
    assert_eq!(turn.stop, "EndTurn", "the demo completes when allowed");
    assert!(
        turn.note.is_empty(),
        "an ordinary ending needs no announcement, got {:?}",
        turn.note
    );
    assert_eq!(
        answered, 2,
        "fs.write and shell.exec are the two the policy asks about"
    );
    // The end of the turn, which is the part that races. The agent sends its
    // last words and then answers the prompt; a client that stops draining
    // the moment the answer arrives loses them, and the Bot looks like it
    // stopped mid-thought. Asserted after the turn has returned, so there is
    // no sleep here pretending to be a synchronisation primitive.
    assert!(
        shell.said().iter().any(|t| t.contains(LAST_WORDS)),
        "the demo's closing words never reached the window; the last chunk was dropped when the stop reason beat it. Seen: {:?}",
        shell.said()
    );
}

/// Every chunk says which conversation it belongs to, and a thought is never
/// dressed up as speech. The page filters on the session and styles on the
/// kind, so both are load-bearing.
#[test]
fn chunks_are_labelled_for_the_page() {
    let (_up, _home, shell, session) = connected(Mode::Reply, "/tmp/openbot-labels");

    shell
        .call("prompt", json!({ "session": session, "text": "go" }))
        .expect("prompt");

    let chunks = shell.chunks.lock().unwrap();
    assert!(!chunks.is_empty(), "the demo reply reached nobody");
    for chunk in chunks.iter() {
        assert_eq!(
            chunk.session, session,
            "a chunk with the wrong session id is invisible to the page"
        );
        assert!(
            ["agent", "user", "thought", "tool", "progress", "result"]
                .contains(&chunk.kind.as_str()),
            "the page has no style for kind {:?}",
            chunk.kind
        );
    }
    assert!(
        chunks
            .iter()
            .any(|c| c.kind == "agent" && c.text == "Done."),
        "the scripted reply is not there as agent speech: {chunks:?}"
    );
}

/// Refusing is a decision the turn must survive: the tool is denied, and the
/// turn still ends rather than hanging on a question nobody will answer
/// again.
#[test]
fn refusing_every_approval_still_ends_the_turn() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-refuse");

    let turn = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    let mut refused = 0;
    while !turn.is_finished() {
        let waiting: Vec<GotAsk> = shell.asks.lock().unwrap().drain(..).collect();
        for ask in waiting {
            // "" is what the page's Refuse button sends.
            shell
                .call("answer_permission", json!({ "id": ask.id, "optionId": "" }))
                .expect("answer_permission");
            refused += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let turn: GotTurn = serde_json::from_value(turn.join().unwrap().expect("the turn failed"))
        .expect("a turn the page can read");
    assert_eq!(refused, 2, "both tool calls should have been asked about");
    assert_eq!(turn.stop, "EndTurn", "a refused tool is not a broken turn");
}

/// Answering the same ask twice is what a double-click is. The second answer
/// must be refused by name rather than reaching the agent, because by then
/// the id belongs to nothing.
#[test]
fn an_approval_cannot_be_answered_twice() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-twice");

    let turn = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    let ask = wait_for(
        || shell.asks.lock().unwrap().first().cloned(),
        Duration::from_secs(120),
    )
    .expect("the agent never asked");

    shell
        .call(
            "answer_permission",
            json!({ "id": ask.id, "optionId": "allow-once" }),
        )
        .expect("the first answer lands");
    let again = shell.call(
        "answer_permission",
        json!({ "id": ask.id, "optionId": "allow-once" }),
    );
    assert!(
        again.is_err(),
        "a second answer to the same ask was accepted: {again:?}"
    );

    // Drain the rest so the turn ends and the stack comes down cleanly.
    while !turn.is_finished() {
        let waiting: Vec<GotAsk> = shell.asks.lock().unwrap().drain(..).collect();
        for ask in waiting {
            let _ = shell.call(
                "answer_permission",
                json!({ "id": ask.id, "optionId": "allow-once" }),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = turn.join();
}

/// Stopping withdraws the questions as well as the turn. The ids come back so
/// the page can take those dialogs down; a dialog left on screen after the
/// agent stopped waiting is a button that does nothing.
#[test]
fn stopping_withdraws_the_approvals_it_was_waiting_on() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-stop");

    let turn = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    let ask = wait_for(
        || shell.asks.lock().unwrap().first().cloned(),
        Duration::from_secs(120),
    )
    .expect("the agent never asked");

    let withdrawn = shell
        .call("cancel", json!({ "session": session }))
        .expect("cancel");
    let withdrawn: Vec<String> = serde_json::from_value(withdrawn).expect("a list of ids");
    assert!(
        withdrawn.contains(&ask.id),
        "the ask the person was looking at was not withdrawn: {withdrawn:?}"
    );

    // It is really gone: answering it now fails, the same as any id the
    // shell is not holding.
    let late = shell.call(
        "answer_permission",
        json!({ "id": ask.id, "optionId": "allow-once" }),
    );
    assert!(
        late.is_err(),
        "a withdrawn ask was still answerable: {late:?}"
    );

    let ended = wait_for(
        || turn.is_finished().then_some(()),
        Duration::from_secs(120),
    );
    assert!(ended.is_some(), "the turn never ended after cancel");

    // It ended because of the cancel. Asserting only "the turn ended" can
    // pass for the wrong reason: cancelling refuses the parked approvals
    // first, and the scripted demo runs to completion through refusals, so
    // the turn can end on its own with `EndTurn` while the cancel
    // notification sits in a queue behind the very prompt it was meant to
    // interrupt.
    let turn: GotTurn = serde_json::from_value(turn.join().unwrap().expect("the turn failed"))
        .expect("a turn the page can read");
    assert_eq!(
        turn.stop, "Cancelled",
        "the stop button did not reach the agent while the turn was running"
    );
    assert_eq!(
        turn.note, "stopped",
        "a cancelled turn should say so to the person"
    );
}

/// The deadlock test. Opening a session while a turn is in flight must
/// work: it is one click, on a button that is on screen the whole time.
///
/// The failure shape is a three-party deadlock none of the other tests here
/// can see: `new_session` holds the shell's engine lock across its round
/// trip; the engine's command loop cannot answer that round trip because it
/// is awaiting the prompt inline; and the prompt cannot finish because it
/// needs the shell's engine lock to hand over the approval it is blocked on.
/// Every one of those is reasonable alone, and together they wedge the
/// entire window.
#[test]
fn a_session_can_be_opened_while_a_turn_is_waiting_on_an_approval() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-alpha");

    let turn = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    // The turn is now parked on `fs.write` and will not move until answered.
    wait_for(
        || shell.asks.lock().unwrap().first().cloned(),
        Duration::from_secs(120),
    )
    .expect("the agent never asked");
    assert!(!turn.is_finished(), "the turn should still be waiting");

    let opened = std::thread::spawn({
        let webview = shell.webview.clone();
        move || {
            call(
                &webview,
                "new_session",
                json!({ "cwd": "/tmp/openbot-beta" }),
            )
        }
    });

    let landed = wait_for(
        || opened.is_finished().then_some(()),
        Duration::from_secs(30),
    );
    assert!(
        landed.is_some(),
        "opening a session hung while a turn was waiting; the window is \
         deadlocked and the only way out is to kill it"
    );
    let second = opened.join().unwrap().expect("new_session");
    assert_ne!(
        second.as_str().expect("a session id"),
        session,
        "a second session must be its own conversation"
    );

    // Let the first turn go, so the stack comes down cleanly.
    while !turn.is_finished() {
        let waiting: Vec<GotAsk> = shell.asks.lock().unwrap().drain(..).collect();
        for ask in waiting {
            let _ = shell.call(
                "answer_permission",
                json!({ "id": ask.id, "optionId": "allow-once" }),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = turn.join();
}

/// What the window receives from `open_bot`.
#[derive(Debug, Deserialize)]
struct GotOpened {
    session: String,
    name: String,
    history: Vec<GotChunk>,
}

/// The promise, at the surface a person touches. OPENBOT's premise is that a
/// teammate remembers; the sidebar's whole job is getting you back to one.
///
/// Opening a Bot must therefore bring its conversation with it, in the reply
/// rather than as events. History emitted as `chunk` events during the
/// command would be dropped: the page filters those by session id, which it
/// does not learn until the command returns, so every replayed chunk would
/// arrive while the page compared it against `null`, and a Bot with a long
/// history would open on an empty transcript.
///
/// This asserts the history is in the reply, so that ordering cannot be
/// reintroduced by rendering it "consistently" with the live stream later.
#[test]
fn opening_a_bot_brings_its_conversation_with_it() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);
    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    // A first conversation, had and finished.
    let opened: GotOpened = serde_json::from_value(
        shell
            .call("open_bot", json!({ "name": "Talent Scout" }))
            .expect("open_bot"),
    )
    .expect("an Opened the page can read");
    assert_eq!(opened.name, "Talent Scout");
    assert!(
        opened.history.is_empty(),
        "a Bot nobody has spoken to has nothing to replay: {:?}",
        opened.history
    );

    shell
        .call(
            "prompt",
            json!({ "session": opened.session, "text": "who should we hire" }),
        )
        .expect("prompt");

    // Away, and back, which is what clicking another Bot and returning does.
    shell
        .call("open_bot", json!({ "name": "Expense Manager" }))
        .expect("open the other one");
    let reopened: GotOpened = serde_json::from_value(
        shell
            .call("open_bot", json!({ "name": "Talent Scout" }))
            .expect("reopen"),
    )
    .expect("an Opened the page can read");

    assert_ne!(
        reopened.session, opened.session,
        "each open is its own session; only the conversation is durable"
    );
    let said: Vec<&str> = reopened.history.iter().map(|c| c.text.as_str()).collect();
    assert!(
        said.iter().any(|t| t.contains("who should we hire")),
        "what the person asked is missing from the reopened transcript: {said:?}"
    );
    assert!(
        said.iter().any(|t| t.contains("Done.")),
        "what the Bot answered is missing from the reopened transcript: {said:?}"
    );
    // A conversation, not a pile.
    let asked = said
        .iter()
        .position(|t| t.contains("who should we hire"))
        .expect("the question");
    let answered = said
        .iter()
        .position(|t| t.contains("Done."))
        .expect("the answer");
    assert!(
        asked < answered,
        "the reply was replayed before the question"
    );

    for chunk in &reopened.history {
        assert_eq!(
            chunk.session, reopened.session,
            "a replayed chunk carrying the wrong session is invisible to the page"
        );
    }
}

/// The sidebar's contents, through the command the window actually calls.
#[test]
fn the_roster_reaches_the_window() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("roster", json!({ "hidden": false }));
    assert!(
        before.is_err(),
        "there is no roster before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    let empty = shell
        .call("roster", json!({ "hidden": false }))
        .expect("roster");
    assert_eq!(empty, json!([]), "a fresh home has nobody in it");

    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");

    let rows: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster the page can read");
    assert_eq!(
        rows.len(),
        1,
        "opening a Bot by name should create it: {rows:?}"
    );
    assert_eq!(rows[0]["name"], "Talent Scout");
    // A title that repeats the name renders as "Talent Scout / Talent Scout"
    // in the sidebar and tells `bot ls` nothing either. Empty until set.
    assert_eq!(
        rows[0]["title"], "",
        "a Bot created by the client should have no title yet: {rows:?}"
    );
}

/// A Bot with no name is a Bot nobody can pick out of a sidebar.
#[test]
fn a_bot_needs_a_name() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);
    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    for blank in ["", "   "] {
        let out = shell.call("open_bot", json!({ "name": blank }));
        assert!(out.is_err(), "{blank:?} was accepted as a name: {out:?}");
    }
}

/// The Agent Computer, through the command the window calls.
///
/// The docs route passwords, 2FA codes and CAPTCHAs through taking control
/// of the computer rather than chat, and openbot enforces that lock in the
/// hub. A window that cannot reach the computer leaves a person only the
/// path the docs warn against.
#[test]
fn the_window_can_open_and_close_the_agent_computer() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("open_computer", json!({}));
    assert!(
        before.is_err(),
        "there is no computer before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    let url = shell
        .call("open_computer", json!({}))
        .expect("open_computer");
    let url = url.as_str().expect("an address").to_owned();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the viewer must never be reachable off this machine: {url}"
    );
    assert!(
        url.contains("?k="),
        "loopback is not a boundary in a browser; the key is what makes it one: {url}"
    );

    // Opening the panel twice must not start a second viewer: that would be
    // two ports onto one signed-in computer, one of which nobody is watching.
    let again = shell.call("open_computer", json!({})).expect("open again");
    assert_eq!(
        again.as_str(),
        Some(url.as_str()),
        "a second viewer was started"
    );

    // While it is open the window can ask whether it is still being served.
    //
    // This pins the wiring, not the process check. `close_computer` drops the
    // viewer, so a command that merely reported "a viewer is held" would pass
    // both of these. What the check is actually made of (a dead process
    // reporting itself dead) is pinned in `openbot`'s `viewer_live.rs`, by
    // killing it.
    assert_eq!(
        shell
            .call("computer_alive", json!({}))
            .expect("computer_alive"),
        json!(true),
        "a running viewer reported itself as gone"
    );

    shell
        .call("close_computer", json!({}))
        .expect("close_computer");

    assert_eq!(
        shell.call("computer_alive", json!({})).expect("computer_alive"),
        json!(false),
        "a closed viewer still reported itself as serving, so the panel would keep showing its last frame"
    );

    // Closing the panel closes the port. A viewer left listening behind a
    // window can still drive the computer.
    let port: u16 = url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|r| r.split('/').next())
        .and_then(|p| p.parse().ok())
        .expect("a port");
    let gone = (0..60).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
    });
    assert!(gone, "the viewer kept serving after the panel was closed");
}

/// Credentials, through the commands the window calls.
///
/// The assertion that matters is the negative one: the value the person typed
/// must not come back out. A store you can read from is not a store, and the
/// window is the surface most likely to be looked at over somebody's shoulder.
#[test]
fn a_credential_supplied_in_the_window_never_comes_back_out_of_it() {
    const VALUE: &str = "sk-live-NEVER-SHOW-THIS-4f2a9c";
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("secret_list", json!({}));
    assert!(
        before.is_err(),
        "there are no credentials before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    assert_eq!(
        shell.call("secret_list", json!({})).expect("list"),
        json!([]),
        "a fresh home holds no credentials"
    );

    shell
        .call(
            "secret_set",
            json!({ "name": "stripe-token", "value": VALUE }),
        )
        .expect("secret_set");

    let held = shell.call("secret_list", json!({})).expect("list");
    let rendered = held.to_string();
    assert!(
        rendered.contains("stripe-token"),
        "the credential was not stored: {rendered}"
    );
    assert!(
        !rendered.contains(VALUE),
        "the window can read a stored credential back: {rendered}"
    );
    let rows: Vec<serde_json::Value> = serde_json::from_value(held).expect("a list");
    assert!(
        rows[0]["fingerprint"]
            .as_str()
            .is_some_and(|f| !f.is_empty() && !VALUE.contains(f)),
        "the fingerprint should identify the value, not reveal it: {rows:?}"
    );

    // A refusal must not quote what was typed. An error carrying a credential
    // puts it in a log, a screenshot and a bug report at once.
    let err = shell
        .call("secret_set", json!({ "name": "", "value": VALUE }))
        .expect_err("a credential needs a name");
    assert!(
        !err.contains(VALUE),
        "the error carried the credential: {err}"
    );

    shell
        .call("secret_remove", json!({ "name": "stripe-token" }))
        .expect("secret_remove");
    assert_eq!(
        shell.call("secret_list", json!({})).expect("list"),
        json!([]),
        "forgetting a credential should forget it"
    );
}

/// Permission rules, through the commands the window calls.
///
/// The docs' auto-review list. The engine enforces the ordering; the window
/// is what writes the rules it enforces, so the two are checked together.
#[test]
fn a_rule_written_in_the_window_is_the_rule_the_hub_will_enforce() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("policy_list", json!({}));
    assert!(
        before.is_err(),
        "there are no rules before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    assert_eq!(
        shell.call("policy_list", json!({})).expect("list"),
        json!([]),
        "a fresh home configures no rules; the shipped default applies"
    );

    shell
        .call(
            "policy_add",
            json!({ "rule": {
                "action": "deny",
                "tool": "shell.exec",
                "when": null,
                "reason": "read-only account",
            }}),
        )
        .expect("policy_add");

    let rules: Vec<serde_json::Value> =
        serde_json::from_value(shell.call("policy_list", json!({})).expect("list"))
            .expect("a list the page can read");
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(rules[0]["action"], "deny");
    assert_eq!(
        rules[0]["reason"], "read-only account",
        "the reason a person wrote must reach the approval they will read"
    );

    // It is the rule the hub loads. Checked against the same loader the
    // hub boots with, so the window and the enforcement cannot disagree.
    let text = std::fs::read_to_string(home.path().join("config.toml")).expect("config");
    assert!(
        text.contains("shell.exec") && text.contains("read-only account"),
        "the rule did not reach the file the hub reads: {text}"
    );

    // A rule that stops a call without saying why is refused, not stored with
    // a blank explanation nobody can act on.
    let err = shell
        .call(
            "policy_add",
            json!({ "rule": { "action": "deny", "tool": "fs.write", "when": null, "reason": null }}),
        )
        .expect_err("a deny with no reason should be refused");
    assert!(err.contains("--reason"), "{err}");

    shell
        .call("policy_remove", json!({ "number": 1 }))
        .expect("policy_remove");
    assert_eq!(
        shell.call("policy_list", json!({})).expect("list"),
        json!([]),
        "removing the only rule should leave none"
    );
}

/// "Connected" must mean there is something to work on.
///
/// `openbot acp` reaches the hub lazily, per turn, so the handshake succeeds
/// against a hub that is wrong or down. If the window said "connected" on the
/// handshake alone, the failure would only appear when somebody sent their
/// first message, after they had written it and believed it was going
/// somewhere.
#[test]
fn connecting_says_whether_there_is_a_computer_behind_the_hub() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");

    // A real hub: connected, and it says what it serves.
    let shell = Shell::build(Mode::Reply);
    let found = shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    assert_eq!(
        found["computer"], true,
        "a live hub was reported as absent: {found}"
    );
    assert!(
        found["tools"].as_u64().is_some_and(|n| n > 0),
        "the computer serves tools; saying how many is what makes it real: {found}"
    );
    assert_eq!(found["why"], serde_json::Value::Null);

    // A hub that is not there: still connects (the roster reads without one
    // and a mistyped URL has to be fixable from inside the window) but it
    // does not claim a computer.
    let orphan = Shell::build(Mode::Reply);
    let found = orphan
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": "ws://127.0.0.1:59999/v1/tools",
            }),
        )
        .expect("connecting to a dead hub is not a failure to connect");
    assert_eq!(
        found["computer"], false,
        "a hub that refused the connection was reported as serving: {found}"
    );
    assert!(
        found["why"]
            .as_str()
            .is_some_and(|w| !w.is_empty() && !w.contains("Error:")),
        "the reason should be the cause, without the prefix: {found}"
    );

    // The window is usable: the roster reads with no computer at all.
    assert_eq!(
        orphan
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
        json!([]),
        "a missing computer should not stop the roster loading"
    );
}

/// The page asks on load so a reload lands on the right panel. Connecting
/// twice is a mistake the shell names rather than papers over.
#[test]
fn the_shell_says_whether_it_is_connected() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("connected", json!({})).expect("connected");
    assert_eq!(before, json!(false), "a fresh shell holds no engine");

    let args = json!({
        "openbot": common::up::openbot().to_str().unwrap(),
        "home": home.path().to_str().unwrap(),
        "hub": up.hub,
    });
    shell.call("connect", args.clone()).expect("connect");
    assert_eq!(
        shell.call("connected", json!({})).expect("connected"),
        json!(true)
    );

    let twice = shell.call("connect", args);
    assert!(
        twice.is_err(),
        "connecting twice would leak the first engine: {twice:?}"
    );

    shell.call("disconnect", json!({})).expect("disconnect");
    assert_eq!(
        shell.call("connected", json!({})).expect("connected"),
        json!(false),
        "disconnect must leave the shell able to say so"
    );

    // The window can connect again afterwards, which is the whole point of a
    // Disconnect button that is not "quit".
    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("reconnect");
}

/// Nothing works before connecting, and the shell says which thing is wrong.
#[test]
fn commands_that_need_an_engine_say_so_when_there_is_none() {
    let shell = Shell::build(Mode::Reply);
    for (cmd, args) in [
        ("new_session", json!({ "cwd": "/tmp/x" })),
        ("prompt", json!({ "session": "s", "text": "hi" })),
        ("cancel", json!({ "session": "s" })),
    ] {
        let out = shell.call(cmd, args);
        assert_eq!(
            out.as_ref().err().map(String::as_str),
            Some("\"not connected\""),
            "{cmd} did not say it has no engine: {out:?}"
        );
    }
}

/// The composer's `/` list crosses the IPC boundary in the shape the page
/// reads.
///
/// `page.rs` drives the real `main.js` but stubs `invoke`, so it proves the
/// menu renders a catalog and not that any catalog ever arrives. The reader's
/// own tests prove the binary's JSON parses. Only this proves the command is
/// registered and that what comes back out has the two field names the page
/// indexes into; a producer and a consumer each tested alone will both stay
/// green while nothing joins them.
#[test]
fn the_skills_a_bot_can_use_reach_the_window() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("skills", json!({}));
    assert!(
        before.is_err(),
        "there are no skills before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");

    // A fresh home. Empty in both halves rather than an error: this is the
    // state the window is in the first time anybody opens it.
    let empty = shell.call("skills", json!({})).expect("skills");
    assert_eq!(
        empty,
        json!({ "skills": [], "problems": [] }),
        "a home with no skills should answer empty, not fail: {empty}"
    );

    let out = std::process::Command::new(common::up::openbot())
        .args(["skill", "new", "refund-a-customer", "--description"])
        .arg("How to issue a refund")
        .arg("--home")
        .arg(home.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .output()
        .expect("could not run openbot");
    assert!(out.status.success(), "`openbot skill new` failed");

    let cat = shell.call("skills", json!({})).expect("skills");
    assert_eq!(
        cat,
        json!({
            "skills": [{ "name": "refund-a-customer",
                         "description": "How to issue a refund" }],
            "problems": [],
        }),
        "the page indexes `.skills` and `.problems` by name: {cat}"
    );
}

/// Editing a Bot from the window keeps the Bot.
///
/// The docs' "Edit a Bot". A rename is the interesting half: the id is what a
/// home is keyed by (conversations, inboxes, group membership, routines), so
/// the name changes and nothing else does. If the id followed the name, every
/// reference in the home would point at a Bot that is no longer there, and the
/// symptom would be a teammate that had forgotten everything.
#[test]
fn editing_a_bot_from_the_window_renames_it_without_losing_it() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    let before = shell.call("bot_describe", json!({ "bot": "nobody" }));
    assert!(
        before.is_err(),
        "there is nothing to edit before there is an agent: {before:?}"
    );

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");

    shell
        .call(
            "bot_describe",
            json!({ "bot": "Talent Scout", "title": "recruiting" }),
        )
        .expect("set a title");

    let rows: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster the page can read");
    assert_eq!(rows[0]["title"], "recruiting", "{rows:?}");

    shell
        .call(
            "bot_describe",
            json!({ "bot": "Talent Scout", "rename": "Recruiting" }),
        )
        .expect("rename");

    let rows: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster the page can read");
    assert_eq!(rows.len(), 1, "a rename made a second Bot: {rows:?}");
    assert_eq!(rows[0]["name"], "Recruiting");
    assert_eq!(
        rows[0]["id"], "talent-scout",
        "the id moved with the name, so the conversation is orphaned: {rows:?}"
    );
    // The edit that named only a title must not have cleared the rest.
    assert_eq!(
        rows[0]["title"], "recruiting",
        "renaming cleared the title: {rows:?}"
    );

    // A change that changes nothing is refused rather than reported done.
    let nothing = shell.call("bot_describe", json!({ "bot": "Recruiting" }));
    assert!(
        nothing.is_err(),
        "an empty edit reported success: {nothing:?}"
    );
}

/// A duplicate carries the brief and not the conversation.
///
/// The docs are explicit about that, and it is the part people are surprised
/// by, so it is the part pinned here. A copy made to cover a second region
/// that inherited the first one's history would answer with facts about the
/// wrong account, confidently, and look like it was working.
#[test]
fn duplicating_a_bot_copies_the_brief_and_not_the_conversation() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");
    shell
        .call(
            "bot_describe",
            json!({ "bot": "Talent Scout", "title": "recruiting",
                    "description": "never contact anyone without approval" }),
        )
        .expect("describe");

    // Give the original something to remember, so "not copied" is a claim
    // about real history rather than about two empty conversations.
    let opened = shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");
    assert!(opened["session"].is_string());

    shell
        .call(
            "bot_duplicate",
            json!({ "bot": "Talent Scout", "newName": "Talent Scout EMEA" }),
        )
        .expect("duplicate");

    let rows: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster the page can read");
    let copy = rows
        .iter()
        .find(|b| b["name"] == "Talent Scout EMEA")
        .unwrap_or_else(|| panic!("the copy is not in the roster: {rows:?}"));

    assert_eq!(
        copy["title"], "recruiting",
        "the brief was not copied: {copy}"
    );
    assert_eq!(
        copy["description"], "never contact anyone without approval",
        "the standing instruction was not copied, which is the whole point: {copy}"
    );
    assert_eq!(
        copy["messages"], 0,
        "the copy inherited a conversation it must not have: {copy}"
    );
    assert_ne!(
        copy["id"], "talent-scout",
        "the copy is its own Bot: {copy}"
    );

    // A name already taken is a refusal, not a second Bot with the same name.
    let again = shell.call(
        "bot_duplicate",
        json!({ "bot": "Talent Scout", "newName": "Talent Scout EMEA" }),
    );
    assert!(again.is_err(), "a duplicate name was accepted: {again:?}");
}

/// Deleting from the window removes the Bot and leaves the home working.
///
/// The store already refuses to leave a group naming a Bot that is gone; this
/// is the join: the window's Delete reaches that behaviour rather than some
/// other deletion.
#[test]
fn deleting_a_bot_from_the_window_leaves_its_group_working() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");
    shell
        .call("open_bot", json!({ "name": "Writer" }))
        .expect("open_bot");
    let out = std::process::Command::new(common::up::openbot())
        .args(["group", "new", "Launch", "--members", "talent-scout,writer"])
        .arg("--home")
        .arg(home.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .output()
        .expect("could not run openbot");
    assert!(out.status.success(), "`openbot group new` failed");

    shell
        .call("bot_delete", json!({ "bot": "talent-scout" }))
        .expect("delete");

    let rows: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": true }))
            .expect("roster"),
    )
    .expect("a roster the page can read");
    assert!(
        !rows.iter().any(|b| b["id"] == "talent-scout"),
        "the Bot is still listed: {rows:?}"
    );

    let groups: Vec<serde_json::Value> =
        serde_json::from_value(shell.call("groups", json!({})).expect("groups"))
            .expect("groups the page can read");
    let members = groups[0]["members"].as_array().expect("members");
    assert!(
        !members.iter().any(|m| m["id"] == "talent-scout"),
        "the group still holds a Bot that does not exist: {groups:?}"
    );
    assert_eq!(members.len(), 1, "{groups:?}");

    // Deleting one that is not there is an error, not a quiet success.
    let again = shell.call("bot_delete", json!({ "bot": "talent-scout" }));
    assert!(
        again.is_err(),
        "deleting nothing reported success: {again:?}"
    );
}

/// The home the window offers is a real path, not shell syntax.
///
/// This is the one default that intentionally does not match the binary's.
/// openbot's own is `./openbot-data`, relative to wherever it was run: right
/// for a terminal, wrong for a window, because an installed application's
/// working directory is not a place anybody chose and Bots landing in it land
/// at random.
///
/// What it must never be is a tilde. Nothing expands one on the way to a
/// subprocess, and `openbot bot --home '~/.openbot' ls` quietly creates a
/// folder called `~` for everybody who does not type a path.
#[test]
fn the_default_home_is_a_path_and_not_a_tilde() {
    let shell = Shell::build(Mode::Reply);
    let home: String = serde_json::from_value(
        shell
            .call("default_home", json!({}))
            .expect("a default home, connected or not"),
    )
    .expect("a string");

    assert!(
        !home.contains('~'),
        "the window offers `{home}`, which openbot would take literally"
    );
    let path = std::path::Path::new(&home);
    assert!(
        path.is_absolute(),
        "`{home}` is relative, so where the Bots land depends on how the app was started"
    );
    assert!(
        home.ends_with(".openbot"),
        "the default should be somewhere recognisable: {home}"
    );

    // It is somewhere openbot can actually be pointed at. Not created here
    // (asking for a default must not write to a person's disk), only that
    // the parent it would go in is real.
    assert!(
        path.parent().is_some_and(std::path::Path::exists),
        "`{home}` is under a directory that does not exist"
    );
}

/// A second prompt must not sweep away the question the first is blocked on.
///
/// The turn-end sweep refuses every approval still parked for a session, which
/// is right when the turn is over and catastrophic if it can fire while one is
/// still waiting: a live question would vanish mid-turn, and the person would
/// watch the Bot stall with nothing on screen to explain it.
///
/// Nothing on the shell side refuses a second `prompt` for one session:
/// `prompt_start` only sends a command, and the page's `busy` flag is page
/// state, not a lock. What makes this safe is one layer down: `openbot acp`
/// keeps one turn per session and a prompt arriving mid-turn joins it, so
/// both calls are answered by the same ending and neither sweeps early. This
/// pins that, because the sweep depends on it.
#[test]
fn a_second_prompt_does_not_withdraw_the_first_turns_question() {
    let (_up, _home, shell, session) = connected(Mode::Tools, "/tmp/openbot-join");

    let first = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "go" }),
            )
        }
    });

    let ask = wait_for(
        || shell.asks.lock().unwrap().first().cloned(),
        Duration::from_secs(120),
    )
    .expect("the agent never asked");
    assert!(!first.is_finished(), "the turn should still be waiting");

    // A second prompt on the same session, while the first is parked.
    let second = std::thread::spawn({
        let webview = shell.webview.clone();
        let session = session.clone();
        move || {
            call(
                &webview,
                "prompt",
                json!({ "session": session, "text": "and also this" }),
            )
        }
    });

    // Give it room to do damage if it is going to: the sweep it would run is
    // synchronous once its turn returns.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        !first.is_finished(),
        "the first turn ended when a second prompt arrived; it should have \
         joined it, not replaced it"
    );

    // The question is still answerable. If a sweep had taken it, this is the
    // error the person would have got for clicking a button that was on their
    // screen.
    shell
        .call(
            "answer_permission",
            json!({ "id": ask.id, "optionId": "allow-once" }),
        )
        .expect("the question the turn is blocked on was withdrawn under it");

    // Let both finish, answering whatever else the scripted run asks.
    for handle in [first, second] {
        while !handle.is_finished() {
            let waiting: Vec<GotAsk> = shell.asks.lock().unwrap().drain(..).collect();
            for ask in waiting {
                let _ = shell.call(
                    "answer_permission",
                    json!({ "id": ask.id, "optionId": "allow-once" }),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.join().unwrap().expect("the turn failed");
    }
}

/// Hiding from the window is the same act `openbot bot hide` performs: the Bot
/// leaves the list and keeps everything else.
#[test]
fn hiding_a_bot_from_the_window_keeps_its_work() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");

    shell
        .call("bot_hide", json!({ "bot": "talent-scout", "hidden": true }))
        .expect("hide");

    let visible: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster");
    assert!(
        visible.is_empty(),
        "a hidden Bot is still in the ordinary list: {visible:?}"
    );

    // Hidden is not gone: "Show hidden chats" must find it, with its work.
    let all: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": true }))
            .expect("roster"),
    )
    .expect("a roster");
    assert_eq!(all.len(), 1, "hiding deleted the Bot: {all:?}");
    assert_eq!(all[0]["hidden"], true);

    shell
        .call(
            "bot_hide",
            json!({ "bot": "talent-scout", "hidden": false }),
        )
        .expect("unhide");
    let back: Vec<serde_json::Value> = serde_json::from_value(
        shell
            .call("roster", json!({ "hidden": false }))
            .expect("roster"),
    )
    .expect("a roster");
    assert_eq!(back.len(), 1, "unhiding did not bring it back: {back:?}");
}

/// Pausing from the window is the same act `openbot routine pause` performs, and
/// the routine keeps its definition either way.
#[test]
fn a_routine_paused_from_the_window_stops_without_being_lost() {
    let up = Up::start().expect("openbot up");
    let home = tempfile::tempdir().expect("a home for the agent's bots");
    let shell = Shell::build(Mode::Reply);

    shell
        .call(
            "connect",
            json!({
                "openbot": common::up::openbot().to_str().unwrap(),
                "home": home.path().to_str().unwrap(),
                "hub": up.hub,
            }),
        )
        .expect("connect");
    shell
        .call("open_bot", json!({ "name": "Talent Scout" }))
        .expect("open_bot");

    let out = std::process::Command::new(common::up::openbot())
        .args([
            "routine",
            "new",
            "talent-scout",
            "morning",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "check the pipeline",
        ])
        .arg("--home")
        .arg(home.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .output()
        .expect("could not run openbot");
    assert!(out.status.success(), "`openbot routine new` failed");

    let running: Vec<serde_json::Value> =
        serde_json::from_value(shell.call("routines", json!({})).expect("routines"))
            .expect("routines");
    assert_eq!(running[0]["enabled"], true, "{running:?}");

    shell
        .call(
            "routine_pause",
            json!({ "bot": "talent-scout", "routine": "morning", "paused": true }),
        )
        .expect("pause");

    let paused: Vec<serde_json::Value> =
        serde_json::from_value(shell.call("routines", json!({})).expect("routines"))
            .expect("routines");
    assert_eq!(paused.len(), 1, "pausing deleted the routine: {paused:?}");
    assert_eq!(paused[0]["enabled"], false);
    assert_eq!(
        paused[0]["trigger"], "every day at 9:00",
        "the definition did not survive: {paused:?}"
    );

    shell
        .call(
            "routine_pause",
            json!({ "bot": "talent-scout", "routine": "morning", "paused": false }),
        )
        .expect("resume");
    let back: Vec<serde_json::Value> =
        serde_json::from_value(shell.call("routines", json!({})).expect("routines"))
            .expect("routines");
    assert_eq!(back[0]["enabled"], true, "{back:?}");

    // A routine that is not there is an error, not a quiet success.
    let missing = shell.call(
        "routine_pause",
        json!({ "bot": "talent-scout", "routine": "nope", "paused": true }),
    );
    assert!(missing.is_err(), "pausing nothing reported success");
}
