//! botroster guest daemon: runs inside the computer and serves tools to the hub.
//!
//! The guest is untrusted: it holds no connector credentials, and it authorises
//! once at handshake via the credential presented on the WebSocket upgrade.
//!
//! The `fs.*` tools are bounded by [`tools::Workspace`], which resolves every
//! path against the workspace root and refuses the ones that escape it.
//! **`shell.exec` is not bounded by it.** It starts a shell with the workspace
//! as its working directory and nothing else: the command may `cd` anywhere the
//! user can reach, and it inherits this process's environment. Today's guest is
//! an ordinary process running as the user — see "What does not exist yet" in
//! `CLAUDE.md` — so the confinement is one tool family's, not the process's.
//!
//! This paragraph used to read "everything it can reach on the filesystem is
//! bounded by `tools::Workspace`", which was the sentence a reader would rely on
//! when deciding what to point this at.

#![forbid(unsafe_code)]

/// Directory names a home is likely to have, for the guard in `tools.rs`.
///
/// Duplicated from `botroster-cli` rather than shared, because the dependency
/// runs the other way: the CLI knows about the guest, not the reverse. The
/// guard uses these to catch the one overlap a guest can detect on its own: a
/// workspace with a home directory inside it.
///
/// Three entries. `.botroster` is what `--home` defaults to now.
/// `botroster-data` is what it defaulted to before, and is still what somebody
/// who followed older instructions will have passed explicitly. A guard that
/// only knew the current default would stop protecting the people most likely
/// to have the overlapping layout.
///
/// `.openbot` is the third for the same reason and a sharper one: the product
/// was called OPENBOT until 0.3.1, `botroster_proto::default_home` still
/// resolves to that directory when it exists and the new one does not, so it
/// is a *live* home name and not only a historical one. Leaving it out would
/// have meant the guard silently stopped covering every machine that upgraded
/// — precisely the machines with the most in their home to lose.
///
/// `botroster-cli` has a test asserting its own default is one of these, so the
/// copy cannot drift from the value it mirrors. That test is what caught this.
pub const DEFAULT_HOME_DIRS: &[&str] = &[".botroster", "botroster-data", ".openbot"];

/// Location of the browser profile for a plain workspace: beside it, never
/// inside it.
///
/// A profile holds cookies and `Login Data`, and `fs.read` is allow-listed
/// with no approval prompt, so a profile inside the workspace would be a
/// credential store the model can read on request.
///
/// The parent is taken from the absolute path, not the argument as typed.
/// `Path::new(".").parent()` is `Some("")`, so `--workspace .` would otherwise
/// yield the relative `.botroster-browser`, which the browser resolves against the
/// process's working directory, i.e. the workspace itself. Both the CLI and
/// the standalone guest use this one function so the computation cannot
/// diverge.
///
/// A workspace at a drive root has nothing beside it, so the profile goes to
/// the temp directory rather than to a guessed relative path.
#[must_use]
pub fn profile_beside(workspace: &std::path::Path) -> std::path::PathBuf {
    let root = std::path::absolute(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    root.parent().map_or_else(
        || std::env::temp_dir().join("botroster-browser"),
        |parent| parent.join(".botroster-browser"),
    )
}

pub mod browser;
pub mod client;
pub mod tools;

pub use client::{run, run_supervised, GuestConfig};

/// Wait for any of the signals that ask this process to stop.
///
/// `tokio::signal::ctrl_c()` alone is not enough: a guest that exits without
/// tearing its browser down leaves a headless Chrome holding the profile, and
/// the next guest then waits out its full launch timeout for a port that never
/// appears.
///
/// * Unix: SIGTERM. This is what `docker stop`, `systemctl stop` and every
///   orchestrator send, and `ctrl_c` never sees it.
/// * Windows: Ctrl-Break. The default handler terminates the process before
///   any cleanup runs.
///
/// Nothing can be done about SIGKILL or `taskkill /F`; those are why
/// `botroster computer force-detach` exists.
pub async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            // Without SIGTERM, still honour Ctrl-C rather than giving up on
            // shutdown entirely.
            Err(_) => return tokio::signal::ctrl_c().await.unwrap_or(()),
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        let mut brk = match tokio::signal::windows::ctrl_break() {
            Ok(s) => s,
            Err(_) => return tokio::signal::ctrl_c().await.unwrap_or(()),
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = brk.recv() => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
pub use tools::{Context, Workspace};

#[cfg(test)]
mod profile_tests {
    use super::profile_beside;
    use std::path::{absolute, Path};

    /// A profile inside the workspace is a credential store `fs.read` can
    /// open, and `fs.read` needs no approval.
    ///
    /// `--workspace .` is the case most likely to regress. Both sides are
    /// compared through the same transform: `canonicalize` returns a `\?\`
    /// verbatim path on Windows and nothing else does, so comparing a
    /// canonical root against a relative profile would answer "not inside"
    /// for a path that is.
    #[test]
    fn a_profile_is_never_inside_the_workspace_it_serves() {
        for w in [".", "work", "./work", "a/b/c"] {
            let root = absolute(Path::new(w)).expect("an absolute root");
            let profile = absolute(profile_beside(Path::new(w))).expect("an absolute profile");
            assert!(
                !profile.starts_with(&root),
                "a workspace of `{w}` puts the profile at {profile:?}, inside {root:?}"
            );
        }
    }

    /// The profile is beside the workspace at a stable location: a profile
    /// that moved every run would lose the signed-in sessions it exists to
    /// keep.
    #[test]
    fn a_profile_sits_beside_the_workspace_and_stays_put() {
        let a = profile_beside(Path::new("/srv/project"));
        let b = profile_beside(Path::new("/srv/project"));
        assert_eq!(a, b, "the same workspace should keep the same profile");
        assert_eq!(a.file_name().unwrap(), ".botroster-browser");
        assert_eq!(
            a.parent(),
            absolute(Path::new("/srv/project")).unwrap().parent()
        );
    }
}
