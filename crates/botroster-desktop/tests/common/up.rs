//! A running `botroster up`, shared by the BOTROSTER test binaries that drive the
//! live stack as shipped. A copy of `botroster-cli/tests/common/up.rs`, since
//! test support code cannot cross package boundaries.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Where the shipped `botroster` binary lives.
///
/// `CARGO_BIN_EXE_botroster` belongs to botroster-cli's own tests; in this crate the
/// path is computed from the workspace layout instead, or taken from
/// `BOTROSTER_BIN` when a caller has a specific binary in mind.
pub fn botroster() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("BOTROSTER_BIN") {
        return path.into();
    }
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/debug");
    path.push(if cfg!(windows) {
        "botroster.exe"
    } else {
        "botroster"
    });
    assert!(
        path.exists(),
        "no `botroster` binary at {}; build the workspace first or set BOTROSTER_BIN",
        path.display()
    );
    path
}

/// Read the hub URL out of a log file `botroster up` is writing.
///
/// A file, not a pipe. A piped stdout is inherited by every grandchild,
/// including the headless Chrome the guest launches, so killing `botroster up`
/// leaves the write end open, the reader never sees EOF, and `cargo test`
/// hangs after the test itself has finished.
pub fn wait_for_hub_url(log: &std::path::Path, child: &mut Child) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if let Some(i) = text.find("ws://") {
                return text[i..].split_whitespace().next().unwrap_or("").to_owned();
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "`botroster up` exited early ({status}):
{}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    super::stop(child);
    panic!(
        "`botroster up` never announced a hub url:
{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

/// A running `botroster up`, killed when this drops.
pub struct Up {
    pub child: Child,
    pub hub: String,
    pub home: std::path::PathBuf,
    pub _dir: tempfile::TempDir,
}

impl Drop for Up {
    fn drop(&mut self) {
        super::stop(&mut self.child);
    }
}

impl Up {
    /// Start it and wait for the banner rather than sleeping a fixed amount.
    ///
    /// The banner is printed only after the guest has registered, so seeing it
    /// is the same as waiting for ready. A fixed sleep would be flaky on a busy
    /// machine and slow on an idle one.
    pub fn start() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("control-plane");
        let log = dir.path().join("up.log");
        let mut child = Command::new(botroster())
            .arg("up")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--home")
            .arg(&home)
            .arg("--workspace")
            .arg(dir.path().join("computer"))
            .env("NO_COLOR", "1")
            .env_remove("BOTROSTER_HUB_URL")
            .env_remove("BOTROSTER_HOME")
            .env_remove("BOTROSTER_WORKSPACE")
            .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start botroster up");

        let hub = wait_for_hub_url(&log, &mut child);
        Some(Up {
            child,
            hub,
            home,
            _dir: dir,
        })
    }
}
