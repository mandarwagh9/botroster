//! A hook someone wrote must stop a call they make.
//!
//! `openbotd::hooks` checks that a `PreToolUse` verdict denies and that a hook
//! which cannot answer denies too; `openbotd::boot` checks that a hooks file
//! loads and that a broken one fails the boot. Neither asks whether a file
//! somebody writes stops a tool call somebody makes, which for a guard rail is
//! the only question there is. This test covers the join between them.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod common;

const OPENBOT: &str = env!("CARGO_BIN_EXE_openbot");

/// Write a hook script that always denies, and return the path to run it by.
///
/// Absolute, by necessity. `cmd /C` does not find a bare `deny.bat` in the
/// working directory, and `./deny.bat` fails too because cmd reads the leading
/// slash as a switch, so the form `"command": "./audit.sh"` cannot work on
/// Windows at all. An absolute path is the one spelling that works everywhere.
fn always_denies(dir: &std::path::Path, reason: &str) -> String {
    let (name, body) = if cfg!(windows) {
        (
            "deny.bat",
            format!("@echo off\r\necho {{\"decision\":\"deny\",\"reason\":\"{reason}\"}}\r\n"),
        )
    } else {
        (
            "deny.sh",
            format!("#!/bin/sh\necho '{{\"decision\":\"deny\",\"reason\":\"{reason}\"}}'\n"),
        )
    };

    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.canonicalize()
        .unwrap()
        .display()
        .to_string()
        // Windows canonicalisation yields a `\\?\` prefix, which cmd will not
        // run. The plain path is what a person would have written anyway.
        .trim_start_matches(r"\\?\")
        .to_owned()
}

/// Read the hub URL out of a log `openbot up` is writing.
fn hub_url(log: &std::path::Path, child: &mut Child) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if let Some(i) = text.find("ws://") {
                return text[i..].split_whitespace().next().unwrap_or("").to_owned();
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "`openbot up` exited early ({status}): {}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    common::stop(child);
    panic!("`openbot up` never announced a hub url");
}

#[test]
fn a_hook_someone_wrote_refuses_a_real_tool_call() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("control-plane");
    std::fs::create_dir_all(&home).unwrap();

    let script = always_denies(dir.path(), "HOOK-REASON refused by the audit script");
    std::fs::write(
        home.join("hooks.json"),
        serde_json::json!({ "hooks": [ { "matches": "shell.*", "command": script } ] }).to_string(),
    )
    .unwrap();

    let log = dir.path().join("up.log");
    let mut child = Command::new(OPENBOT)
        .arg("up")
        .args(["--bind", "127.0.0.1:0", "--home"])
        .arg(&home)
        .args(["--snapshot-every", "0"])
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HUB_URL")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_WORKSPACE")
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start openbot up");
    let hub = hub_url(&log, &mut child);

    let call = |tool: &str, args: &str| {
        Command::new(OPENBOT)
            .args(["call", tool, args, "--approve", "auto"])
            .env("OPENBOT_HUB_URL", &hub)
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };

    let denied = call("shell.exec", r#"{"command":"echo hello"}"#);
    let err = String::from_utf8_lossy(&denied.stderr).to_string();
    assert!(!denied.status.success(), "the hook did not stop the call");
    // The hook's own words. Whoever wrote the guard rail said why, and that is
    // the useful half of the message.
    assert!(
        err.contains("HOOK-REASON"),
        "the hook's reason was lost: {err}"
    );

    // It guards only what it matched. A hook that quietly stopped everything
    // would look identical from the denied call's side.
    let allowed = call("fs.write", r#"{"path":"note.md","contents":"ok"}"#);
    assert!(
        allowed.status.success(),
        "an unmatched tool was refused too: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );

    common::stop(&mut child);
}
