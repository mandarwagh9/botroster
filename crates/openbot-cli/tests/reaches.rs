//! What OPENBOT knows must actually reach the model.
//!
//! Most tests ask whether a piece works. This asks whether it is reached,
//! which is a different question: a durable volume that `openbot up` never
//! writes to, or a skill provider that is skipped when the directory is empty
//! at boot, both pass their own tests while doing nothing for a real run. The
//! only way to catch that class is to look at what a real run sends a real
//! endpoint, so this stands up a vendor that records the request and reads
//! what arrived.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;

const OPENBOT: &str = env!("CARGO_BIN_EXE_openbot");

/// A vendor that records what it was sent and ends every turn.
fn recording_vendor() -> (String, Arc<Mutex<Vec<String>>>) {
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
    (format!("http://{addr}"), seen)
}

fn serve_one(mut s: std::net::TcpStream, sink: &Mutex<Vec<String>>) {
    // Headers first, then exactly Content-Length bytes. A request carrying the
    // tool catalogue is several kilobytes, so a fixed-size read would truncate
    // the very thing under test.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&head).to_lowercase();
    let len: usize = text
        .split("content-length:")
        .nth(1)
        .and_then(|rest| rest.split("\r\n").next())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; len];
    if s.read_exact(&mut body).is_ok() {
        sink.lock()
            .unwrap()
            .push(String::from_utf8_lossy(&body).to_string());
    }

    let reply = br#"{"choices":[{"message":{"content":"noted"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
    let out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    let _ = s.write_all(out.as_bytes());
    let _ = s.write_all(reply);
}

/// Read the hub URL out of a log `openbot up` is writing.
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
                "`openbot up` exited early ({status}): {}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    common::stop(child);
    panic!("`openbot up` never announced a hub url");
}

#[test]
fn a_bots_brief_its_tools_and_its_memory_all_reach_the_model() {
    let (vendor, seen) = recording_vendor();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("control-plane");
    let log = dir.path().join("up.log");

    let mut child = Command::new(OPENBOT)
        .arg("up")
        .args(["--bind", "127.0.0.1:0", "--home"])
        .arg(&home)
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HUB_URL")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_WORKSPACE")
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start openbot up");
    let hub = hub_url(&log, &mut child);

    let made = Command::new(OPENBOT)
        .args(["bot", "new", "Auditor", "--description"])
        .arg("STANDING-BRIEF always cite the invoice number")
        .arg("--home")
        .arg(&home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );

    let run = |task: &str| {
        let out = Command::new(OPENBOT)
            .args(["run", "--bot", "Auditor", "--base-url"])
            .arg(&vendor)
            .args([
                "--dialect",
                "openai",
                "--model",
                "fake",
                "--api-key-env",
                "FAKE_KEY",
                "--approve",
                "auto",
                "--home",
            ])
            .arg(&home)
            .arg(task)
            .env("OPENBOT_HUB_URL", &hub)
            .env("FAKE_KEY", "not-a-real-key")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "run failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run("the invoice number is 8891, remember it");
    run("what was the invoice number?");
    common::stop(&mut child);

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "the vendor was not asked twice");

    // A Bot that is not a prompt is a Bot whose brief is in the prompt.
    assert!(
        requests[0].contains("STANDING-BRIEF"),
        "the Bot's standing brief never reached the model"
    );

    // A tool the model is not offered may as well not exist.
    for tool in ["fs__write", "shell__exec", "bot__send"] {
        assert!(
            requests[0].contains(tool),
            "`{tool}` was never offered to the model"
        );
    }

    // Memory: the second task starts from the first one's conversation
    // rather than from nothing.
    assert!(
        requests[1].contains("8891"),
        "the second run began with no memory of the first"
    );
}
