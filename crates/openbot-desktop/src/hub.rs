//! Reachability check for the computer behind the hub URL.
//!
//! `openbot acp` connects to the hub lazily, per turn, so the ACP handshake
//! succeeds against a hub that is wrong or down and the failure surfaces only
//! when the first message is sent. A status line that says "connected" should
//! mean the computer is actually there, so this module asks up front using the
//! command that already answers the question: `openbot tools` lists what the
//! bound computer serves and fails with the reason when nothing is there.
//!
//! This is one check at one moment. A hub that goes away afterwards is
//! reported by the turn that hits it.

use std::path::Path;

/// What the hub is serving, or why nothing is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The computer answered, with this many tools.
    Serving(usize),
    /// It did not, for this reason: the binary's own words, which name the
    /// refused connection rather than paraphrasing it.
    Unreachable(String),
}

impl Reach {
    #[must_use]
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Serving(_))
    }
}

/// Ask the hub what it serves.
///
/// # Errors
/// Only if the binary cannot be run at all. A hub that refuses is not an
/// error here but an answer; the caller needs to tell the two apart, since one
/// means the installation is broken and the other means the computer is not
/// running yet.
pub async fn reach(openbot: &Path, hub: &str) -> anyhow::Result<Reach> {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("tools")
        .arg("--hub")
        .arg(hub)
        .env("NO_COLOR", "1")
        // No `OPENBOT_HOME` scrub, unlike this crate's other children: `openbot
        // tools` declares no home argument, so an ambient one has no effect.
        // Held by `the_children_the_window_does_not_scrub_read_no_home`.
        .env_remove("OPENBOT_HUB_URL");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;

    if !out.status.success() {
        return Ok(Reach::Unreachable(reason(&String::from_utf8_lossy(
            &out.stderr,
        ))));
    }
    // One line per tool. The count is a more informative summary than a bare
    // "ok".
    let listed = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    Ok(Reach::Serving(listed))
}

/// The useful part of the binary's error output.
///
/// The first line carries the cause. The rest is a backtrace-shaped tail that
/// would make a status line unreadable.
fn reason(stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no reason given");
    first.trim_start_matches("Error:").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_is_the_first_line_without_its_prefix() {
        let stderr = "Error: connect: IO error: the target machine actively refused it\n\
                      note: run with RUST_BACKTRACE=1\n";
        assert_eq!(
            reason(stderr),
            "connect: IO error: the target machine actively refused it"
        );
    }

    /// Empty stderr still produces readable text rather than an empty status
    /// line.
    #[test]
    fn silence_still_says_something() {
        assert_eq!(reason(""), "no reason given");
        assert_eq!(reason("   \n\n"), "no reason given");
    }

    #[test]
    fn serving_and_unreachable_are_told_apart() {
        assert!(Reach::Serving(16).is_serving());
        assert!(!Reach::Unreachable("refused".into()).is_serving());
    }
}
