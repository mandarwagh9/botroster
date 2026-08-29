//! A connector configured the way a person configures one, called the way an
//! agent calls one.
//!
//! `broker.rs` proves the load-bearing property (a token reaches the remote
//! and nothing else) but it builds `ConnectorTools` itself, so it cannot
//! notice if the chain from `botroster connector add` to a tool call has come
//! apart. The boot tests only assert the list is empty on a bare home.
//!
//! So this uses the CLI to store the secret and add the connector, starts the
//! real `botroster up`, calls the tool through the hub, and asks the mock remote
//! what it received.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;

const BOTROSTER: &str = env!("CARGO_BIN_EXE_botroster");
const TOKEN: &str = "sk-live-must-not-leak-9f3c1a7d";

/// A remote MCP server that records the `Authorization` header it was sent.
fn mock_remote() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let sink = Arc::clone(&seen);

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || serve_one(stream, &sink));
        }
    });
    (format!("http://{addr}/mcp"), seen)
}

fn serve_one(mut sock: std::net::TcpStream, sink: &Mutex<Vec<String>>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match sock.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    for line in text.lines() {
        if let Some(v) = line.to_lowercase().strip_prefix("authorization:") {
            sink.lock().unwrap().push(v.trim().to_owned());
        }
    }
    let len: usize = text
        .to_lowercase()
        .split("content-length:")
        .nth(1)
        .and_then(|rest| rest.split("\r\n").next())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    let _ = sock.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).to_string();

    let reply = if body.contains("tools/list") {
        br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"create_issue","description":"File an issue","inputSchema":{"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}}]}}"#.to_vec()
    } else {
        br#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"filed ENG-1"}]}}"#
            .to_vec()
    };
    let out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    let _ = sock.write_all(out.as_bytes());
    let _ = sock.write_all(&reply);
}

fn hub_url(log: &std::path::Path, child: &mut Child) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if let Some(i) = text.find("ws://") {
                return text[i..].split_whitespace().next().unwrap_or("").to_owned();
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "`botroster up` exited early ({status}): {}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    common::stop(child);
    panic!("`botroster up` never announced a hub url");
}

#[test]
fn a_connector_added_from_the_cli_is_callable_and_keeps_its_token() {
    let (url, seen) = mock_remote();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("control-plane");
    std::fs::create_dir_all(&home).unwrap();

    // Stored the way the CLI stores it: on stdin, because a token in argv is
    // world-readable in /proc and lands in shell history.
    let mut child = Command::new(BOTROSTER)
        .args(["secret", "--home"])
        .arg(&home)
        .args(["set", "linear-token"])
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("botroster secret set");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(TOKEN.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "storing the secret failed");

    let added = Command::new(BOTROSTER)
        .args(["connector", "--home"])
        .arg(&home)
        .args(["add", "linear"])
        .arg(&url)
        .args(["--authorization", "Bearer ${linear-token}"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "adding the connector failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );

    let log = dir.path().join("up.log");
    let mut up = Command::new(BOTROSTER)
        .arg("up")
        .args(["--bind", "127.0.0.1:0", "--home"])
        .arg(&home)
        .args(["--snapshot-every", "0"])
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HUB_URL")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_WORKSPACE")
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .expect("botroster up");
    let hub = hub_url(&log, &mut up);

    let called = Command::new(BOTROSTER)
        .args([
            "call",
            "linear__create_issue",
            r#"{"title":"the roof is leaking"}"#,
            "--approve",
            "auto",
        ])
        .env("BOTROSTER_HUB_URL", &hub)
        .env("NO_COLOR", "1")
        // `botroster call` has no `--home`, so the token is passed here; the
        // hub above was started on `home`.
        .envs(common::up::token_in(&home))
        .output()
        .unwrap();
    common::stop(&mut up);

    let out = String::from_utf8_lossy(&called.stdout).to_string();
    let err = String::from_utf8_lossy(&called.stderr).to_string();
    assert!(
        called.status.success(),
        "a connector added from the CLI was not callable:\n{out}{err}"
    );
    assert!(
        out.contains("filed ENG-1"),
        "the remote's answer did not come back: {out}"
    );

    // The property the broker exists for, checked on this path rather than
    // only on the one `broker.rs` builds by hand.
    let headers = seen.lock().unwrap().clone();
    assert!(
        headers.iter().any(|h| h.contains(TOKEN)),
        "the remote never received the credential: {headers:?}"
    );
    assert!(
        !out.contains(TOKEN) && !err.contains(TOKEN),
        "the credential came back to the caller"
    );
    let banner = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !banner.contains(TOKEN),
        "the credential was printed by `botroster up`"
    );
}
