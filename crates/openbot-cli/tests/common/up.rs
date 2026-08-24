//! A running `openbot up`, shared by the test binaries that drive the live
//! stack as shipped.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Read the hub URL out of a log file `openbot up` is writing.
///
/// A file, not a pipe. A piped stdout is inherited by every grandchild,
/// including the headless Chrome the guest launches, so killing `openbot up`
/// would leave the write end open, the reader would never see EOF, and
/// `cargo test` would hang after the test itself has finished.
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
                "`openbot up` exited early ({status}):
{}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    super::stop(child);
    panic!(
        "`openbot up` never announced a hub url:
{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

/// A running `openbot up`, killed when this drops.
pub struct Up {
    pub child: Child,
    pub hub: String,
    /// The control plane's home, shared with every command this harness runs.
    pub home: std::path::PathBuf,
    /// Where the banner was written. Read it to assert on what `up` announced.
    pub log: std::path::PathBuf,
    pub _dir: tempfile::TempDir,
}

impl Drop for Up {
    fn drop(&mut self) {
        super::stop(&mut self.child);
    }
}

impl Up {
    /// Start it, and wait for the banner rather than sleeping a guessed amount.
    ///
    /// The banner is printed only after the guest has registered, so seeing it
    /// is the same as waiting for ready. A fixed sleep would be flaky on a busy
    /// machine and slow on an idle one.
    pub fn start() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("up.log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_openbot"))
            .arg("up")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--home")
            .arg(dir.path().join("control-plane"))
            .arg("--workspace")
            .arg(dir.path().join("computer"))
            .env("NO_COLOR", "1")
            .env_remove("OPENBOT_HUB_URL")
            .env_remove("OPENBOT_HOME")
            .env_remove("OPENBOT_WORKSPACE")
            .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start openbot up");

        let hub = wait_for_hub_url(&log, &mut child);
        let home = dir.path().join("control-plane");
        Some(Up {
            child,
            hub,
            home,
            log,
            _dir: dir,
        })
    }

    /// Drive one CLI command against this stack and require it to succeed.
    pub fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_openbot"))
            .args(args)
            .env("OPENBOT_HUB_URL", &self.hub)
            // The hub's own home, so Bot, group and routine commands write
            // where this test's hub reads. Without it they default to
            // `./openbot-data` in the working directory, which `cargo test`
            // sets to the crate, so every test in this binary would share one
            // store while running in parallel, keep it between runs, and
            // leave it in the tree.
            .env("OPENBOT_HOME", &self.home)
            .env("NO_COLOR", "1")
            .output()
            .expect("could not run openbot")
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`openbot {}` failed
stdout: {}
stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}
