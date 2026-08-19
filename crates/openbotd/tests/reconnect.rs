//! A guest has to survive the hub restarting.
//!
//! If `run` held one connection and returned when it dropped, restarting the
//! control plane would kill every computer attached to it, and the operator
//! would only find out when the next task failed with "no such server".
//! Upgrading the hub is a normal thing to do; it must not require restarting
//! every guest by hand.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::ServersListResult;
use openbot_proto::{Frame, Hello, Method, Outcome, Request, RpcId};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// Ask a hub what tool servers it has, over a fresh connection.
async fn servers(url: &str) -> Vec<String> {
    let Ok((mut sock, _)) = tokio_tungstenite::connect_async(url).await else {
        return Vec::new();
    };
    if sock
        .send(Message::Text(
            serde_json::to_string(&Hello::harness()).unwrap(),
        ))
        .await
        .is_err()
    {
        return Vec::new();
    }
    let _ack = sock.next().await;

    let id = RpcId::Num(1);
    let req = Request::new(id.clone(), Method::ServersList, Some(json!({})));
    if sock
        .send(Message::Text(Frame::Request(req).encode()))
        .await
        .is_err()
    {
        return Vec::new();
    }
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(2), sock.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(Frame::Response(r)) = Frame::decode(&t) {
                    if r.id == id {
                        if let Outcome::Result(v) = r.outcome {
                            return serde_json::from_value::<ServersListResult>(v)
                                .map(|l| {
                                    l.servers
                                        .into_iter()
                                        .map(|s| s.server_id.as_str().to_owned())
                                        .collect()
                                })
                                .unwrap_or_default();
                        }
                        return Vec::new();
                    }
                }
            }
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

async fn wait_for_guest(url: &str) -> bool {
    // Longer than the backoff ceiling. A window equal to `RECONNECT_MAX`
    // would let a guest that had escalated to its longest wait before the hub
    // came back do the right thing and still miss it, which shows up under a
    // full-suite run where the gap with no hub is wider. The backoff is
    // correct behaviour rather than something to tune away, so the window is
    // the thing that is generous: ninety seconds costs nothing when the guest
    // comes back in two.
    for _ in 0..900 {
        if servers(url).await.iter().any(|s| s == "reconnect-test") {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// A real `openbotd` process, killed and reaped however the test exits.
///
/// A subprocess, not an in-process task: aborting a task leaves the sockets it
/// already accepted open, so the guest would never notice anything happened.
/// A hub that actually dies closes every connection, which is the event under
/// test.
///
/// A bare `Child` would leak a running `openbotd` if the test panicked between
/// spawning and killing it, so the kill lives in `Drop`.
struct Hub(std::process::Child);

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Hub {
    fn stop(mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_hub(addr: &str, log: &std::path::Path) -> Hub {
    Hub(std::process::Command::new(env!("CARGO_BIN_EXE_openbotd"))
        .args(["--bind", addr, "--home"])
        .arg(log.parent().unwrap().join("control-plane"))
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::from(
            std::fs::File::create(log).unwrap(),
        ))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("could not start openbotd"))
}

#[tokio::test]
async fn a_guest_comes_back_after_the_hub_restarts() -> anyhow::Result<()> {
    // A fixed port, because the guest reconnects to the URL it was given,
    // which is the point: the hub comes back at the same address.
    let addr = "127.0.0.1:8531";
    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;

    let hub = start_hub(addr, &dir.path().join("hub1.log"));
    for _ in 0..100 {
        if !servers(&url).await.is_empty() || servers(&url).await.is_empty() {
            // Any answer at all means it is listening; an empty list is fine.
            if tokio_tungstenite::connect_async(&url).await.is_ok() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.path().join("computer"), true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "reconnect-test".into(),
        description: "a guest that should survive a restart".into(),
    };
    let guest = tokio::spawn(async move {
        let _ = openbot_guest::run_supervised(cfg, ctx).await;
    });

    assert!(wait_for_guest(&url).await, "the guest never registered");

    // The hub dies: an upgrade, a crash, a reboot of the control plane.
    hub.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        servers(&url).await.is_empty(),
        "the hub is supposed to be down"
    );

    // And comes back at the same address.
    let hub2 = start_hub(addr, &dir.path().join("hub2.log"));

    // Nobody restarted the guest. It has to find its own way back, and
    // re-announce what it serves.
    let back = wait_for_guest(&url).await;

    guest.abort();
    hub2.stop();
    assert!(back, "the computer never came back after the hub restarted");
    Ok(())
}
