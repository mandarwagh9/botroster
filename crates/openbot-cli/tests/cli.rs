//! Tests that drive the shipped binary.
//!
//! Most other tests call library code, and a command can break in ways no
//! library test can see: a relative browser profile path, a tool count in the
//! README that nothing checks. These tests run the real executable from a
//! clean directory, the way a person would. `CARGO_BIN_EXE_openbot` is the
//! binary cargo just built, so this cannot drift from what ships.
//!
//! Commands needing a live hub are in `cli_live.rs`; these are the ones that
//! only touch local state, which is most of them.

use std::path::Path;
use std::process::Output;

const OPENBOT: &str = env!("CARGO_BIN_EXE_openbot");

/// Run the real binary with a home, and fail loudly with both streams.
///
/// `--home` is accepted at either position: `openbot bot --home X new Y` and
/// `openbot bot new Y --home X` both work. This puts it after the group, which
/// is where it is declared; `home_is_accepted_after_the_leaf_too` covers the
/// other. Inserting it here rather than at every call site keeps the tests
/// reading like the commands a person types.
fn run(home: &Path, args: &[&str]) -> Output {
    let (cmd, rest) = args.split_first().expect("a command");
    let out = std::process::Command::new(OPENBOT)
        .arg(cmd)
        .arg("--home")
        .arg(home)
        .args(rest)
        // Colour codes would make every assertion below fragile.
        .env("NO_COLOR", "1")
        // Never inherit a developer's hub or key into a test.
        .env_remove("OPENBOT_HUB_URL")
        .env_remove("OPENBOT_HOME")
        .output()
        .unwrap_or_else(|e| panic!("could not run {OPENBOT}: {e}"));
    out
}

fn ok(home: &Path, args: &[&str]) -> String {
    let out = run(home, args);
    assert!(
        out.status.success(),
        "`openbot {}` failed ({})\nstdout: {}\nstderr: {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn fails(home: &Path, args: &[&str]) -> String {
    let out = run(home, args);
    assert!(
        !out.status.success(),
        "`openbot {}` was expected to fail but succeeded:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn home_is_accepted_after_the_leaf_too() {
    // Flags are typed last by habit. `--home` is declared on the group
    // (`bot`, `routine`, ...) so without `global = true` the leaf position is
    // a clap parse error: "unexpected argument '--home' found". `openbot
    // status --home X` works regardless, because status has no leaf, which
    // hides the problem.
    let h = home();
    // A read-only leaf per group, so this asserts on parsing and nothing else.
    let leaves: &[&[&str]] = &[
        &["bot", "ls"],
        &["group", "ls"],
        &["routine", "ls"],
        &["secret", "ls"],
        &["skill", "ls"],
        &["connector", "ls"],
        &["config", "show"],
    ];

    // `computer` carries its own pair of these, under different env vars.
    // Checked separately because the flag has a different name.
    for leaf in [["computer", "snapshots"], ["computer", "status"]] {
        let out = std::process::Command::new(OPENBOT)
            .args(leaf)
            .arg("--store")
            .arg(h.path())
            .env("NO_COLOR", "1")
            .env_remove("OPENBOT_HUB_URL")
            .env_remove("OPENBOT_STORE")
            .output()
            .expect("run openbot");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("unexpected argument"),
            "`openbot {} --store X` was rejected by the parser:
{err}",
            leaf.join(" ")
        );
        assert!(
            out.status.success(),
            "`openbot {} --store X` failed:
{err}",
            leaf.join(" ")
        );
    }

    for leaf in leaves {
        let out = std::process::Command::new(OPENBOT)
            .args(*leaf)
            .arg("--home")
            .arg(h.path())
            .env("NO_COLOR", "1")
            .env_remove("OPENBOT_HUB_URL")
            .env_remove("OPENBOT_HOME")
            .output()
            .expect("run openbot");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("unexpected argument"),
            "`openbot {} --home X` was rejected by the parser:\n{err}",
            leaf.join(" ")
        );
        assert!(
            out.status.success(),
            "`openbot {} --home X` failed:\n{err}",
            leaf.join(" ")
        );
    }

    // The value must actually be used, not merely tolerated: a default
    // silently winning here would create ./openbot-data under the test's cwd
    // and read someone else's Bots.
    let out = std::process::Command::new(OPENBOT)
        .args(["bot", "new", "Placed"])
        .arg("--home")
        .arg(h.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .output()
        .expect("run openbot");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        h.path().join("bots").exists(),
        "--home after the leaf parsed, but the Bot was written somewhere else"
    );
}

#[test]
fn every_command_has_help_that_actually_renders() {
    // A subcommand whose `--help` panics or exits non-zero is broken for the
    // one person most likely to try it: someone who has never used it.
    let d = home();
    for cmd in [
        "up",
        "run",
        "tools",
        "call",
        "secret",
        "connector",
        "watch",
        "servers",
        "bot",
        "group",
        "event",
        "config",
        "routine",
        "computer",
    ] {
        let out = std::process::Command::new(OPENBOT)
            .args([cmd, "--help"])
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "`openbot {cmd} --help` failed");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("Usage"),
            "`openbot {cmd} --help` printed no usage:\n{text}"
        );
    }
    drop(d);
}

#[test]
fn bots_can_be_created_listed_and_talked_about() {
    let d = home();
    let h = d.path();

    let out = ok(h, &["bot", "new", "Account Health", "--title", "Renewals"]);
    assert!(out.contains("created"), "{out}");

    let list = ok(h, &["bot", "ls"]);
    assert!(
        list.contains("Renewals"),
        "the new bot is not listed:\n{list}"
    );

    let show = ok(h, &["bot", "show", "Account Health"]);
    assert!(show.contains("Account Health"), "{show}");

    // A name that does not exist must be an error, not an empty success.
    let e = fails(h, &["bot", "show", "Nobody"]);
    assert!(!e.is_empty(), "a missing bot produced no explanation");
}

/// Editing a Bot, through the binary a client drives.
///
/// The interesting half is what a rename does not do. A Bot's id is the key
/// its conversation, inbox, group membership and routines are stored under, so
/// renaming keeps it, and the Bot then has to answer to both the new name and
/// the id, or half the references in a home stop working.
#[test]
fn a_bot_can_be_renamed_and_described_without_losing_who_it_is() {
    let d = home();
    let h = d.path();

    ok(
        h,
        &[
            "bot",
            "new",
            "Talent Scout",
            "--title",
            "recruiting",
            "--description",
            "finds people",
        ],
    );

    let out = ok(h, &["bot", "set", "Talent Scout", "--rename", "Recruiting"]);
    assert!(
        out.contains("talent-scout") && out.contains("Recruiting"),
        "a rename should say the id stayed and the name changed: {out}"
    );

    // Reachable by the new name (the resolve path a renamed Bot depends on)
    // and still by the id, which is what groups hold.
    let by_name = ok(h, &["bot", "show", "Recruiting"]);
    assert!(by_name.contains("Recruiting"), "{by_name}");
    let by_id = ok(h, &["bot", "show", "talent-scout"]);
    assert!(by_id.contains("Recruiting"), "{by_id}");

    // Editing one field leaves the other alone. A settings form sends what was
    // edited, and clearing a description nobody touched is data loss.
    ok(h, &["bot", "set", "Recruiting", "--title", "hiring"]);
    let json = ok(h, &["bot", "ls", "--json"]);
    assert!(
        json.contains(r#""title":"hiring""#),
        "the title did not change: {json}"
    );
    assert!(
        json.contains(r#""description":"finds people""#),
        "editing the title cleared the description: {json}"
    );

    // An edit that changes nothing is reported as such, not as a line that
    // reads as though something happened.
    let e = fails(h, &["bot", "set", "Recruiting"]);
    assert!(e.contains("nothing to change"), "{e}");
}

/// Deleting a Bot must leave the rest of the home working.
///
/// A group is not inside the Bot's directory, so removing only the directory
/// would leave the membership list naming a Bot that is gone, and every post
/// to that group would then fail with `no bot \`talent-scout\``, permanently,
/// from an operation that had said "deleted".
#[test]
fn deleting_a_bot_does_not_leave_a_group_pointing_at_it() {
    let d = home();
    let h = d.path();

    ok(h, &["bot", "new", "Talent Scout"]);
    ok(h, &["bot", "new", "Writer"]);
    ok(
        h,
        &["group", "new", "Launch", "--members", "talent-scout,writer"],
    );
    ok(
        h,
        &[
            "routine",
            "new",
            "talent-scout",
            "morning",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "check the pipeline",
        ],
    );

    let said = ok(h, &["bot", "rm", "talent-scout"]);
    // Irreversible, so it says what it took. A routine that stops running is
    // otherwise noticed weeks later, if at all.
    assert!(said.contains("routine"), "{said}");
    assert!(
        said.contains("Launch"),
        "the group it was pulled out of is not named: {said}"
    );

    let groups = ok(h, &["group", "ls", "--json"]);
    assert!(
        !groups.contains("talent-scout"),
        "the group still holds a Bot that does not exist: {groups}"
    );
    assert!(groups.contains("writer"), "{groups}");

    // Posting needs a hub and there is none here (that is what `cli_live.rs`
    // is for), so this asserts the precise property rather than the whole
    // turn: whatever else stops it, it is not stopped by a member who does
    // not exist.
    let out = run(h, &["group", "post", "Launch", "hello", "--demo"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !said.contains("no bot"),
        "the group still resolves to a deleted Bot: {said}"
    );
}

#[test]
fn a_handoff_lands_in_the_recipients_inbox() {
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Researcher"]);
    ok(h, &["bot", "new", "Writer"]);

    ok(
        h,
        &[
            "bot",
            "send",
            "Writer",
            "sources are in /workspace/refs",
            "--from",
            "Researcher",
        ],
    );
    let inbox = ok(h, &["bot", "inbox", "Writer"]);
    assert!(inbox.contains("sources are in"), "{inbox}");
}

#[test]
fn secrets_round_trip_through_the_shipped_binary() {
    let d = home();
    let h = d.path();

    // `secret set` reads stdin by design: a value in argv is world-readable
    // in /proc and lands in shell history. So it cannot be tested with the
    // plain helper above.
    let mut child = std::process::Command::new(OPENBOT)
        .args(["secret", "--home"])
        .arg(h)
        .args(["set", "linear-token"])
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"sk-live-from-the-cli-test\n")
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "secret set failed");

    let listed = ok(h, &["secret", "ls"]);
    assert!(listed.contains("linear-token"), "{listed}");
    // The listing shows a fingerprint, never the value.
    assert!(
        !listed.contains("sk-live"),
        "a secret was printed: {listed}"
    );

    ok(h, &["secret", "rm", "linear-token"]);
    let after = ok(h, &["secret", "ls"]);
    // The empty-state hint names a secret ("openbot secret set linear-token"),
    // so asserting the name is absent would pass for the wrong reason. Assert
    // on the empty state itself.
    assert!(
        after.contains("no secrets yet"),
        "the store is not empty after rm: {after}"
    );
}

#[test]
fn a_connector_with_a_literal_token_is_refused_by_the_binary() {
    let d = home();
    let h = d.path();
    let e = fails(
        h,
        &[
            "connector",
            "add",
            "linear",
            "https://mcp.example.invalid/mcp",
            "--authorization",
            "Bearer sk-live-literal",
        ],
    );
    assert!(
        e.contains("openbot secret set"),
        "the refusal did not say what to do instead:\n{e}"
    );

    // A connector referencing a secret that does not exist fails at add
    // time, where the person still remembers what they typed.
    let e = fails(
        h,
        &[
            "connector",
            "add",
            "linear",
            "https://mcp.example.invalid/mcp",
            "--authorization",
            "Bearer ${nope-token}",
        ],
    );
    assert!(e.contains("nope-token"), "{e}");
}

#[test]
fn routines_can_be_scheduled_and_listed() {
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Account Health"]);

    ok(
        h,
        &[
            "routine",
            "new",
            "Account Health",
            "Morning watch list",
            "--cron",
            "0 9 * * MON-FRI",
            "--instructions",
            "Rank the portfolio by churn risk.",
        ],
    );
    // Listings show the slug id, which is what every other command takes.
    let list = ok(h, &["routine", "ls"]);
    assert!(list.contains("morning-watch-list"), "{list}");
    assert!(
        list.contains("next "),
        "a routine that never says when: {list}"
    );

    // An impossible cron must be refused when it is written, not when it
    // would first fire.
    let e = fails(
        h,
        &[
            "routine",
            "new",
            "Account Health",
            "Broken",
            "--cron",
            "not a cron",
            "--instructions",
            "x",
        ],
    );
    assert!(!e.is_empty());
}

#[test]
fn a_group_needs_members_that_exist() {
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Coordinator"]);
    ok(h, &["bot", "new", "Writer"]);

    ok(
        h,
        &[
            "group",
            "new",
            "Website Launch",
            "--members",
            "Coordinator,Writer",
        ],
    );
    let list = ok(h, &["group", "ls"]);
    assert!(list.contains("website-launch"), "{list}");

    let e = fails(
        h,
        &[
            "group",
            "new",
            "Ghosts",
            "--members",
            "Coordinator,DoesNotExist",
        ],
    );
    assert!(e.contains("DoesNotExist"), "{e}");
}

#[test]
fn config_reads_back_what_it_was_given() {
    let d = home();
    let h = d.path();
    ok(h, &["config", "set", "--model", "grok-4-5"]);
    let shown = ok(h, &["config", "show"]);
    assert!(shown.contains("grok-4-5"), "{shown}");
}

#[test]
fn the_computer_reports_its_state_before_anything_has_run() {
    // `--store`, not `--home`: this one operates on the volume directly.
    let d = home();
    let out = std::process::Command::new(OPENBOT)
        .args(["computer", "--store"])
        .arg(d.path())
        .arg("status")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "computer status failed on a fresh store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn hiding_a_bot_says_its_routines_keep_running() {
    // SPEC §8 asks for this to be surfaced clearly. "Its work is kept" on its
    // own reads as "the data is safe", not "this Bot goes on running, and
    // spending, out of sight".
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Watcher"]);
    ok(
        h,
        &[
            "routine",
            "new",
            "Watcher",
            "Nightly digest",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "x",
        ],
    );

    let out = ok(h, &["bot", "hide", "Watcher"]);
    assert!(
        out.contains("does not pause"),
        "hiding said nothing about the routine still running:\n{out}"
    );
    assert!(out.contains("nightly-digest"), "{out}");
    assert!(
        out.contains("routine pause"),
        "the warning does not say what to do about it:\n{out}"
    );
}

#[test]
fn hiding_a_bot_with_nothing_scheduled_stays_quiet() {
    // A warning that fires when there is nothing to warn about is noise, and
    // noise is how a real warning gets ignored.
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Quiet"]);
    let out = ok(h, &["bot", "hide", "Quiet"]);
    assert!(!out.contains("does not pause"), "{out}");
}

#[test]
fn deleting_a_bot_takes_its_routines_with_it() {
    // The other half of the same spec line: delete removes them, hide does not.
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Watcher"]);
    ok(
        h,
        &[
            "routine",
            "new",
            "Watcher",
            "Nightly",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "x",
        ],
    );
    ok(h, &["bot", "rm", "Watcher"]);

    let listed = ok(h, &["routine", "ls"]);
    assert!(
        !listed.contains("nightly"),
        "a deleted Bot left its routines behind, which would fire with no Bot \
         to run them:\n{listed}"
    );
}

#[test]
fn a_skill_can_be_created_listed_shown_and_removed() {
    let d = home();
    let h = d.path();

    ok(
        h,
        &[
            "skill",
            "new",
            "Refund a customer",
            "--description",
            "How to issue a refund, including approvals.",
        ],
    );

    let listed = ok(h, &["skill", "ls"]);
    assert!(listed.contains("refund-a-customer"), "{listed}");
    assert!(listed.contains("How to issue a refund"), "{listed}");

    let shown = ok(h, &["skill", "show", "refund-a-customer"]);
    assert!(shown.contains("Write the procedure here"), "{shown}");

    ok(h, &["skill", "rm", "refund-a-customer"]);
    let after = ok(h, &["skill", "ls"]);
    assert!(after.contains("no skills yet"), "{after}");
}

#[test]
fn a_skill_that_will_not_load_is_reported_to_the_person_who_wrote_it() {
    // A skill missing its description is invisible to the Bot. A tracing line
    // at hub boot is not enough: the author never sees it and would only
    // watch the Bot ignore the procedure.
    let d = home();
    let h = d.path();
    let broken = h.join("skills").join("half-written");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("SKILL.md"),
        "---\nname: half-written\n---\n\nsteps\n",
    )
    .unwrap();

    let listed = ok(h, &["skill", "ls"]);
    assert!(
        listed.contains("half-written") && listed.contains("description"),
        "a skill that failed to load was not reported:\n{listed}"
    );
}

#[test]
fn a_skill_name_cannot_escape_its_directory() {
    let d = home();
    let h = d.path();
    for bad in ["../escape", "a/b"] {
        let e = fails(h, &["skill", "new", bad, "--description", "x"]);
        assert!(!e.is_empty(), "`{bad}` was accepted as a skill name");
    }
}

#[test]
fn a_bots_conversation_can_be_read_back() {
    // The conversation survives the process, and this is how it is read back;
    // `bot show` reports only how many messages there are.
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Account Health"]);

    let empty = ok(h, &["bot", "log", "Account Health"]);
    assert!(
        empty.contains("has not been given anything yet"),
        "an empty Bot should say so, not print nothing: {empty}"
    );
    // It should say what to type next.
    assert!(empty.contains("openbot run --bot"), "{empty}");
}

#[test]
fn reading_back_a_bot_that_does_not_exist_is_an_error() {
    let d = home();
    let e = fails(d.path(), &["bot", "log", "Nobody"]);
    assert!(!e.is_empty());
}

#[test]
fn closing_a_pipe_early_is_not_a_crash() {
    // `openbot bot log big-bot | head` must not end in a Rust panic:
    //
    //   thread 'main' panicked at library/std/src/io/stdio.rs:
    //   failed printing to stdout: The pipe is being closed. (os error 232)
    //
    // `println!` panics when stdout closes, and Rust ignores SIGPIPE, so this
    // happens on every platform. To a person it looks like the tool crashed
    // when they simply stopped reading.
    use std::io::{BufRead, BufReader};

    let d = home();
    let h = d.path();
    // Output has to exceed the pipe buffer, or the child finishes writing
    // before anyone walks away and the test proves nothing. A large skill body
    // is the simplest way to be sure.
    let skill = h.join("skills").join("big");
    std::fs::create_dir_all(&skill).unwrap();
    let body = "a line of a very long procedure
"
    .repeat(20_000);
    std::fs::write(
        skill.join("SKILL.md"),
        format!(
            "---
name: big
description: a long one
---

{body}"
        ),
    )
    .unwrap();

    let mut child = std::process::Command::new(OPENBOT)
        .args(["skill", "--home"])
        .arg(h)
        .args(["show", "big"])
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Read one line, then close the pipe, as `head -1` does.
    {
        let out = child.stdout.take().unwrap();
        let mut r = BufReader::new(out);
        let mut first = String::new();
        let _ = r.read_line(&mut first);
        assert!(!first.trim().is_empty(), "no output at all");
    }

    let status = child.wait().unwrap();
    // 101 is a Rust panic. Anything else, including being killed by a signal
    // on Unix, is a normal way for a program to stop when its reader leaves.
    assert_ne!(
        status.code(),
        Some(101),
        "the CLI panicked because somebody stopped reading"
    );
}

#[test]
fn status_works_when_the_hub_is_down() {
    // The state it exists for. A status command that needs the thing it is
    // reporting on to be healthy is no use on the day it is not.
    let d = home();
    let h = d.path();
    ok(h, &["bot", "new", "Account Health"]);

    let out = std::process::Command::new(OPENBOT)
        .args(["status", "--home"])
        .arg(h)
        // A port with nothing on it.
        .env("OPENBOT_HUB_URL", "ws://127.0.0.1:1/v1/tools")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "status should not fail when the hub is"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("unreachable"), "{text}");
    // It still reports what it can read locally.
    assert!(text.contains("bots"), "{text}");
}

#[test]
fn a_routine_that_is_both_due_and_idle_does_not_get_one_last_run() {
    // `openbot status` says routines "are paused until you resume them". That
    // sentence is only true because of an ordering in `tick`: the idle sweep
    // runs, saves `enabled = false`, and only then is the due list collected.
    // Swapped, every idle routine would fire once more on the tick that
    // pauses it, which is precisely the unattended spend the rule exists to
    // prevent, and the output would still say "paused".
    //
    // The unit tests cover `idle_routines` returning the right list; this
    // covers the order they are used in.
    let h = home();
    ok(h.path(), &["bot", "new", "Watcher"]);
    ok(
        h.path(),
        &[
            "routine",
            "new",
            "Watcher",
            "Nightly",
            "--cron",
            "* * * * *",
            "--instructions",
            "check the logs",
        ],
    );

    // Make it genuinely overdue, and the account long unattended, without
    // waiting a minute of wall clock for a cron boundary.
    let store = openbot_bots::BotStore::open(h.path()).expect("open the store");
    let mut r = store.all_routines().expect("routines").pop().expect("one");
    r.last_run = Some(chrono::Utc::now() - chrono::Duration::days(2));
    store.save_routine(&r).expect("save");
    store
        .mark_seen(chrono::Utc::now() - chrono::Duration::days(30))
        .expect("mark seen");
    assert!(
        store.due(chrono::Utc::now()).expect("due").len() == 1,
        "the routine was not actually due, so this test would pass for the wrong reason"
    );

    let out = ok(
        h.path(),
        &["routine", "tick", "--approve", "auto", "--demo"],
    );
    assert!(out.contains("paused watcher/nightly"), "{out}");
    assert!(
        !out.contains("running watcher/nightly"),
        "an idle-paused routine still ran on the tick that paused it:
{out}"
    );

    // It stays paused, rather than being disabled only for this tick.
    let after = ok(h.path(), &["routine", "ls"]);
    assert!(after.contains("paused"), "{after}");
}

#[test]
fn pruning_will_not_guess_how_much_history_to_destroy() {
    // This is the only command here that destroys history. A default keep
    // count would let `openbot computer prune`, typed by someone finding out
    // what it does, silently delete older snapshots. Every other lifecycle
    // operation in this layer is undoable; this one is not, so the number has
    // to be typed.
    let h = home();
    let out = std::process::Command::new(OPENBOT)
        .args(["computer", "prune"])
        .arg("--store")
        .arg(h.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_STORE")
        .output()
        .expect("run openbot");

    assert!(!out.status.success(), "a bare prune was accepted");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("KEEP"),
        "the missing argument is not named: {err}"
    );

    // It still works when told what to keep.
    let out = std::process::Command::new(OPENBOT)
        .args(["computer", "prune", "3"])
        .arg("--store")
        .arg(h.path())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_STORE")
        .output()
        .expect("run openbot");
    assert!(
        out.status.success(),
        "prune with a count failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `bot ls --json` is what OPENBOT's own clients read to build a roster.
///
/// A sidebar assembled by parsing the column layout breaks the first time a
/// Bot's name is long enough to widen a column, and it cannot see a field the
/// table does not print. So the machine-readable form is the contract, and
/// this pins it: valid JSON, one array, and every field a roster needs.
#[test]
fn bot_ls_json_is_a_roster_a_client_can_read() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    ok(
        home,
        &[
            "bot",
            "new",
            "Talent Scout",
            "--title",
            "Hiring",
            "--description",
            "Finds candidates",
        ],
    );
    ok(home, &["bot", "new", "Expense Manager"]);
    ok(home, &["bot", "hide", "Expense Manager"]);

    let text = ok(home, &["bot", "ls", "--json"]);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"));

    assert_eq!(
        rows.len(),
        1,
        "a hidden Bot should stay out of the list unless asked for: {text}"
    );
    let scout = &rows[0];
    assert_eq!(scout["name"], "Talent Scout");
    assert_eq!(
        scout["title"], "Hiring",
        "a sidebar shows the job, not just the name"
    );
    assert_eq!(scout["description"], "Finds candidates");
    assert_eq!(scout["hidden"], false);
    assert!(
        scout["id"].is_string(),
        "the id is what a client addresses; it must be there"
    );
    assert!(
        scout["messages"].is_number(),
        "a roster shows whether a conversation exists"
    );

    // `--all` is how "Show hidden chats" at the bottom of the sidebar works,
    // and the flag has to travel or the client cannot tell the two lists apart.
    let text = ok(home, &["bot", "ls", "--json", "--all"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(text.trim()).expect("JSON");
    assert_eq!(rows.len(), 2, "--all should include the hidden Bot: {text}");
    assert!(
        rows.iter()
            .any(|b| b["name"] == "Expense Manager" && b["hidden"] == true),
        "the hidden one must say so, or it looks identical to the rest: {text}"
    );
}

/// An empty roster is an empty array, not a sentence about there being none.
/// A client that has to special-case prose is a client that will get it wrong.
#[test]
fn an_empty_roster_is_still_json() {
    let home = tempfile::tempdir().unwrap();
    let text = ok(home.path(), &["bot", "ls", "--json"]);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"));
    assert!(rows.is_empty(), "expected [], got {text}");
}

/// A rule written by the command is a rule the hub will enforce. Adding one
/// and listing it back proves the two halves agree; `config.rs`'s own tests
/// prove the second half reaches the engine.
#[test]
fn permission_rules_can_be_added_listed_and_removed() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();

    let empty = ok(home, &["permission", "ls", "--json"]);
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(empty.trim()).unwrap(),
        Vec::<serde_json::Value>::new()
    );

    ok(
        home,
        &[
            "permission",
            "add",
            "--action",
            "deny",
            "--tool",
            "shell.exec",
            "--reason",
            "read-only account",
        ],
    );
    ok(
        home,
        &["permission", "add", "--action", "allow", "--tool", "fs.*"],
    );

    let text = ok(home, &["permission", "ls", "--json"]);
    let rules: Vec<serde_json::Value> = serde_json::from_str(text.trim()).expect("JSON");
    assert_eq!(rules.len(), 2, "{text}");
    assert_eq!(rules[0]["action"], "deny");
    assert_eq!(rules[0]["tool"], "shell.exec");
    assert_eq!(
        rules[0]["reason"], "read-only account",
        "the reason a person wrote must survive to the approval they will read"
    );

    ok(home, &["permission", "rm", "1"]);
    let text = ok(home, &["permission", "ls", "--json"]);
    let rules: Vec<serde_json::Value> = serde_json::from_str(text.trim()).expect("JSON");
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0]["tool"], "fs.*",
        "the wrong rule was removed: {text}"
    );

    let err = fails(home, &["permission", "rm", "9"]);
    assert!(err.contains("no rule 9"), "{err}");
}

/// Editing a rule must not delete the rest of the file. A config editor that
/// drops the parts it does not understand is worse than none, and the typed
/// `Config` knows about two tables; anything else a person wrote, or a later
/// version added, would vanish on the first `permission add`.
#[test]
fn adding_a_rule_leaves_the_rest_of_the_config_alone() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[model]\nid = \"grok-4-5\"\n\n[ui]\ntheme = \"dark\"\n",
    )
    .unwrap();

    ok(
        home.path(),
        &[
            "permission",
            "add",
            "--action",
            "allow",
            "--tool",
            "fs.read",
        ],
    );

    let after = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        after.contains("grok-4-5"),
        "the model setting was dropped: {after}"
    );
    assert!(
        after.contains("theme") && after.contains("dark"),
        "a table this build does not know about was dropped: {after}"
    );
    assert!(
        after.contains("fs.read"),
        "the rule was not written: {after}"
    );
}

/// A rule that stops or refuses a call has to say why. The reason is what the
/// person sees in the approval and what whoever reads the log has to go on;
/// "denied" on its own is unactionable. An `allow` needs none; nobody is
/// interrupted by it.
#[test]
fn a_rule_that_stops_a_call_must_say_why() {
    let home = tempfile::tempdir().unwrap();
    for action in ["deny", "ask"] {
        let err = fails(
            home.path(),
            &[
                "permission",
                "add",
                "--action",
                action,
                "--tool",
                "shell.exec",
            ],
        );
        assert!(err.contains("--reason"), "{action}: {err}");
    }
    ok(
        home.path(),
        &[
            "permission",
            "add",
            "--action",
            "allow",
            "--tool",
            "fs.read",
        ],
    );
}

/// A misspelt action is refused by name rather than quietly becoming
/// something else.
#[test]
fn an_unknown_action_is_named_in_the_error() {
    let home = tempfile::tempdir().unwrap();
    let err = fails(
        home.path(),
        &[
            "permission",
            "add",
            "--action",
            "maybe",
            "--tool",
            "fs.read",
        ],
    );
    assert!(err.contains("maybe"), "{err}");
    assert!(
        err.contains("allow"),
        "the error should list what is valid: {err}"
    );
}

/// Search reads the conversations on disk and says where a phrase was said.
///
/// This is how a half-remembered conversation is found without knowing which
/// Bot had it.
#[test]
fn search_finds_a_phrase_and_says_who_said_it() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    ok(home, &["bot", "new", "Account Health"]);
    ok(home, &["bot", "new", "Bug Repro"]);
    std::fs::write(
        home.join("bots/account-health/conversation.jsonl"),
        "{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"look at the renewal risk for Acme\"}]}\n\
         {\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Acme shows churn signals\"}]}\n",
    )
    .unwrap();

    let text = ok(home, &["search", "renewal", "--json"]);
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"));
    assert_eq!(hits.len(), 1, "{text}");
    assert_eq!(hits[0]["kind"], "bot");
    assert_eq!(
        hits[0]["name"], "account-health",
        "the result must say who said it"
    );
    assert_eq!(hits[0]["role"], "user");
    assert!(
        hits[0]["at"].is_number(),
        "a result has to be able to say where in the conversation it was"
    );
    assert!(
        hits[0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("renewal")),
        "the snippet should contain the match: {text}"
    );

    // Case does not matter: nobody remembers how they capitalised it.
    let upper = ok(home, &["search", "RENEWAL", "--json"]);
    let hits: Vec<serde_json::Value> = serde_json::from_str(upper.trim()).expect("JSON");
    assert_eq!(hits.len(), 1, "search should ignore case: {upper}");

    // A Bot that said nothing matching does not appear at all.
    let none = ok(home, &["search", "nothing-said-this", "--json"]);
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(none.trim()).unwrap(),
        Vec::<serde_json::Value>::new()
    );
}

/// Tool traffic is not searched. "Find where we discussed the renewal" means
/// the sentence, not the `fs.read` that followed it, and matching a tool's
/// arguments or output buries the sentence under the machinery beneath it.
#[test]
fn search_does_not_match_inside_tool_traffic() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    ok(home, &["bot", "new", "Bug Repro"]);
    std::fs::write(
        home.join("bots/bug-repro/conversation.jsonl"),
        "{\"role\":\"assistant\",\"content\":[\
           {\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"fs.read\",\"input\":{\"path\":\"renewal.md\"}}]}\n",
    )
    .unwrap();

    let text = ok(home, &["search", "renewal", "--json"]);
    let hits: Vec<serde_json::Value> = serde_json::from_str(text.trim()).expect("JSON");
    assert!(
        hits.is_empty(),
        "a tool argument was matched as though somebody had said it: {text}"
    );
}

/// A long message is shown around the match, not from its beginning. A hit two
/// thousand characters in would otherwise scroll two thousand characters of
/// something else past the person reading.
#[test]
fn a_long_message_is_shown_around_the_match() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    ok(home, &["bot", "new", "Long Winded"]);
    let padding = "x".repeat(2000);
    std::fs::write(
        home.join("bots/long-winded/conversation.jsonl"),
        format!(
            "{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{padding} needle {padding}\"}}]}}\n"
        ),
    )
    .unwrap();

    let text = ok(home, &["search", "needle", "--json"]);
    let hits: Vec<serde_json::Value> = serde_json::from_str(text.trim()).expect("JSON");
    let snippet = hits[0]["text"].as_str().expect("a snippet");
    assert!(snippet.contains("needle"), "{snippet}");
    assert!(
        snippet.chars().count() < 200,
        "the whole message was returned instead of the line around the match: {} chars",
        snippet.chars().count()
    );
    assert!(
        snippet.starts_with('…') && snippet.ends_with('…'),
        "truncation on both sides should be visible: {snippet}"
    );
}

/// Nothing to look for is a refusal, not a listing of everything ever said.
#[test]
fn an_empty_search_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let err = fails(home.path(), &["search", "   "]);
    assert!(err.contains("nothing to look for"), "{err}");
}

/// Whether `status` printed a row under this label.
///
/// Matching the label rather than the substring: "config" appears inside "none
/// configured", which is on the model line of every home without a model, so a
/// substring test for the absence of a config row fails on a healthy account.
fn has_row(shown: &str, label: &str) -> bool {
    shown
        .lines()
        .any(|l| l.split_whitespace().next() == Some(label))
}

/// `status` notices a `config.toml` the hub would refuse to start on.
///
/// The command's one-line help is "Is anything wrong?", and it answered no
/// about a file that will not parse. `ModelOverrides::applied` reads the config
/// with `load(home).unwrap_or_default()`, so an unparseable file silently
/// became the shipped defaults, and every line `status` printed described
/// settings nobody had chosen.
///
/// Driven through the binary rather than the renderer. `status::render` is unit
/// tested for both states, and it would keep passing while `gather` swallowed
/// the error — the two halves are only connected out here, which is the seam
/// this file exists for.
///
/// The config below is the README's own `[permission]` example as it shipped:
/// `action = "ask"`, rejected by the parser that reads it. `ask` is an accepted
/// spelling now, so the example is written with a genuinely unknown action to
/// keep testing the property rather than that one historical typo.
#[test]
fn status_reports_a_config_the_hub_would_refuse() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[permission]\nrules = [{ tool = \"shell.exec\", action = \"maybe\" }]\n",
    )
    .unwrap();

    let out = run(home.path(), &["status"]);
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        shown.contains("config"),
        "`status` said nothing about a config.toml that will not load, while reporting on \
         settings it invented to replace it:\n{shown}"
    );
    assert!(
        shown.contains("maybe") || shown.contains("unknown variant"),
        "the reason has to reach the screen, or the next step is a guess:\n{shown}"
    );
}

/// And a config that loads is not announced.
///
/// Without this, a `status` that printed the warning unconditionally would
/// satisfy the test above. This screen is a list of what is wrong; a reassuring
/// row on every run is how people stop reading the rows that matter.
#[test]
fn status_says_nothing_about_a_config_that_loads() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[permission]\nrules = [{ tool = \"shell.exec\", action = \"ask\" }]\n",
    )
    .unwrap();

    let out = run(home.path(), &["status"]);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !has_row(&shown, "config"),
        "a working config should be silent. `ask` is an accepted spelling of \
         `require_approval`, because every other surface in this product teaches that \
         word:\n{shown}"
    );
}

/// One command, one terminal, nothing configured.
///
/// This is the first thing anyone types, and it used to fail. `run` connected
/// to a hub that was not there and reported a bare winsock errno whose remedy
/// named `openbotd` and `openbot-guest` — two binaries the documented install
/// does not put on anyone's PATH — while not mentioning `openbot up`, which is
/// the binary they had just typed. Getting to a first result cost five steps
/// and two terminals, and nothing said so until the first terminal was gone.
///
/// The split is real in a deployment, where the guest runs elsewhere. It was
/// never a reason for it to be the first thing a person meets.
///
/// Deliberately not asserting on the transcript: what the demo script does is
/// covered elsewhere, and pinning it here would make this fail for reasons that
/// have nothing to do with whether the command stands on its own.
#[test]
fn a_first_run_needs_no_second_terminal_and_no_config() {
    let home = tempfile::tempdir().unwrap();
    let out = run(
        home.path(),
        &["run", "--demo", "--approve", "auto", "prove it"],
    );
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "a fresh install could not reach a first result in one command:\n{shown}"
    );
    assert!(
        !shown.contains("could not reach the hub"),
        "the hub was not started for a command that needed one:\n{shown}"
    );
}

/// And it says so while it does it.
///
/// Starting a computer takes a second or two, and silence during it reads as a
/// hang. This is also the assertion that the stack really was started here
/// rather than found: if a hub were somehow already running, this line would be
/// absent and the test above would still pass.
#[test]
fn a_run_that_starts_its_own_computer_says_it_is_doing_so() {
    let home = tempfile::tempdir().unwrap();
    let out = run(
        home.path(),
        &["run", "--demo", "--approve", "auto", "prove it"],
    );
    let shown = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        shown.contains("starting a computer"),
        "no sign that a computer was started, so this run either found one or did without:\n{shown}"
    );
}
