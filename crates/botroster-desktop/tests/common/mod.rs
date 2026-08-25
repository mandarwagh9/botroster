//! Shared by the BOTROSTER test binaries that drive the live stack.
//!
//! A copy of the harness in `botroster-cli/tests/common`, because test-support
//! code cannot cross package boundaries and it is the same stack being
//! driven. Each test binary compiles this module whole but uses only some of
//! it; dead-code analysis is per binary, so the allow lives here.

#![allow(dead_code)]

pub mod up;

/// Stop a `botroster up` and everything it started.
///
/// `Child::kill` terminates that one process. Its browser is a child, and the
/// child of a killed process is not killed but orphaned, because
/// `kill_on_drop` needs destructors and a forcible kill runs none.
///
/// On Unix, SIGTERM first: `botroster up` handles it and tears the browser down
/// the way Ctrl-C would. Windows has no such signal, so the OS is asked to
/// kill the whole tree instead.
pub fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
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
