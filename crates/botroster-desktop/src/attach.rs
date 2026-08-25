//! Copying a local file to where a Bot can read it.
//!
//! The guest is jailed to its workspace: `Workspace::resolve` refuses any path
//! that escapes the root, so a file outside it is not readable by a Bot no
//! matter how a prompt describes it. Attaching a file means copying it in
//! first; this module is the client's half of that.
//!
//! # Why a path and not the bytes
//!
//! ACP supports sending file contents in the prompt as an embedded resource,
//! which needs no workspace at all. That is the wrong choice here for two
//! reasons:
//!
//! * The transcript replays. The task becomes the user turn, is written to
//!   `conversation.jsonl`, and `run_task` seeds every following turn with the
//!   last `history` messages (40 by default). A file sent inline is re-sent to
//!   the model on every turn until it falls out of that window. A size cap only
//!   chooses how much gets re-sent.
//! * It would bypass the policy gate. Contents in a prompt reach the model with
//!   no tool call to evaluate, so an operator who denied `fs.read` would still
//!   have files read to their Bot. Enforcement lives in the hub (SPEC §6.0),
//!   and a client that puts file contents in front of a model without a tool
//!   call has moved it.
//!
//! So the file is copied in and the prompt carries a path. The Bot reads it
//! with `fs.read` if it wants it: once, and gated.

use std::path::Path;

/// Copy `file` into the computer's workspace, returning the workspace-relative
/// path the Bot can read.
///
/// # Errors
/// If the binary cannot be run, or the file cannot be read.
pub async fn put(botroster: &Path, hub: &str, file: &Path) -> anyhow::Result<String> {
    let out = base(botroster, hub)
        .arg(file)
        .arg("--json")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "could not attach {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("`botroster attach` answered something else: {e}"))?;
    v.get("path")
        .and_then(|p| p.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("`botroster attach` did not say where it landed"))
}

fn base(botroster: &Path, hub: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(botroster);
    // Resolved through the hub so the destination is the workspace the guest
    // is actually serving. A home-derived path is correct only when `botroster up`
    // was started without `--workspace`, and silently wrong otherwise.
    cmd.arg("attach")
        .arg("--hub")
        .arg(hub)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL");
    #[cfg(windows)]
    {
        // Otherwise attaching a file flashes a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
