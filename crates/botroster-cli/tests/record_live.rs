//! The record, through the shipped binary and a real stack.
//!
//! `botrosterd/tests/record_live.rs` drives the hub directly and proves what it
//! writes. This proves the wiring and the surface: that `boot::hub_from_home`
//! attaches the recorder at all, that `botroster run` opens its session naming
//! a Bot, that the file lands in the home a person would look in, and that
//! there is a way to read it back. Every one of those is a place the feature
//! can be complete and still do nothing — which is this backlog's own headline
//! about the three top items that were built and never run.

use std::process::Command;

const BOTROSTER: &str = env!("CARGO_BIN_EXE_botroster");

mod common;

use common::up::Up;

/// Run a command against `up`'s hub and home, and require it to succeed.
fn ok(up: &Up, args: &[&str]) -> String {
    let out = Command::new(BOTROSTER)
        .args(args)
        .arg("--home")
        .arg(&up.home)
        .env("BOTROSTER_HUB_URL", &up.hub)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .output()
        .expect("could not run botroster");
    assert!(
        out.status.success(),
        "`botroster {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The same, but for a command expected to fail.
fn fails(up: &Up, args: &[&str]) -> String {
    let out = Command::new(BOTROSTER)
        .args(args)
        .arg("--home")
        .arg(&up.home)
        .env("BOTROSTER_HUB_URL", &up.hub)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .output()
        .expect("could not run botroster");
    assert!(
        !out.status.success(),
        "`botroster {}` was expected to fail and did not: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Do the scripted demo as a Bot, and return its session id.
fn a_recorded_run(up: &Up) -> String {
    ok(up, &["bot", "new", "Scribe"]);
    ok(
        up,
        &[
            "run",
            "--demo",
            "--approve",
            "auto",
            "--bot",
            "scribe",
            "prove it",
        ],
    );
    let listed = ok(up, &["bot", "record", "scribe", "--json"]);
    let ids: Vec<String> = serde_json::from_str(listed.trim()).expect("the id list is JSON");
    assert_eq!(ids.len(), 1, "expected one session, got {ids:?}");
    ids.into_iter().next().expect("one id")
}

/// A Bot that has done nothing says what would fill the record.
///
/// A bare "no records" reads as a broken feature. The empty state is the first
/// thing anybody sees, because everyone's first Bot has done nothing yet.
#[test]
fn an_empty_record_says_what_would_fill_it() {
    let Some(up) = Up::start() else {
        return;
    };
    ok(&up, &["bot", "new", "Scribe"]);

    let said = ok(&up, &["bot", "record", "scribe"]);
    assert!(
        said.contains("has not used a tool yet"),
        "the empty state does not say it is empty: {said}"
    );
    assert!(
        said.contains("botroster run --bot scribe"),
        "the empty state does not say what would fill it: {said}"
    );
}

/// The record lists the session, then shows what happened in it.
#[test]
fn the_record_lists_a_session_and_then_shows_its_steps() {
    let Some(up) = Up::start() else {
        return;
    };
    let sid = a_recorded_run(&up);

    let listed = ok(&up, &["bot", "record", "scribe"]);
    assert!(listed.contains(&sid), "the session is not listed: {listed}");
    assert!(
        listed.contains("4 steps"),
        "the demo makes four tool calls and the listing says otherwise: {listed}"
    );

    let shown = ok(&up, &["bot", "record", "scribe", "--session", &sid]);
    for tool in ["fs.write", "fs.read", "fs.list", "shell.exec"] {
        assert!(shown.contains(tool), "`{tool}` is missing from: {shown}");
    }
    assert!(
        shown.contains("{\"path\":\"botroster-demo.md\"}"),
        "the steps do not show what the Bot actually asked for: {shown}"
    );
}

/// `--json` prints the stored lines, unchanged.
///
/// The record is already JSON on disk, so re-encoding it would give a script
/// and a person reading the file two different sets of bytes to reason about.
#[test]
fn json_prints_what_is_stored_rather_than_a_second_encoding() {
    let Some(up) = Up::start() else {
        return;
    };
    let sid = a_recorded_run(&up);

    let printed = ok(
        &up,
        &["bot", "record", "scribe", "--session", &sid, "--json"],
    );
    let on_disk = std::fs::read_to_string(
        up.home
            .join("bots")
            .join("scribe")
            .join("sessions")
            .join(format!("{sid}.jsonl")),
    )
    .expect("the record is on disk");

    let printed: Vec<&str> = printed.lines().filter(|l| !l.trim().is_empty()).collect();
    let stored: Vec<&str> = on_disk.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(printed, stored, "--json is not what the file holds");
    assert_eq!(stored.len(), 4, "expected four recorded steps");

    // And the sequence in the file is the sequence in the lines.
    for (i, line) in stored.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line).expect("a line parses");
        assert_eq!(v["seq"].as_u64(), Some(i as u64 + 1), "line {i}: {line}");
    }
}

/// Asking for a session that was never recorded says which ones there are.
#[test]
fn an_unknown_session_points_at_the_listing() {
    let Some(up) = Up::start() else {
        return;
    };
    a_recorded_run(&up);

    let said = fails(&up, &["bot", "record", "scribe", "--session", "sess-nope"]);
    assert!(
        said.contains("botroster bot record scribe"),
        "the error does not say how to find the real ones: {said}"
    );
}

/// A real run leaves a record behind, through the shipped binary.
///
/// `botrosterd/tests/record_live.rs` drives the hub directly and proves what it
/// writes. This proves the wiring: that `boot::hub_from_home` actually attaches
/// the recorder, that `botroster run` opens its session naming a Bot, and that
/// the file lands in the home a person would look in. Every part of that is a
/// place the feature can be complete and still do nothing.
#[test]
fn a_run_leaves_a_record_in_the_bots_home() {
    let Some(up) = Up::start() else {
        return;
    };

    // The Bot has to exist for `--bot` to name it, and naming it explicitly is
    // what makes the path asserted on below deterministic: without the flag,
    // `run` uses a Bot named after the working directory, which is the crate's
    // and would differ per checkout.
    up.ok(&["bot", "new", "Scribe"]);

    // The scripted demo, which writes a file, reads it back, lists the
    // workspace and runs a shell command — four tool calls against the real
    // guest, with no model and no key.
    ok(
        &up,
        &[
            "run",
            "--demo",
            "--approve",
            "auto",
            "--bot",
            "scribe",
            "prove it",
        ],
    );

    let sessions = up.home.join("bots").join("scribe").join("sessions");
    let files: Vec<_> = std::fs::read_dir(&sessions)
        .unwrap_or_else(|e| panic!("no record directory at {}: {e}", sessions.display()))
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        files.len(),
        1,
        "expected one session's record in {}, found {}",
        sessions.display(),
        files.len()
    );

    let text = std::fs::read_to_string(files[0].path()).expect("the record is readable");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 4,
        "the demo makes four tool calls and the record holds {}:\n{text}",
        lines.len()
    );

    // Parsed, not merely present: a file of unparseable lines is not a record.
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("line {i} does not parse: {e}"));
        assert_eq!(
            v["seq"].as_u64(),
            Some(i as u64 + 1),
            "line {i} is numbered {} in a file where it is line {}",
            v["seq"],
            i + 1
        );
        assert!(
            v["tool"].as_str().is_some_and(|t| !t.is_empty()),
            "a step with no tool name: {line}"
        );
    }
    assert!(
        text.contains("fs.write") && text.contains("shell.exec"),
        "the record does not name the tools the demo actually used:\n{text}"
    );
}
