//! The commands that need a running system, driven as shipped.
//!
//! Some defects are not library problems: a relative browser profile path
//! handed to Chrome, or a quickstart demo that nothing runs. Library tests
//! cannot see them; these can.
//!
//! Starts the real `botroster up` on an ephemeral port, reads the port out of its
//! own banner, and drives the other commands against it.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BOTROSTER: &str = env!("CARGO_BIN_EXE_botroster");

mod common;

use common::up::{wait_for_hub_url, Up};

#[test]
fn up_serves_a_computer_that_the_other_commands_can_reach() {
    let up = Up::start().expect("botroster up");

    // The guest is registered: `up` does not print its banner until it is.
    let servers = up.ok(&["servers"]);
    assert!(
        servers.contains("botroster-workspace"),
        "no computer registered:\n{servers}"
    );

    // The catalogue reaches a client, with tools from the guest and the hub.
    let tools = up.ok(&["tools"]);
    for expected in ["fs.read", "shell.exec", "bot.send"] {
        assert!(tools.contains(expected), "`{expected}` missing:\n{tools}");
    }
}

#[test]
fn the_quickstart_demo_is_the_one_in_the_readme() {
    let up = Up::start().expect("botroster up");

    // Character for character what `botroster up` prints as the next command,
    // and what the README's "Try it" says.
    let out = up.ok(&["run", "--demo", "--approve", "auto", "prove it"]);
    assert!(out.contains("completed"), "the demo did not finish:\n{out}");
    assert!(out.contains("fs.write"), "{out}");
    assert!(out.contains("shell.exec"), "{out}");
}

#[test]
fn a_direct_tool_call_goes_through_the_hub() {
    let up = Up::start().expect("botroster up");

    up.ok(&[
        "call",
        "fs.write",
        r#"{"path":"note.md","contents":"from the cli test"}"#,
        "--approve",
        "auto",
    ]);
    let read = up.ok(&[
        "call",
        "fs.read",
        r#"{"path":"note.md"}"#,
        "--approve",
        "auto",
    ]);
    assert!(read.contains("from the cli test"), "{read}");
}

#[test]
fn denying_an_approval_means_nothing_happened() {
    let up = Up::start().expect("botroster up");

    // The gate is the product's central claim, so it is checked against the
    // shipped binary rather than only in the library.
    let out = up.run(&[
        "call",
        "fs.write",
        r#"{"path":"denied.md","contents":"should not exist"}"#,
        "--approve",
        "deny",
    ]);
    assert!(!out.status.success(), "a denied write reported success");

    let listed = up.ok(&["call", "fs.list", "{}", "--approve", "auto"]);
    assert!(
        !listed.contains("denied.md"),
        "a denied write still landed:\n{listed}"
    );
}

#[test]
fn a_bot_handoff_made_by_a_tool_call_reaches_the_inbox() {
    let up = Up::start().expect("botroster up");
    let home = up._dir.path().join("control-plane");

    // `bot.send` is served by the hub so it passes the approval gate. Driving
    // it through `botroster call` exercises that path exactly as a model would.
    let mk = |name: &str| {
        Command::new(BOTROSTER)
            .args(["bot", "--home"])
            .arg(&home)
            .args(["new", name])
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };
    assert!(mk("Researcher").status.success());
    assert!(mk("Writer").status.success());

    up.ok(&[
        "call",
        "bot.send",
        r#"{"to":"Writer","message":"sources are in refs/"}"#,
        "--approve",
        "auto",
        "--no-server",
    ]);

    let inbox = Command::new(BOTROSTER)
        .args(["bot", "--home"])
        .arg(&home)
        .args(["inbox", "Writer"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&inbox.stdout);
    assert!(text.contains("sources are in refs/"), "{text}");
}

/// Serve one page on loopback for as long as the process lives.
fn serve_a_page() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut s = stream;
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body = b"<html><head><title>botroster cli test</title></head>\
                             <body><h1>it opened</h1></body></html>";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            });
        }
    });
    format!("http://{addr}/")
}

/// The browser must work under a relative `--workspace`.
///
/// `botroster up` computes its browser profile from the workspace, and Chrome
/// silently refuses a relative `--user-data-dir`; a relative profile makes
/// every browser tool fail with "the browser never published a debugging
/// port" while the whole library suite stays green. Nothing here mocks
/// anything: a real page, a real Chrome, started by the shipped binary in a
/// relative directory.
#[test]
fn the_browser_works_when_up_is_run_from_a_relative_directory() {
    if botroster_guest::browser::find_browser().is_none() {
        assert!(
            std::env::var_os("BOTROSTER_REQUIRE_BROWSER").is_none(),
            "BOTROSTER_REQUIRE_BROWSER is set but no browser was found"
        );
        eprintln!("SKIP: no Chromium-family browser installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    // Relative paths, from inside a temp directory: exactly the shape a person
    // gets by typing `botroster up` in a project folder.
    let mut child = Command::new(BOTROSTER)
        .args([
            "up",
            "--bind",
            "127.0.0.1:0",
            "--home",
            "./cp",
            "--workspace",
            "./computer",
        ])
        .current_dir(dir.path())
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HUB_URL")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_WORKSPACE")
        .stdout(Stdio::from(
            std::fs::File::create(dir.path().join("up.log")).unwrap(),
        ))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let hub = wait_for_hub_url(&dir.path().join("up.log"), &mut child);

    let url = serve_a_page();
    let out = Command::new(BOTROSTER)
        .args([
            "call",
            "browser.open",
            &format!(r#"{{"url":"{url}"}}"#),
            "--approve",
            "auto",
        ])
        .env("BOTROSTER_HUB_URL", &hub)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    // This is the one test here that opens a real page, so it is the one that
    // leaves a browser behind if the guest is merely killed.
    common::stop(&mut child);

    assert!(
        out.status.success(),
        "browser.open failed under `botroster up`: {stderr}{stdout}"
    );
    assert!(
        stdout.contains("botroster cli test"),
        "the page did not load: {stdout}"
    );
}

#[test]
fn the_computers_files_are_on_the_durable_volume() {
    // A computer's work compounds instead of resetting, and the storage layer
    // exists to deliver that. If `botroster up` worked in a plain directory
    // while `snapshot`, `restore` and `prune` operated on
    // `<home>/volumes/<id>/current`, the two would never meet and `botroster
    // computer status` beside a busy agent would report 0 files.
    //
    // So: start `up` with no `--workspace`, have the agent write a file, and
    // require that a snapshot taken from the outside actually contains it.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("control-plane");
    let log = dir.path().join("up.log");
    let mut child = Command::new(BOTROSTER)
        .arg("up")
        .args(["--bind", "127.0.0.1:0", "--home"])
        .arg(&home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HUB_URL")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_WORKSPACE")
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start botroster up");
    let hub = wait_for_hub_url(&log, &mut child);

    let wrote = Command::new(BOTROSTER)
        .args([
            "call",
            "fs.write",
            r#"{"path":"work.md","contents":"a week of work"}"#,
            "--approve",
            "auto",
        ])
        .env("BOTROSTER_HUB_URL", &hub)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        wrote.status.success(),
        "{}",
        String::from_utf8_lossy(&wrote.stderr)
    );

    // From the outside, as an operator would, while the guest is still up.
    let snap = Command::new(BOTROSTER)
        .args(["computer", "snapshot", "from the test", "--store"])
        .arg(&home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_STORE")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&snap.stdout).to_string();
    assert!(
        snap.status.success(),
        "{}",
        String::from_utf8_lossy(&snap.stderr)
    );
    assert!(
        said.contains("1 files"),
        "the snapshot did not capture the agent's work -- the computer is not on the volume: {said}"
    );

    // The volume is held while a guest is on it, so nothing rolls the
    // directory out from under it.
    let restore = Command::new(BOTROSTER)
        .args(["computer", "restore", "snap-000000", "--store"])
        .arg(&home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_STORE")
        .output()
        .unwrap();
    assert!(
        !restore.status.success(),
        "a restore ran under a live guest"
    );
    let err = String::from_utf8_lossy(&restore.stderr);
    assert!(err.contains("guest is running"), "{err}");

    common::stop(&mut child);
}

/// An ACP client, driven against the live stack.
///
/// Reads on a thread so a wedged agent fails on a timeout instead of hanging
/// `cargo test` forever. The reason `session/prompt` spawns off the SDK's
/// event loop is that awaiting a turn inline deadlocks against the very
/// approval reply the turn is waiting for; a regression there would hang, and
/// a hang reports nothing.
struct AcpClient {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    _dir: tempfile::TempDir,
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AcpClient {
    fn start(hub: &str) -> Self {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(BOTROSTER)
            .args(["acp", "--demo", "--home"])
            .arg(dir.path())
            .env("BOTROSTER_HUB_URL", hub)
            .env("NO_COLOR", "1")
            .env_remove("BOTROSTER_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start `botroster acp`");
        let stdin = child.stdin.take().expect("piped");
        let out = child.stdout.take().expect("piped");
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            _dir: dir,
        }
    }

    fn send(&mut self, line: &str) {
        use std::io::Write;
        writeln!(self.stdin, "{line}").expect("the agent closed its stdin");
        self.stdin.flush().expect("flush");
    }

    fn next(&self) -> serde_json::Value {
        let line = self.lines.recv_timeout(Duration::from_secs(90)).expect(
            "the agent went quiet; a turn that never answers is the deadlock this design avoids",
        );
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC: {e}\n{line}"))
    }

    /// Read until the response to `id`, answering anything the agent asks on
    /// the way and keeping every notification it sent.
    fn pump_until(&mut self, id: i64) -> (serde_json::Value, Vec<serde_json::Value>) {
        let mut updates = Vec::new();
        loop {
            let msg = self.next();
            if msg["id"] == serde_json::json!(id)
                && (!msg["result"].is_null() || !msg["error"].is_null())
            {
                return (msg, updates);
            }
            match msg["method"].as_str() {
                // The agent is asking a human. Say yes, the way a person
                // clicking "Allow once" would, using the id BOTROSTER itself
                // offered, so a rename on either side fails here.
                Some("session/request_permission") => {
                    let rid = msg["id"].clone();
                    let opt = msg["params"]["options"]
                        .as_array()
                        .and_then(|o| o.first())
                        .and_then(|o| o["optionId"].as_str())
                        .unwrap_or("allow-once")
                        .to_owned();
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0", "id": rid,
                        "result": {"outcome": {"outcome": "selected", "optionId": opt}}
                    });
                    self.send(&reply.to_string());
                }
                Some("session/update") => updates.push(msg),
                _ => {}
            }
        }
    }
}

/// An editor asks, a Bot works, and the editor watches it happen.
#[test]
fn an_acp_client_can_drive_a_whole_turn() {
    let up = Up::start().expect("botroster up");
    let mut acp = AcpClient::start(&up.hub);

    acp.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#);
    let (init, _) = acp.pump_until(1);
    assert_eq!(init["result"]["protocolVersion"], 1);

    acp.send(r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp/acp-turn","mcpServers":[]}}"#);
    let (new, _) = acp.pump_until(2);
    let session = new["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_owned();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "prove it"}]}
    });
    acp.send(&prompt.to_string());
    let (res, updates) = acp.pump_until(3);

    assert!(res["error"].is_null(), "the turn failed: {}", res["error"]);
    assert_eq!(
        res["result"]["stopReason"], "end_turn",
        "the demo script completes, so anything else is a real difference: {res}"
    );

    // Streaming is the difference between an adapter and a batch job. These
    // arrived before the response above, which is the property being
    // asserted; an implementation that buffered the turn and answered at the
    // end would satisfy every other line in this test.
    assert!(
        !updates.is_empty(),
        "the client watched a whole turn and was told nothing until it ended"
    );
    let kinds: Vec<&str> = updates
        .iter()
        .filter_map(|u| u["params"]["update"]["sessionUpdate"].as_str())
        .collect();
    // Text, because `--demo` replays a single scripted reply and calls no
    // tools (`config::build` returns `Scripted::builder().say(fallback)`), so
    // asserting `tool_call` here would fail on that assumption rather than on
    // a defect. Tool-call rendering is covered in the `update_for` unit
    // tests; reaching it from here needs the richer script `botroster run
    // --demo` uses.
    assert!(
        kinds.contains(&"agent_message_chunk"),
        "the model spoke and the client heard nothing: {kinds:?}"
    );

    // `Up` kills the stack when it drops; nothing to do by hand.
}

/// A group turn goes to the group's thread, and not into the member's own.
///
/// `botroster group post` shares `run_task` with every other turn, and the
/// single thing that makes it different is `Thread::Group`: seeded from the
/// thread, appended back to the thread.
///
/// Both halves are asserted, because only checking the first would pass just
/// as well if a group turn also filled up the member's private conversation,
/// and a Bot accumulating a conversation it took one turn of is how its own
/// context stops meaning anything.
#[test]
fn a_group_turn_lands_in_the_thread_and_not_in_the_members_own_log() {
    let up = Up::start().expect("botroster up");

    up.ok(&["bot", "new", "Researcher"]);
    up.ok(&["bot", "new", "Writer"]);
    up.ok(&["group", "new", "Launch", "--members", "researcher,writer"]);

    let out = up.ok(&[
        "group",
        "post",
        "Launch",
        "@writer draft the announcement",
        "--demo",
        "--approve",
        "auto",
    ]);
    assert!(
        out.contains("completed"),
        "the group turn did not finish:\n{out}"
    );

    // The thread has the exchange, question then answer.
    let thread = up.ok(&["group", "log", "Launch", "--json"]);
    let msgs: Vec<serde_json::Value> =
        serde_json::from_str(thread.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\n{thread}"));
    assert_eq!(msgs.len(), 2, "expected the ask and the reply: {thread}");
    assert_eq!(msgs[0]["role"], "user");
    assert!(
        msgs[0]["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("draft the announcement")),
        "what was asked is missing from the thread: {thread}"
    );
    assert_eq!(msgs[1]["role"], "assistant", "{thread}");

    // The member who answered kept its own conversation to itself.
    let own = up.ok(&["bot", "log", "Writer"]);
    assert!(
        own.contains("has not been given anything yet"),
        "a group turn filled the member's own log: {own}"
    );

    // The mention picked the owner. Nobody mentioned means the coordinator,
    // and this named the second member specifically.
    assert!(
        out.contains("writer"),
        "the mentioned member should have answered: {out}"
    );
}

/// A mention of somebody outside the group is refused rather than quietly
/// handed to the coordinator. Silently reassigning it would produce a reply
/// from the wrong Bot with nothing on screen saying so.
#[test]
fn posting_to_a_group_with_an_unknown_mention_is_refused() {
    let up = Up::start().expect("botroster up");
    up.ok(&["bot", "new", "Fact Checker"]);
    up.ok(&["bot", "new", "Copy Editor"]);
    // Two members, because the store enforces "two to six" and refuses one.
    up.ok(&[
        "group",
        "new",
        "Review",
        "--members",
        "fact-checker,copy-editor",
    ]);

    let out = up.run(&["group", "post", "Review", "@nobody do it", "--demo"]);
    assert!(
        !out.status.success(),
        "a mention of a non-member was accepted"
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("not in") && text.contains("fact-checker"),
        "the refusal should name who is actually in the group: {text}"
    );
}

/// Every tool the computer offers has a considered verdict in the shipped
/// policy.
///
/// The fallback is `RequireApproval`, so a tool nobody wrote a rule for is
/// safe, but its approval card reads "no rule covers `browser.type`; asking
/// to be safe", which tells a person nothing about what they are approving.
/// A card is only a decision if it says what will happen.
///
/// The two lists live in different crates that must not see each other:
/// `botroster-guest` may never reach `botrosterd` (`crates/botroster-guest/tests/
/// isolation.rs`), so the catalogue is read the only way both are visible at
/// once: over the wire from a running computer, which is also the list a real
/// Bot is offered.
#[test]
fn every_tool_the_computer_offers_has_a_rule_of_its_own() {
    let up = Up::start().expect("botroster up");
    let listed = up.ok(&["tools"]);

    let policy = botrosterd::policy::Policy::default();
    let mut unnamed = Vec::new();
    for line in listed.lines() {
        let Some(tool) = line.split_whitespace().next() else {
            continue;
        };
        if !tool.contains('.') {
            continue;
        }
        // A rule that names it produces a verdict carrying that rule's own
        // reason; the fallback produces the "no rule covers" sentence. Asking
        // the policy is better than parsing its rules: this is the answer a
        // person would actually be shown.
        if let botrosterd::policy::Verdict::Ask(why) = policy.evaluate(tool, &serde_json::json!({}))
        {
            if why.contains("no rule covers") {
                unnamed.push(tool.to_owned());
            }
        }
    }

    assert!(
        !listed.is_empty() && listed.contains("fs.read"),
        "no catalogue was read, so this proves nothing:\n{listed}"
    );
    assert!(
        unnamed.is_empty(),
        "these are offered to a Bot and have no rule, so approving one shows \
         `no rule covers …` instead of what it does: {unnamed:?}"
    );
}

/// Every namespace something already serves is a namespace a connector
/// cannot take.
///
/// `check_id` refuses a connector id in `RESERVED` and explains why: the guest
/// serves `fs.*`, a model sees both a guest tool and a connector tool as
/// `<id>__<tool>` because vendors do not allow dots in a function name, and two
/// tools with one wire name means a call can reach the wrong system.
///
/// `RESERVED` is a hand-written list (`fs`, `shell` and `browser` from the
/// guest, `bot` from the hub, `botroster` held back), and this is what keeps it
/// joined to the catalogue when a new namespace appears. Read over the wire
/// from a running computer for the same reason the policy join is:
/// `botroster-guest` may never reach `botrosterd`, so the catalogue is the only
/// place both halves are visible at once.
#[test]
fn every_namespace_served_is_one_a_connector_cannot_claim() {
    let up = Up::start().expect("botroster up");
    let listed = up.ok(&["tools"]);

    let mut prefixes: Vec<String> = Vec::new();
    for line in listed.lines() {
        let Some(tool) = line.split_whitespace().next() else {
            continue;
        };
        let Some((prefix, _)) = tool.split_once('.') else {
            continue;
        };
        if !prefixes.iter().any(|p| p == prefix) {
            prefixes.push(prefix.to_owned());
        }
    }

    // The catalogue really did arrive: an empty one would make every
    // assertion below true and mean nothing.
    assert!(
        prefixes.len() >= 3,
        "only {} namespaces came back from a running computer ({prefixes:?}), so this \
         checked almost nothing",
        prefixes.len()
    );

    let unreserved: Vec<&String> = prefixes
        .iter()
        .filter(|p| !botrosterd::connector::RESERVED.contains(&p.as_str()))
        .collect();
    assert!(
        unreserved.is_empty(),
        "these namespaces are served but not reserved, so a connector may still be named \
         after one and a model would see two tools with a single wire name: {unreserved:?}"
    );
}

/// The banner says whether this hub is the routine scheduler.
///
/// Covers the call site rather than the renderer. `up::routines_line` is unit
/// tested for both states, and deleting the one line in `banner` that calls it
/// left that test green — the mutation was invisible from inside the module.
#[test]
fn the_banner_says_whether_this_hub_is_checking_routines() {
    let Some(up) = common::up::Up::start() else {
        return;
    };
    let banner = std::fs::read_to_string(&up.log).unwrap_or_default();
    assert!(
        banner.contains("routines"),
        "`up` announced a hub without saying whether routines are checked here. A person \
         whose routines never fire and a person who turned the scheduler off see the same \
         nothing; the banner is what separates them.\n{banner}"
    );
}

/// A routine that is due actually runs, with nobody typing anything.
///
/// This is the regression test for a feature that was complete and inert. The
/// cron parsed, `due` computed the right answer and `tick` executed correctly,
/// and nothing called `tick`: `up.rs` did not contain the word "routine". The
/// desktop client starts the runtime with `botroster up`, so it was every desktop
/// user, and the symptom was silence.
///
/// Slow because it is real, in the way `CLAUDE.md` means: it starts the shipped
/// binary and waits for a wall-clock timer. No model is configured, so the run
/// is recorded as a failure — which is the point. **A recorded run of any kind
/// proves the clock ticked**, and that is the property that did not exist.
/// Asserting on success would need a model and would test the agent instead.
#[test]
fn a_due_routine_runs_without_anyone_asking() {
    let Some(up) = common::up::Up::start() else {
        return;
    };
    up.ok(&["bot", "new", "tester"]);
    up.ok(&[
        "routine",
        "new",
        "tester",
        "digest",
        "--cron",
        "* * * * *",
        "--instructions",
        "say hi",
    ]);
    assert!(
        up.ok(&["routine", "show", "tester", "digest"])
            .contains("never run"),
        "the routine should not have run before the timer reached it; if it already had, \
         this test cannot tell a working scheduler from a broken one"
    );

    // The timer's default interval is one minute, so this waits just over one.
    // Polling rather than sleeping the whole budget keeps the common case fast.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(100);
    while std::time::Instant::now() < deadline {
        if !up
            .ok(&["routine", "show", "tester", "digest"])
            .contains("never run")
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    panic!(
        "a routine due every minute never ran in 100 seconds. `botroster up` is not \
         calling `routine tick`, which is the state this test exists to prevent \
         returning to.\nbanner:\n{}",
        std::fs::read_to_string(&up.log).unwrap_or_default()
    );
}
