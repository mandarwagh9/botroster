//! The viewer's front door, driven as shipped.
//!
//! `botroster watch` opens an HTTP port that can drive a computer signed into
//! things. A refactor that dropped the token check would look fine in review
//! and in every other test, so its defences are checked here.
//!
//! This starts the real binary and speaks HTTP to it over a socket. The
//! assertions are the three refusals and the one permission:
//!
//! * no token, or the wrong one: refused, including for the page itself
//! * a `Host` that is not loopback: refused, even with a valid token
//! * input before taking control: refused
//! * the page itself: served, given the key

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BOTROSTER: &str = env!("CARGO_BIN_EXE_botroster");

mod common;

/// A child process that is killed and reaped however the test leaves.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        common::stop(&mut self.0);
    }
}

/// One HTTP request, hand-rolled so the test needs no client library.
fn http(port: u16, request: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect to the viewer");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn get(port: u16, path: &str, host: &str) -> String {
    http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    )
}

fn post(port: u16, path: &str, body: &str) -> String {
    http(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn status(response: &str) -> u16 {
    response
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

/// Wait for a file to contain something, rather than sleeping a guess.
fn wait_for(path: &std::path::Path, needle: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if s.contains(needle) {
                return Some(s);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

struct Viewer {
    port: u16,
    key: String,
    /// So a test can drive the computer the way an agent would, and find out
    /// whether the viewer's button reached the hub's enforcement.
    hub: String,
    _watch: Proc,
    _up: Proc,
    _dir: tempfile::TempDir,
}

impl Viewer {
    /// Drive the computer the way an agent would, and say what came back.
    fn agent_says(&self) -> Result<(), String> {
        let out = Command::new(BOTROSTER)
            .args(["call", "fs.list", "{}", "--approve", "auto"])
            .env("BOTROSTER_HUB_URL", &self.hub)
            .env("NO_COLOR", "1")
            .output()
            .expect("run botroster call");
        if out.status.success() {
            return Ok(());
        }
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

/// Start a hub with a computer, then a viewer onto it.
fn start() -> Viewer {
    let dir = tempfile::tempdir().unwrap();
    let up_log = dir.path().join("up.log");
    let up = Proc(
        Command::new(BOTROSTER)
            .args(["up", "--bind", "127.0.0.1:0", "--home"])
            .arg(dir.path().join("control-plane"))
            .arg("--workspace")
            .arg(dir.path().join("computer"))
            .env("NO_COLOR", "1")
            .env_remove("BOTROSTER_HUB_URL")
            .stdout(Stdio::from(std::fs::File::create(&up_log).unwrap()))
            .stderr(Stdio::null())
            .spawn()
            .expect("botroster up"),
    );

    let banner = wait_for(&up_log, "ws://").expect("`botroster up` never announced a hub");
    let hub = banner[banner.find("ws://").unwrap()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    // Port 0: the OS picks, and the banner says which.
    let watch_log = dir.path().join("watch.log");
    let watch = Proc(
        Command::new(BOTROSTER)
            .args(["watch", "--port", "0"])
            .env("BOTROSTER_HUB_URL", &hub)
            .env("NO_COLOR", "1")
            .stdout(Stdio::from(std::fs::File::create(&watch_log).unwrap()))
            .stderr(Stdio::null())
            .spawn()
            .expect("botroster watch"),
    );

    let text = wait_for(&watch_log, "http://").expect("`botroster watch` never announced a url");
    let url = text[text.find("http://").unwrap()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    // http://127.0.0.1:PORT/?k=KEY
    let (addr, key) = url.split_once("/?k=").expect("no key in the viewer url");
    let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();

    Viewer {
        port,
        key: key.to_owned(),
        hub,
        _watch: watch,
        _up: up,
        _dir: dir,
    }
}

#[test]
fn the_page_needs_the_key_and_so_does_every_route() {
    let v = start();

    // The page itself. Serving this without the key would hand an attacker the
    // shape of the API and a same-origin place to call it from.
    assert_eq!(
        status(&get(v.port, "/", "127.0.0.1")),
        403,
        "page without a key"
    );
    assert_eq!(
        status(&get(v.port, "/frame", "127.0.0.1")),
        403,
        "frames without a key"
    );
    assert_eq!(
        status(&get(v.port, "/frame?k=guessed", "127.0.0.1")),
        403,
        "frames with the wrong key"
    );

    // With the key, the page is served and is the viewer.
    let page = get(v.port, &format!("/?k={}", v.key), "127.0.0.1");
    assert_eq!(status(&page), 200);
    assert!(page.contains("botroster"), "that is not the viewer page");
    assert!(
        page.contains("Take control"),
        "the page is missing its one control"
    );
}

#[test]
fn a_rebinding_host_is_refused_even_with_the_right_key() {
    let v = start();
    // `evil.com` resolving to 127.0.0.1 arrives with its own Host header, and
    // would otherwise be same-origin with this server: able to read the reply,
    // not merely fire and forget.
    let r = get(v.port, &format!("/frame?k={}", v.key), "evil.com");
    assert_eq!(status(&r), 403, "a rebinding host got through");
    assert!(r.contains("bad host"), "{r}");
}

#[test]
fn input_before_taking_control_is_refused() {
    let v = start();
    // The page hides the controls, but the page is not the boundary.
    let r = post(
        v.port,
        &format!("/input?k={}", v.key),
        r#"{"kind":"click","x":10,"y":10}"#,
    );
    assert_eq!(status(&r), 409, "input landed without taking control");
    assert!(r.contains("take control first"), "{r}");
}

#[test]
fn taking_control_and_giving_it_back_both_work() {
    let v = start();

    let taken = post(
        v.port,
        &format!("/takeover?k={}", v.key),
        r#"{"reason":"entering a 2FA code"}"#,
    );
    assert_eq!(status(&taken), 200, "{taken}");
    assert!(taken.contains("claimed"), "{taken}");

    let back = post(v.port, &format!("/release?k={}", v.key), "{}");
    assert_eq!(status(&back), 200, "{back}");
    assert!(back.contains("released"), "{back}");
}

#[test]
fn watching_starts_nothing() {
    let v = start();
    // No page has been opened, so there is no browser, and asking for a frame
    // must not start one. An observer that changes what it observes is not an
    // observer.
    let r = get(v.port, &format!("/frame?k={}", v.key), "127.0.0.1");
    assert_eq!(status(&r), 200, "{r}");
    assert!(
        r.contains("\"idle\":true"),
        "watching an empty computer did not report idle: {r}"
    );
}

#[test]
fn taking_control_through_the_viewer_locks_the_agent_out() {
    // Two halves, each tested, and this joins them. The tests above check
    // that `/takeover` answers 200; botrosterd's `takeover.rs` checks that a
    // held computer refuses the agent. Neither asks whether the button
    // reaches the enforcement, and this one is a security property.
    let v = start();
    assert!(
        v.agent_says().is_ok(),
        "the computer was already unusable, so this test would pass for the wrong reason"
    );

    let taken = post(
        v.port,
        &format!("/takeover?k={}", v.key),
        r#"{"reason":"entering a 2FA code"}"#,
    );
    assert_eq!(status(&taken), 200, "{taken}");

    let refused = v
        .agent_says()
        .expect_err("the agent kept working while a person held the computer");
    assert!(refused.contains("taken over"), "{refused}");
    // The reason travels with the refusal. An agent told only "denied" can say
    // nothing useful about it, and neither can whoever reads the log.
    assert!(
        refused.contains("2FA"),
        "the reason did not travel: {refused}"
    );

    let back = post(v.port, &format!("/release?k={}", v.key), "{}");
    assert_eq!(status(&back), 200, "{back}");
    assert!(
        v.agent_says().is_ok(),
        "giving the computer back did not give it back"
    );
}
