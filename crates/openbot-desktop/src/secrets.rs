//! Credentials, supplied from the window.
//!
//! A secure secret request is a value entered into the request itself,
//! masked, kept out of the transcript, and never shown to the model. It is not
//! a general-purpose password manager. `openbot secret` is the store behind
//! that; this module lets the window write to it without a terminal.
//!
//! openbot does not yet have the interactive half: a Bot cannot ask for a
//! credential it is missing mid-turn. A connector's secrets are checked when
//! it is added and a missing one is refused there. So this is the supplying
//! half only.
//!
//! Two rules apply throughout, both inherited from the CLI:
//!
//! * The value goes down stdin, never in an argument. Command-line arguments
//!   are world-readable in `/proc/<pid>/cmdline` on Linux for as long as the
//!   process lives, and land in shell history besides.
//! * Nothing ever reads a value back. There is no command that prints one and
//!   no function here that returns one. A fingerprint is enough to tell two
//!   tokens apart or confirm a rotation.

use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

/// One stored credential, as a person sees it: a name and a fingerprint.
///
/// There is intentionally no value field; the store exists so that values are
/// never read back.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// What a connector refers to, e.g. `linear-token`.
    pub name: String,
    /// Enough to tell two apart, or to confirm a rotation.
    pub fingerprint: String,
}

/// Every credential the hub holds for this home.
///
/// # Errors
/// If the binary cannot be run, exits non-zero, or answers something else.
pub async fn list(openbot: &Path, home: &Path) -> anyhow::Result<Vec<Entry>> {
    let out = base(openbot, home)
        .arg("ls")
        .arg("--json")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "`openbot secret ls` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("`openbot secret ls --json` answered something else: {e}"))
}

/// Store a credential under a name.
///
/// The value is written to the child's stdin and is not kept, logged, or
/// echoed anywhere, including in the error if this fails.
///
/// # Errors
/// If the binary cannot be run or refuses the name. The error never contains
/// the value.
pub async fn set(openbot: &Path, home: &Path, name: &str, value: &str) -> anyhow::Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow::anyhow!("a credential needs a name"));
    }
    let mut child = base(openbot, home)
        .arg("set")
        .arg(name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;

    {
        use tokio::io::AsyncWriteExt as _;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin on `openbot secret set`"))?;
        stdin
            .write_all(value.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("could not hand over the value: {e}"))?;
        // Dropped here so the child sees EOF; otherwise `set` never returns.
        drop(stdin);
    }

    let out = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow::anyhow!("`openbot secret set` did not finish: {e}"))?;
    if !out.status.success() {
        // Only stderr, and only the binary's own words. The value must never
        // be interpolated into a message.
        return Err(anyhow::anyhow!(
            "could not store `{name}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Forget a credential.
///
/// # Errors
/// If the binary cannot be run or does not know the name.
pub async fn remove(openbot: &Path, home: &Path, name: &str) -> anyhow::Result<()> {
    let out = base(openbot, home)
        .arg("rm")
        .arg(name)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "could not forget `{name}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn base(openbot: &Path, home: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("secret")
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_HUB_URL");
    #[cfg(windows)]
    {
        // Otherwise storing a credential flashes a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
