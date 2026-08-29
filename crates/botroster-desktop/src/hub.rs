//! Reachability check for the computer behind the hub URL.
//!
//! `botroster acp` connects to the hub lazily, per turn, so the ACP handshake
//! succeeds against a hub that is wrong or down and the failure surfaces only
//! when the first message is sent. A status line that says "connected" should
//! mean the computer is actually there, so this module asks up front using the
//! command that already answers the question: `botroster tools` lists what the
//! bound computer serves and fails with the reason when nothing is there.
//!
//! This is one check at one moment. A hub that goes away afterwards is
//! reported by the turn that hits it.
//!
//! The module also starts a computer when there is none. An installed window
//! whose answer to "no computer" is "open a terminal and type `botroster up`"
//! is not an application, so the window runs that command itself. What it
//! starts, it owns and stops; what was already running belongs to whoever
//! started it and is left alone.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// What the hub is serving, or why nothing is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The computer answered, with this many tools.
    Serving(usize),
    /// It did not, for this reason: the binary's own words, which name the
    /// refused connection rather than paraphrasing it.
    Unreachable(String),
    /// A hub answered and would not have this client.
    ///
    /// Held apart from [`Self::Unreachable`] because the two call for opposite
    /// responses. Nothing listening is a reason to start a computer; a hub that
    /// said no is a reason to report — it is already there, holding the port,
    /// and starting a second one on top fails on the bind with "address in
    /// use", which describes neither the cause nor the fix.
    ///
    /// `botroster up` has the same distinction in `hub_or_start`, and it was
    /// added there first. This variant is the window's half of it; without it,
    /// the fix was in one of the two places a person meets it.
    Refused(String),
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
pub async fn reach(botroster: &Path, hub: &str, home: &Path) -> anyhow::Result<Reach> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("tools")
        .arg("--hub")
        .arg(hub)
        .env("NO_COLOR", "1")
        // No `BOTROSTER_HOME` scrub, unlike this crate's other children: `botroster
        // tools` declares no home argument, so an ambient one has no effect.
        // Held by `the_children_the_window_does_not_scrub_read_no_home`.
        .env_remove("BOTROSTER_HUB_URL");
    // `botroster tools` has no home argument either, so it cannot find the
    // token by itself; the window is the only thing here that knows which home
    // the hub was started on. See `token_at`.
    if let Some(t) = token_at(home) {
        cmd.env(botroster_proto::HUB_TOKEN_ENV, t);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Bounded, because "refused" is not the only way to fail. Something that
    // is listening on the port but is not a hub accepts the connection and
    // then says nothing, and the ask would never return: the window would sit
    // on Connect with no error and no computer, forever. A wrong port is far
    // more likely to hit some other service than to hit nothing at all.
    let out = match tokio::time::timeout(ASK_PATIENCE, cmd.output()).await {
        Ok(r) => r.map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?,
        Err(_) => {
            return Ok(Reach::Unreachable(format!(
                "no answer from {hub} within {}s: something is listening there, but it is not a computer",
                ASK_PATIENCE.as_secs()
            )));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(line) = refusal(&stderr) {
            return Ok(Reach::Refused(line));
        }
        return Ok(Reach::Unreachable(reason(&stderr)));
    }
    // One line per tool. The count is a more informative summary than a bare
    // "ok".
    let listed = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    Ok(Reach::Serving(listed))
}

/// The token a child must present to reach the hub whose home is `home`.
///
/// Read at the moment of use, never captured earlier. `botroster up` writes
/// this file as it starts, so a value taken before [`start`] is `None` — and
/// `start`'s own poll loop would then present that `None` to every probe of
/// the very hub it is waiting for, and wait out the full patience for a hub
/// that came up seconds in.
///
/// Every site that spawns a `botroster` child which talks to the hub must set
/// this when the home has one, and must leave the inherited variable alone
/// when it does not.
///
/// The second half is a deliberate exception to this crate's usual discipline,
/// which is that the window decides its children's environment rather than the
/// shell that launched it. The window can only decide what it knows, and the
/// token belongs to whichever home the *hub* was started on — which is
/// `home` whenever this window started it, and unknowable when somebody points
/// Connect at a hub on another machine. Scrubbing would close the only channel
/// that case has. Setting `BOTROSTER_HUB_TOKEN` before launching the window is
/// therefore how a foreign hub is reached today; that it has no UI is recorded
/// in `.claude/product-review/BACKLOG.md`, not papered over here.
///
/// `tests/environment.rs` sweeps the sites.
#[must_use]
pub fn token_at(home: &Path) -> Option<String> {
    botroster_proto::hub_token_in(home)
}

/// A computer this process started, and is therefore responsible for.
///
/// Dropping it kills the child. That is the point: the window starting a
/// background daemon that outlives it would leave an orphan holding the
/// workspace lock, and the next launch would fail to start one with no
/// explanation a person could act on.
#[derive(Debug)]
pub struct Started(tokio::process::Child);

impl Drop for Started {
    fn drop(&mut self) {
        // Best effort by necessity: `Drop` cannot await, and a child that has
        // already exited returns an error here that means nothing to anybody.
        let _ = self.0.start_kill();
    }
}

/// Start a computer and wait until it serves.
///
/// Returns once the hub answers, so the caller can proceed knowing there is
/// something behind it rather than racing the daemon's startup.
///
/// Only ever called after [`reach`] reports nothing there. Starting a second
/// computer over a running one would fail on the workspace lock, and the
/// failure would be reported as though the first one were broken.
///
/// # Errors
/// If the binary cannot be run, if it exits while starting, or if it does not
/// serve within `patience`. The last case returns the hub's own refusal rather
/// than a timeout message, because "connection refused" tells a person what to
/// check and "timed out" does not.
pub async fn start(
    botroster: &Path,
    home: &Path,
    hub: &str,
    patience: Duration,
) -> anyhow::Result<Started> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("up")
        .arg("--home")
        .arg(home)
        // Bound to whatever the hub URL names, not to the default. A window
        // pointed at a non-default port would otherwise start a computer on
        // 8443 and then fail to reach it, reporting "refused" about a hub it
        // had just started itself.
        .arg("--bind")
        .arg(bind_of(hub))
        .env("NO_COLOR", "1")
        // The window passes both explicitly; an ambient value from whatever
        // shell launched the window must not decide where a person's Bots
        // live. Held by `the_children_the_window_does_not_scrub_read_no_home`.
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL")
        // Nothing reads this child's output. Inheriting the window's handles
        // on Windows keeps a console alive behind the app.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    let mut started = Started(child);

    // Poll rather than sleep a fixed interval: a warm start serves in well
    // under a second and a cold one on a slow disk takes several, so any
    // single wait is either too long for everybody or too short for somebody.
    let deadline = tokio::time::Instant::now() + patience;
    // Assigned by every arm below before the deadline check reads it; an
    // initial value here would be dead and would hide that.
    let mut last;
    loop {
        // An exit while starting is the informative failure: a port already
        // taken, a locked workspace, a home that cannot be written. Its own
        // words beat anything this function could invent.
        if let Ok(Some(status)) = started.0.try_wait() {
            let why = drain(&mut started.0).await;
            anyhow::bail!(
                "the computer stopped while starting ({status}){}",
                if why.is_empty() {
                    String::new()
                } else {
                    format!(": {why}")
                }
            );
        }
        match reach(botroster, hub, home).await {
            Ok(Reach::Serving(_)) => return Ok(started),
            // Kept in the loop rather than bailed on. `up` writes its token
            // before it binds, so a refusal here should not happen — but if it
            // ever did, the child is still coming up and one early probe is a
            // bad reason to give up. Carried into `last`, so the deadline
            // message says the hub refused us rather than that it was silent.
            Ok(Reach::Refused(why) | Reach::Unreachable(why)) => last = why,
            Err(e) => last = e.to_string(),
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the computer did not answer at {hub} within {}s: {last}",
                patience.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// How long a single ask waits before calling the hub unreachable.
///
/// Generous enough for a busy machine, short enough that the polling in
/// [`start`] keeps making progress against its own deadline.
const ASK_PATIENCE: Duration = Duration::from_secs(10);

/// The `host:port` a hub URL names, for `botroster up --bind`.
///
/// Deliberately not a URL-parsing dependency: the shape is fixed
/// (`ws://host:port/path`) and the fallback is the same default the binary
/// would have used anyway, so a URL this cannot read costs nothing beyond
/// binding where the caller was already going to look.
fn bind_of(hub: &str) -> String {
    hub.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .filter(|hp| hp.contains(':'))
        .unwrap_or("127.0.0.1:8443")
        .to_owned()
}

/// Whatever the child managed to say before it stopped.
async fn drain(child: &mut tokio::process::Child) -> String {
    use tokio::io::AsyncReadExt as _;
    let Some(mut err) = child.stderr.take() else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = err.read_to_end(&mut buf).await;
    reason(&String::from_utf8_lossy(&buf))
}

/// The useful part of the binary's error output.
///
/// The first line carries the cause. The rest is a backtrace-shaped tail that
/// would make a status line unreadable.
/// The line saying a hub refused us, if the child printed one.
///
/// Not [`reason`], which takes the *first* non-empty line: a refusal is
/// reported as "could not reach the computer at ws://…" and then the sentence
/// that says what to do about it, so the first line is the least useful part of
/// three. This finds the informative one by the marker both crates agree on.
fn refusal(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| l.contains(botroster_proto::REFUSED_PREFIX))
        .map(str::to_owned)
}

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

    /// The bind follows the hub URL, so a window pointed at a non-default
    /// port starts a computer there rather than on the default and then
    /// reporting the default as refused.
    #[test]
    fn the_bind_address_comes_from_the_hub_url() {
        assert_eq!(bind_of("ws://127.0.0.1:8443/v1/tools"), "127.0.0.1:8443");
        assert_eq!(bind_of("ws://127.0.0.1:9000/v1/tools"), "127.0.0.1:9000");
        assert_eq!(bind_of("ws://0.0.0.0:1234/v1/tools"), "0.0.0.0:1234");
    }

    /// A URL with no port falls back to the binary's own default rather than
    /// binding something invented, which would fail to start at all.
    #[test]
    fn a_url_without_a_port_falls_back_to_the_default() {
        assert_eq!(bind_of("ws://example.test/v1/tools"), "127.0.0.1:8443");
        assert_eq!(bind_of("not a url"), "127.0.0.1:8443");
        assert_eq!(bind_of(""), "127.0.0.1:8443");
    }

    #[test]
    fn serving_and_unreachable_are_told_apart() {
        assert!(Reach::Serving(16).is_serving());
        assert!(!Reach::Unreachable("refused".into()).is_serving());
    }
}
