//! What a timed-out `shell.exec` leaves behind.
//!
//! `shell.exec` is the most powerful tool the guest offers, and a timeout is
//! the normal way a bad command ends. If the process survives the timeout, the
//! model is told "timed out" while the command keeps running: still writing
//! files, still holding the workspace. An agent that retries then stacks
//! orphans.
//!
//! Detected by side effect rather than by process introspection: the command
//! sleeps, then writes a marker. If the marker ever appears, the command
//! outlived the call that gave up on it.

use std::sync::Arc;
use std::time::Duration;

use openbot_guest::{Context, Workspace};
use serde_json::json;

/// Sleep, then write a marker, portable across the two shells the guest uses.
fn sleep_then_touch(marker: &str) -> String {
    if cfg!(windows) {
        // `ping` is the usual way to sleep in cmd without extra tooling.
        format!("ping -n 5 127.0.0.1 > nul & echo done > {marker}")
    } else {
        format!("sleep 4 && echo done > {marker}")
    }
}

#[tokio::test]
async fn a_timed_out_command_does_not_keep_running() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Arc::new(Context::new(
        Workspace::new(dir.path(), true).unwrap(),
        dir.path().join(".browser"),
    ));

    let marker = "survived.txt";
    let out = openbot_guest::tools::invoke(
        &ctx,
        "shell.exec",
        &json!({ "command": sleep_then_touch(marker), "timeout_secs": 1 }),
        &mut |_| {},
    )
    .await;

    assert!(out.is_err(), "the call should have timed out");
    let e = out.unwrap_err().to_string();
    assert!(e.contains("timed out"), "{e}");

    // Well past when the command would have finished had it survived.
    tokio::time::sleep(Duration::from_secs(7)).await;

    let survivor = dir.path().join(marker);
    assert!(
        !survivor.exists(),
        "the command outlived the timeout and wrote {}; the model was told it \
         stopped while it was still running",
        survivor.display()
    );
}
