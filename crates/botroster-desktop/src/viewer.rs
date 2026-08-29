//! The Agent Computer, for the window to show.
//!
//! The Agent Computer is opened from a conversation to watch the shared
//! desktop and to take control for a password, a 2FA code or a CAPTCHA,
//! instead of typing any of those into chat. `botroster watch` serves the live
//! view, and the takeover lock is enforced in the hub rather than by the
//! page, so a Bot is locked out while a person holds it.
//!
//! This module starts the shipped viewer rather than building a second one. A
//! reimplementation would be a second thing to keep in step with the hub's
//! lock, and the existing viewer is the one with tests
//! (`botroster-cli/tests/viewer.rs`) covering its refusals.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// How long to wait for the viewer to announce its address before giving up.
/// It has to reach the hub first, so this is not instant.
const STARTUP: Duration = Duration::from_secs(30);

/// A running `botroster watch`, and where it is serving.
///
/// Killed on drop, so closing the panel closes the port rather than leaving a
/// viewer that can drive a signed-in computer listening behind the window.
#[derive(Debug)]
pub struct Viewer {
    child: std::process::Child,
    url: String,
    /// Holds the log file the child writes to. Dropped last.
    _dir: tempfile::TempDir,
}

impl Viewer {
    /// The address to show, key and all.
    ///
    /// The key is required. Loopback is not a boundary in a browser: any page
    /// can POST to `127.0.0.1`, and CORS stops it reading the reply, not
    /// making the request. The viewer refuses anything without the key.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether the process is still running. A viewer whose process died
    /// should not leave a panel showing a frozen last frame.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The viewer's process id.
    ///
    /// Exists so a test can end the viewer by killing the process rather than
    /// by dropping it, which exercises the failure path a crash takes.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the viewer against a hub, and wait for it to say where it is.
///
/// # Errors
/// If the binary cannot be run, exits early, or never announces an address.
pub async fn open(botroster: &Path, hub: &str, home: &Path) -> anyhow::Result<Viewer> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("watch.log");

    // A file, not a pipe. A piped stdout is inherited by every grandchild, so
    // killing the child can leave the write end open, the reader never sees
    // EOF, and whoever is waiting hangs after the work is done. See
    // `tests/common/up.rs` for the same pattern.
    let mut cmd = std::process::Command::new(botroster);
    cmd.arg("watch")
        .arg("--hub")
        .arg(hub)
        // 0 picks a free one. A fixed port would collide with a `botroster watch`
        // the person already has open in a terminal.
        .arg("--port")
        .arg("0")
        .env("NO_COLOR", "1")
        // As in `hub::reach`: `botroster watch` declares no home argument, so
        // there is no `BOTROSTER_HOME` to scrub. Held by
        // `the_children_the_window_does_not_scrub_read_no_home`.
        .env_remove("BOTROSTER_HUB_URL")
        .stdout(Stdio::from(std::fs::File::create(&log)?))
        .stderr(Stdio::null());
    // `botroster watch` opens its own connection to the hub, so it needs the
    // token as much as the agent does — and having no home argument, it cannot
    // find one for itself. See `crate::hub::token_at`.
    if let Some(t) = crate::hub::token_at(home) {
        cmd.env(botroster_proto::HUB_TOKEN_ENV, t);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;

    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if let Some(url) = std::fs::read_to_string(&log)
            .ok()
            .as_deref()
            .and_then(url_in)
        {
            return Ok(Viewer {
                child,
                url,
                _dir: dir,
            });
        }
        if let Ok(Some(status)) = child.try_wait() {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            return Err(anyhow::anyhow!(
                "`botroster watch` exited early ({status}): {}",
                said.trim()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = child.kill();
    Err(anyhow::anyhow!(
        "`botroster watch` never said where it was listening"
    ))
}

/// The address `botroster watch` announces, if it has announced one yet.
///
/// Matched on the scheme and host rather than on the surrounding prose, which
/// is a banner meant for a person and free to change.
fn url_in(text: &str) -> Option<String> {
    let at = text.find("http://127.0.0.1")?;
    let url: String = text[at..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    // Without the key the URL is not usable: the panel would load and be
    // refused. Half a URL is worse than none.
    url.contains("?k=").then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_read_out_of_the_banner() {
        let banner = "watching botroster-workspace\n\n  \
                      http://127.0.0.1:8198/?k=470cdfb7-a352-431b-afbe-53e0f4440a8f\n\n  \
                      The link carries a one-time key\n";
        assert_eq!(
            url_in(banner).as_deref(),
            Some("http://127.0.0.1:8198/?k=470cdfb7-a352-431b-afbe-53e0f4440a8f")
        );
    }

    /// An unfinished banner yields `None`, not a panic or a partial line.
    #[test]
    fn a_banner_that_has_not_arrived_yet_is_not_an_address() {
        assert_eq!(url_in(""), None);
        assert_eq!(url_in("watching botroster-workspace\n"), None);
    }

    /// A URL without the key would load and be refused, which reads to a
    /// person as the computer being broken rather than as a race.
    #[test]
    fn an_address_without_its_key_is_not_usable() {
        assert_eq!(url_in("  http://127.0.0.1:8198/\n"), None);
    }
}
