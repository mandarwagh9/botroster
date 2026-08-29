//! The Agent Computer, started the way the window starts it.
//!
//! The docs are explicit that a password, a 2FA code or a CAPTCHA is handled
//! by taking control of the computer rather than typing it into chat. botroster
//! enforces that lock in the hub, and `botroster-cli/tests/viewer.rs` proves the
//! enforcement works when the viewer is started from a terminal.
//!
//! This asks the question that matters for the client: **is the viewer the
//! window opens the same viewer, with the same lock?** Two well-tested halves
//! and nothing joining them is the shape of every reachability bug this
//! project has had, and this one is a security property.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use botroster_desktop::viewer;

use common::up::Up;

/// One HTTP request, hand-rolled so the test needs no client library.
fn post(port: u16, path: &str, body: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect to the viewer");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

/// Ask the computer to do something, the way an agent would: through the hub,
/// past the policy gate. `Err` is the refusal text.
fn agent_says(hub: &str, home: &std::path::Path) -> Result<String, String> {
    let out = std::process::Command::new(common::up::botroster())
        .arg("call")
        .arg("fs.list")
        .arg("{}")
        .arg("--hub")
        .arg(hub)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HUB_URL")
        // `botroster call` declares no `--home`, so it cannot find the token
        // the hub requires. The window's own children are given it by
        // `hub::token_at`; this probe is given it here.
        .envs(common::up::token_in(home))
        .output()
        .expect("could not run botroster call");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// The port and key out of the address the viewer announced.
fn parts(url: &str) -> (u16, String) {
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .expect("a loopback url");
    let (port, rest) = rest.split_once('/').expect("a path");
    let key = rest.split("?k=").nth(1).expect("a key").to_owned();
    (port.parse().expect("a port"), key)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_can_open_the_computer_and_lock_the_bot_out_of_it() {
    let up = Up::start().expect("botroster up");
    let mut view = viewer::open(&common::up::botroster(), &up.hub, &up.home)
        .await
        .expect("the window could not open the computer");

    assert!(
        view.url().starts_with("http://127.0.0.1:"),
        "the viewer must never be reachable off this machine, got {}",
        view.url()
    );
    assert!(
        view.url().contains("?k="),
        "loopback is not a boundary in a browser; the key is what makes it one: {}",
        view.url()
    );
    assert!(view.alive(), "the viewer died before it was shown");

    let (port, key) = parts(view.url());

    // Without this the test could pass on a computer that was already
    // unusable, which would say nothing about the lock at all.
    assert!(
        agent_says(&up.hub, &up.home).is_ok(),
        "the computer was already refusing work before anyone took control"
    );

    let taken = post(
        port,
        &format!("/takeover?k={key}"),
        r#"{"reason":"entering a 2FA code"}"#,
    );
    assert!(
        taken.contains(" 200 "),
        "taking control through the window's viewer failed: {taken}"
    );

    let refused = agent_says(&up.hub, &up.home)
        .expect_err("the Bot kept working while a person held the computer");
    assert!(
        refused.contains("taken over"),
        "the Bot was not locked out: {refused}"
    );
    // The reason travels with the refusal. A Bot told only "denied" can say
    // nothing useful about it, and neither can whoever reads the log.
    assert!(
        refused.contains("2FA"),
        "the refusal should carry why control was taken: {refused}"
    );

    // Giving it back returns the computer, or "take control" would be a
    // one-way door and nobody would use it twice.
    let released = post(port, &format!("/release?k={key}"), "{}");
    assert!(
        released.contains(" 200 "),
        "giving control back failed: {released}"
    );
    assert!(
        agent_says(&up.hub, &up.home).is_ok(),
        "the computer stayed locked after control was given back"
    );
}

/// Closing the panel must close the port. A viewer left listening behind a
/// window is one that can still drive a signed-in computer.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_viewer_stops_it_serving() {
    let up = Up::start().expect("botroster up");
    let view = viewer::open(&common::up::botroster(), &up.hub, &up.home)
        .await
        .expect("open the computer");
    let (port, _key) = parts(view.url());
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "the viewer is not serving"
    );

    drop(view);

    // The process is killed on drop; give the OS a moment to release the port.
    let gone = (0..60).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        TcpStream::connect(("127.0.0.1", port)).is_err()
    });
    assert!(
        gone,
        "the viewer kept serving after the window let go of it"
    );
}

/// A hub that is not there is an error the window can show, not a wait.
#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_that_cannot_reach_a_hub_says_so() {
    // Nothing was started, so no home holds a token for this address.
    let nowhere = tempfile::tempdir().expect("a temp dir");
    let err = viewer::open(
        &common::up::botroster(),
        "ws://127.0.0.1:1/v1/tools",
        nowhere.path(),
    )
    .await
    .expect_err("a viewer with no hub should not report success");
    let text = err.to_string();
    assert!(
        text.contains("exited early") || text.contains("never said"),
        "the error should say what went wrong, got {text}"
    );
}

/// **A viewer that died must stop claiming to be alive.**
///
/// The panel is an iframe onto another process: when it dies the frame keeps
/// showing whatever it last painted, which looks exactly like a computer
/// sitting idle. `alive` said so in its own doc comment from the day it was
/// written, and nothing asked it except when re-opening, the one case the
/// comment was not about.
///
/// Killed rather than closed, because closing it politely is the case that was
/// already covered and is not the one that goes wrong.
#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_that_was_killed_stops_reporting_itself_alive() {
    let up = Up::start().expect("botroster up");
    let mut view = viewer::open(&common::up::botroster(), &up.hub, &up.home)
        .await
        .expect("open the computer");
    assert!(view.alive(), "it should be up before anything kills it");

    let pid = view.pid();
    #[cfg(windows)]
    let killed = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    #[cfg(not(windows))]
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    assert!(killed.is_ok(), "could not end the viewer to test the check");

    // `try_wait` reaps it; give the OS a moment to make the exit visible.
    let noticed = (0..60).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        !view.alive()
    });
    assert!(
        noticed,
        "the viewer was killed and still reported itself as serving, so the panel would keep showing a still picture of a computer that is gone"
    );
}
