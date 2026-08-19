//! `openbot acp` against a real client's bytes.
//!
//! Not a unit test of the mapping; that lives beside the mapping. This drives
//! the actual binary the way an editor does: spawn it, write JSON-RPC on its
//! stdin, read JSON-RPC off its stdout. Everything in between is the part that
//! cannot be checked any other way: that the framing matches, that stdout
//! carries protocol and nothing else, and that a session really does reach a
//! Bot on disk.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A running `openbot acp`, with its pipes.
struct Acp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
}

impl Drop for Acp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Acp {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openbot"))
            .args(["acp", "--demo", "--home"])
            .arg(home)
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start `openbot acp`");
        let stdin = child.stdin.take().expect("piped");
        let out = BufReader::new(child.stdout.take().expect("piped"));
        Self { child, stdin, out }
    }

    /// Send one request and read one line back.
    fn call(&mut self, line: &str) -> serde_json::Value {
        writeln!(self.stdin, "{line}").expect("the agent closed its stdin");
        self.stdin.flush().expect("flush");
        let mut buf = String::new();
        self.out
            .read_line(&mut buf)
            .expect("the agent closed its stdout");
        assert!(
            !buf.trim().is_empty(),
            "the agent sent a blank line where a response belonged"
        );
        serde_json::from_str(&buf).unwrap_or_else(|e| {
            panic!("stdout carried something that is not JSON-RPC: {e}\nline was: {buf}")
        })
    }

    fn initialize(&mut self) -> serde_json::Value {
        self.call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
        )
    }

    /// Send one request and read until its answer, keeping what arrived first.
    ///
    /// Notifications share the stream with responses, so [`call`], which
    /// reads exactly one line, returns whichever came first. A test written
    /// on top of it either asserts about a notification while believing it has
    /// the response, or never sees the notification at all. Everything before
    /// the matching id comes back too, in order.
    ///
    /// [`call`]: Self::call
    fn call_watching(
        &mut self,
        line: &str,
        id: i64,
    ) -> (serde_json::Value, Vec<serde_json::Value>) {
        writeln!(self.stdin, "{line}").expect("the agent closed its stdin");
        self.stdin.flush().expect("flush");
        let mut before = Vec::new();
        for _ in 0..64 {
            let mut buf = String::new();
            self.out
                .read_line(&mut buf)
                .expect("the agent closed its stdout");
            let msg: serde_json::Value = serde_json::from_str(&buf).unwrap_or_else(|e| {
                panic!("stdout carried something that is not JSON-RPC: {e}\nline was: {buf}")
            });
            if msg["id"] == id {
                return (msg, before);
            }
            before.push(msg);
        }
        panic!("no response to id {id} in 64 lines; saw {before:#?}");
    }
}

/// Every notification that is a `session/update` of one kind.
fn updates_of<'a>(lines: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    lines
        .iter()
        .filter(|m| m["method"] == "session/update")
        .map(|m| &m["params"]["update"])
        .filter(|u| u["sessionUpdate"] == kind)
        .collect()
}

#[test]
fn an_editor_can_initialize_and_open_a_session() {
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());

    let init = acp.initialize();
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(
        init["result"]["protocolVersion"], 1,
        "the version on the wire is the integer 1; anything else fails negotiation"
    );

    // `loadSession` is implemented: a Bot's conversation is the durable thing
    // here, so a client that reconnects must be able to get it back rather
    // than showing an empty transcript beside a Bot that remembers
    // everything. Claiming a capability that is not implemented leaves a
    // client waiting on a method that never answers; withdrawing one that is
    // implemented leaves it showing a blank window. Both are wrong, so this
    // is pinned.
    assert_eq!(
        init["result"]["agentCapabilities"]["loadSession"], true,
        "openbot stopped offering session loading, so no client can show a conversation"
    );

    let new = acp.call(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp/Payments API","mcpServers":[]}}"#,
    );
    let session = new["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no session id in {new}"));
    assert!(
        session.starts_with("openbot-"),
        "a session id should say whose it is, got {session}"
    );

    // The session is bound to a Bot named for the directory, and that Bot is
    // on disk. A session that resolved to nothing would answer exactly the
    // same way up to here.
    let bots = openbot_bots::BotStore::open(home.path()).unwrap();
    let names: Vec<String> = bots
        .list(true)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "payments-api"),
        "expected a Bot named for the working directory, found {names:?}"
    );
}

#[test]
fn opening_the_same_directory_twice_reaches_the_same_bot() {
    // A project keeps its Bot. Two sessions on one directory must not produce
    // two Bots, or every reopen starts a new one with no memory of the last.
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());
    acp.initialize();

    let a = acp.call(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp/ledger","mcpServers":[]}}"#,
    );
    let b = acp.call(
        r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/tmp/ledger","mcpServers":[]}}"#,
    );

    // Both must actually succeed. Without this the rest of the test can pass
    // for the wrong reason: if Bot reuse were removed, the second call would
    // fail with "a bot named `ledger` already exists", `assert_ne!` would
    // compare a string against null (unequal, therefore green), and the Bot
    // count would stay at one because creation had failed rather than
    // because anything was reused.
    for (which, res) in [("first", &a), ("second", &b)] {
        assert!(
            res["result"]["sessionId"].is_string(),
            "the {which} session/new did not succeed: {res}"
        );
    }
    assert_ne!(
        a["result"]["sessionId"], b["result"]["sessionId"],
        "two sessions should be distinct even when they share a Bot"
    );

    let bots = openbot_bots::BotStore::open(home.path()).unwrap();
    let ledger: Vec<_> = bots
        .list(true)
        .unwrap()
        .into_iter()
        .filter(|b| b.name == "ledger")
        .collect();
    assert_eq!(
        ledger.len(),
        1,
        "reopening a directory made a second Bot; the first one's memory is now unreachable"
    );
}

#[test]
fn prompting_a_session_that_was_never_opened_is_an_error_not_a_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());
    acp.initialize();

    let res = acp.call(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{"sessionId":"openbot-nope","prompt":[{"type":"text","text":"hello"}]}}"#,
    );
    assert!(
        res["error"].is_object(),
        "an unknown session answered as though it had run: {res}"
    );
    assert!(
        res["result"].is_null(),
        "an unknown session must not produce a stop reason"
    );
}

/// A block OPENBOT cannot read is reported on the wire before the turn runs,
/// not left for the person to infer from an answer that never mentions their
/// screenshot.
///
/// End-to-end because that is where it can go wrong: the notice is built in
/// `prompt_text` and sent at the handler, and a unit test of the first cannot
/// tell whether the second happens. Deleting the send leaves every unit test
/// green.
#[test]
fn an_image_openbot_cannot_read_is_reported_before_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());
    acp.initialize();
    let new = acp.call(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp/ledger","mcpServers":[]}}"#,
    );
    let session = new["result"]["sessionId"].as_str().expect("a session");

    let (res, before) = acp.call_watching(
        &format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"is this right?"}},{{"type":"image","data":"iVBOR","mimeType":"image/png"}}]}}}}"#
        ),
        3,
    );
    // What the turn then does is not this test's subject: it needs a hub,
    // and there is none here, so it fails on connect. The notice is sent
    // before any of that, so a person learns their image went unread whether
    // the turn succeeds, fails or is cancelled.
    assert_eq!(res["id"], 3, "the prompt was answered by something: {res}");

    // As the person's own message, because that is how it comes back on
    // `session/load`; the transcript must read the same live and on reopen.
    let said: Vec<String> = updates_of(&before, "user_message_chunk")
        .iter()
        .filter_map(|u| u["content"]["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        said.iter().any(|t| t.contains("1 image")),
        "the image was dropped without a word; user chunks were {said:?}"
    );
}

/// The doc comment on `prompt_text` leans on this: image, audio and embedded
/// context are declined because OPENBOT says so during the handshake. The
/// struct is `#[non_exhaustive]` and every field defaults to `false`, so a
/// schema bump could add one that defaults the other way and silently turn a
/// documented refusal into an undocumented claim. Assert the wire, not the
/// type.
#[test]
fn the_handshake_promises_only_what_openbot_can_read() {
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());
    let init = acp.initialize();
    let caps = &init["result"]["agentCapabilities"]["promptCapabilities"];
    for claimed in ["image", "audio", "embeddedContext"] {
        assert_eq!(
            caps[claimed], false,
            "openbot advertises {claimed} and then drops it: {caps}"
        );
    }
}

/// stdout is the protocol. A stray `println!` anywhere in the startup path
/// corrupts the stream, and the symptom is a client that cannot parse the
/// handshake, a long way from the cause.
#[test]
fn nothing_but_protocol_is_written_to_stdout() {
    let home = tempfile::tempdir().unwrap();
    let mut acp = Acp::start(home.path());
    let init = acp.initialize();
    // `call` already fails if the first line is not JSON, so reaching here
    // with a matching id proves the response was the first thing written.
    assert_eq!(init["id"], 1, "something was printed before the response");
}
