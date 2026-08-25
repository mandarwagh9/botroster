//! Shared by the test binaries that start a real `botroster up`.
//!
//! Each test binary compiles this module whole, but each uses only some of
//! it: `cli_live` wants `Up::ok`, `acp_sdk_live` only `Up::start`. Dead-code
//! analysis is per binary, so an item used by any binary warns in all the
//! others; the allow below covers that once.

#![allow(dead_code)]

pub mod up;

/// Stop a `botroster up` and everything it started.
///
/// `Child::kill` terminates that one process. Its browser is a child, and the
/// child of a killed process is not killed; it is orphaned, several Chrome
/// processes at a time, because `kill_on_drop` needs destructors and a
/// forcible kill runs none.
///
/// On Unix, SIGTERM first: `botroster up` handles it and tears the browser down
/// the way Ctrl-C would, which is both cleaner and what a person does. Windows
/// has no such signal, so ask the OS for the whole tree instead.
pub fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        // Give it a moment to take the browser with it before insisting.
        for _ in 0..40 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    let _ = child.kill();
    let _ = child.wait();
}
