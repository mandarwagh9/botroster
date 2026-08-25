//! End-to-end: harness → hub → guest → back.
//!
//! Spins up a real `openbotd` listener on an ephemeral port, connects a real
//! `openbot-guest` tool server to it over a real WebSocket, then drives it from a
//! minimal harness client. Nothing is mocked; this suite is the P0 milestone
//! in SPEC.md §10.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::*;
use openbot_proto::{
    codes, Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, ServerId, SessionId,
    ToolCallId, ToolId,
};
use openbotd::hub::Hub;
use openbotd::policy::Policy;
use openbotd::server::Server;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A minimal harness: enough of a client to drive the hub over the wire.
struct Harness {
    sock: Sock,
    next_id: i64,
    /// Progress notifications observed while awaiting responses.
    pub progress: Vec<ToolCallProgressFrame>,
}

impl Harness {
    async fn connect(url: &str) -> anyhow::Result<(Self, HelloAck)> {
        let (mut sock, _) = connect_async(url).await?;
        sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
            .await?;
        let ack: HelloAck = match sock.next().await {
            Some(Ok(Message::Text(t))) => serde_json::from_str(&t)?,
            other => anyhow::bail!("bad handshake reply: {other:?}"),
        };
        Ok((
            Self {
                sock,
                next_id: 1,
                progress: Vec::new(),
            },
            ack,
        ))
    }

    /// Send a request and read frames until its response arrives, collecting
    /// any progress notifications seen on the way.
    async fn call(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<Outcome> {
        let id = RpcId::Num(self.next_id);
        self.next_id += 1;

        let mut req = Request::new(id.clone(), method, Some(params));
        if let Some(s) = session {
            req = req.in_session(s.clone());
        }
        self.sock
            .send(Message::Text(Frame::Request(req).encode()))
            .await?;

        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), self.sock.next())
                .await
                .map_err(|_| anyhow::anyhow!("timed out awaiting a response to {method}"))?;
            let Some(Ok(Message::Text(t))) = msg else {
                anyhow::bail!("socket closed awaiting {method}");
            };
            match Frame::decode(&t)? {
                Frame::Response(r) if r.id == id => return Ok(r.outcome),
                Frame::Notification(n) if n.parsed_method() == Some(Method::ToolCallProgress) => {
                    if let Some(p) = n.params {
                        self.progress.push(serde_json::from_value(p)?);
                    }
                }
                _ => {}
            }
        }
    }

    async fn ok(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<serde_json::Value> {
        match self.call(method, params, session).await? {
            Outcome::Result(v) => Ok(v),
            Outcome::Error(e) => anyhow::bail!("{method} failed: [{}] {}", e.code, e.message),
        }
    }
}

/// Boot a hub and a guest wired to it. Returns the ws URL and the temp
/// workspace, which must stay alive for the duration of the test.
async fn boot() -> anyhow::Result<(String, tempfile::TempDir, Arc<Hub>)> {
    // These tests are about routing, so the approval gate is opened
    // explicitly. Policy itself is covered in openbot-agent's approval suite.
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    let server = Arc::new(Server::new(Arc::clone(&hub)));
    tokio::spawn(Arc::clone(&server).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ws = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "openbot-workspace".into(),
        description: "test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ws).await;
    });

    // Wait for the guest to appear in the registry rather than sleeping blind.
    let (mut probe, _) = Harness::connect(&url).await?;
    for _ in 0..100 {
        let v = probe.ok(Method::ServersList, json!({}), None).await?;
        let list: ServersListResult = serde_json::from_value(v)?;
        if !list.servers.is_empty() {
            return Ok((url, dir, hub));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("guest never registered with the hub")
}

/// Open a session and bind the guest to it.
async fn session(h: &mut Harness) -> anyhow::Result<(SessionId, Vec<ToolDescription>)> {
    let v = h.ok(Method::SessionOpen, json!({}), None).await?;
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;

    let v = h
        .ok(
            Method::SessionBindServer,
            serde_json::to_value(SessionBindServerParams {
                server_id: ServerId::new("openbot-workspace"),
            })?,
            Some(&sid),
        )
        .await?;
    let bound: SessionBindServerResult = serde_json::from_value(v)?;
    Ok((sid, bound.tools))
}

#[tokio::test]
async fn guest_registers_and_advertises_its_catalog() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, ack) = Harness::connect(&url).await?;

    assert_eq!(
        ack.supported_protocol_versions,
        vec![openbot_proto::PROTOCOL_VERSION.to_string()]
    );
    assert!(ack
        .capabilities
        .iter()
        .any(|c| c == "session_attach_server"));

    let (_sid, tools) = session(&mut h).await?;
    let ids: Vec<_> = tools
        .iter()
        .map(|t| t.tool_id.as_str().to_owned())
        .collect();
    for expected in ["fs.list", "fs.read", "fs.write", "shell.exec"] {
        assert!(
            ids.contains(&expected.to_string()),
            "catalog missing {expected}; got {ids:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn tools_list_returns_the_bound_snapshot() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, bound) = session(&mut h).await?;

    let v = h.ok(Method::ToolsList, json!({}), Some(&sid)).await?;
    let listed: ToolsListResult = serde_json::from_value(v)?;
    assert_eq!(listed.tools.len(), bound.len());
    Ok(())
}

#[tokio::test]
async fn write_then_read_round_trips_through_the_hub() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    let call = |tool: &str, args: serde_json::Value, n: u32| {
        serde_json::to_value(ToolCallRequestParams {
            tool_id: ToolId::new(tool),
            call_id: ToolCallId::new(format!("call-{n}")),
            args,
        })
        .unwrap()
    };

    const CONTENTS: &str = "from openbot";
    let v = h
        .ok(
            Method::ToolCall,
            call(
                "fs.write",
                json!({"path":"notes/hello.txt","contents":CONTENTS}),
                1,
            ),
            Some(&sid),
        )
        .await?;
    let r: ToolCallResult = serde_json::from_value(v)?;
    assert_eq!(r.output["bytes_written"], CONTENTS.len());

    let v = h
        .ok(
            Method::ToolCall,
            call("fs.read", json!({"path":"notes/hello.txt"}), 2),
            Some(&sid),
        )
        .await?;
    let r: ToolCallResult = serde_json::from_value(v)?;
    assert_eq!(r.output["contents"], CONTENTS);

    let v = h
        .ok(
            Method::ToolCall,
            call("fs.list", json!({"path":"notes"}), 3),
            Some(&sid),
        )
        .await?;
    let r: ToolCallResult = serde_json::from_value(v)?;
    let names: Vec<_> = r.output["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("hello.txt")]);
    Ok(())
}

#[tokio::test]
async fn progress_frames_are_relayed_before_the_terminal() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("shell.exec"),
        call_id: ToolCallId::new("exec-1"),
        args: json!({ "command": "echo hello-from-the-guest" }),
    })?;
    let v = h.ok(Method::ToolCall, params, Some(&sid)).await?;
    let r: ToolCallResult = serde_json::from_value(v)?;

    assert_eq!(r.output["exit_code"], 0);
    assert!(r.output["stdout"]
        .as_str()
        .unwrap()
        .contains("hello-from-the-guest"));

    // Progress must have arrived, and all of it before the terminal: the
    // harness only returns once it sees the response, so anything collected
    // necessarily preceded it.
    assert_eq!(
        h.progress.len(),
        2,
        "expected starting + finished, got {:?}",
        h.progress
    );
    assert_eq!(h.progress[0].payload["stage"], "starting");
    assert_eq!(h.progress[1].payload["stage"], "finished");
    assert!(h
        .progress
        .iter()
        .all(|p| p.call_id == ToolCallId::new("exec-1")));
    Ok(())
}

#[tokio::test]
async fn a_path_escape_is_refused_by_the_guest() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("fs.write"),
        call_id: ToolCallId::new("escape-1"),
        args: json!({ "path": "../../pwned.txt", "contents": "nope" }),
    })?;
    match h.call(Method::ToolCall, params, Some(&sid)).await? {
        Outcome::Error(e) => {
            assert_eq!(e.code, codes::TOOL_FAILED);
            assert!(
                e.message.contains("escapes the workspace"),
                "got: {}",
                e.message
            );
        }
        Outcome::Result(v) => panic!("escape should have failed, got {v}"),
    }
    Ok(())
}

#[tokio::test]
async fn calling_without_a_bound_server_is_a_clean_error() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;

    // Session opened but never bound.
    let v = h.ok(Method::SessionOpen, json!({}), None).await?;
    let sid = serde_json::from_value::<SessionOpenResult>(v)?.session_id;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("fs.list"),
        call_id: ToolCallId::new("unbound-1"),
        args: json!({}),
    })?;
    match h.call(Method::ToolCall, params, Some(&sid)).await? {
        Outcome::Error(e) => assert_eq!(e.code, codes::NO_SERVER_BOUND),
        Outcome::Result(v) => panic!("expected NO_SERVER_BOUND, got {v}"),
    }
    Ok(())
}

#[tokio::test]
async fn an_unknown_tool_fails_the_call_not_the_connection() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("fs.rm_rf"),
        call_id: ToolCallId::new("bogus-1"),
        args: json!({}),
    })?;
    match h.call(Method::ToolCall, params, Some(&sid)).await? {
        Outcome::Error(e) => assert!(e.message.contains("unknown tool"), "got: {}", e.message),
        Outcome::Result(v) => panic!("expected failure, got {v}"),
    }

    // The connection must still work afterwards.
    let v = h.ok(Method::ToolsList, json!({}), Some(&sid)).await?;
    assert!(!serde_json::from_value::<ToolsListResult>(v)?
        .tools
        .is_empty());
    Ok(())
}

#[tokio::test]
async fn a_harness_may_not_push_a_tool_snapshot() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    // `serve` is a tool-server verb. A harness issuing it must be refused.
    match h
        .call(Method::Serve, json!({ "tools": [] }), Some(&sid))
        .await?
    {
        Outcome::Error(e) => assert_eq!(e.code, codes::FORBIDDEN),
        Outcome::Result(v) => panic!("a harness must not be able to serve; got {v}"),
    }
    Ok(())
}

/// The reverse direction, which matters more.
///
/// The guest is the least-trusted thing connected to the hub: it runs the
/// tools a model chose, against pages a model opened. If a compromised one
/// could act as a harness it would not need to escape the sandbox at all: it
/// could open its own session, call tools on another computer, or take one
/// over and lock the person out of their own keyboard. The harness-cannot-
/// `serve` direction above is the one where the worst outcome is a confused
/// tool list.
#[tokio::test]
async fn a_tool_server_may_not_act_as_a_harness() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;

    // Connect as a tool server rather than a harness.
    let (mut sock, _) = connect_async(&url).await?;
    let hello = Hello::tool_server("impostor").with_description("a guest with ideas");
    sock.send(Message::Text(serde_json::to_string(&hello)?))
        .await?;
    let _ack: HelloAck = match sock.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t)?,
        other => anyhow::bail!("bad handshake: {other:?}"),
    };

    // Everything a harness may do and a guest may not. `session.open` first,
    // because without a session the rest would be refused for the wrong
    // reason and the test would pass while proving nothing.
    for (n, method, params) in [
        (1, Method::SessionOpen, json!({})),
        (2, Method::ServersList, json!({})),
        (
            3,
            Method::ToolCall,
            json!({ "call_id": "c1", "tool_id": "fs.list", "args": { "path": "." } }),
        ),
        (
            4,
            Method::ComputerTakeover,
            json!({ "server_id": "openbot-workspace", "reason": "mine now" }),
        ),
    ] {
        let req = Request::new(RpcId::Num(n), method, Some(params));
        sock.send(Message::Text(Frame::Request(req).encode()))
            .await?;
        let Some(Ok(Message::Text(t))) = sock.next().await else {
            anyhow::bail!("socket closed answering {method}");
        };
        let Frame::Response(r) = Frame::decode(&t)? else {
            anyhow::bail!("expected a response to {method}");
        };
        match r.outcome {
            Outcome::Error(e) => assert_eq!(
                e.code,
                codes::FORBIDDEN,
                "`{method}` was refused for the wrong reason: {}",
                e.message
            ),
            Outcome::Result(v) => {
                panic!("a tool server was allowed to call `{method}`: {v}")
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_unknown_method_is_reported_in_the_legacy_shape() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;

    let id = RpcId::Num(9999);
    let raw = serde_json::json!({
        "jsonrpc": "2.0", "id": 9999, "method": "does.not.exist", "params": {}
    });
    h.sock.send(Message::Text(raw.to_string())).await?;

    let Some(Ok(Message::Text(t))) = h.sock.next().await else {
        anyhow::bail!("no reply")
    };
    let Frame::Response(Response {
        outcome: Outcome::Error(e),
        id: got,
        ..
    }) = Frame::decode(&t)?
    else {
        anyhow::bail!("expected an error response, got {t}")
    };
    assert_eq!(got, id);
    assert_eq!(e.code, codes::METHOD_NOT_FOUND);
    // Clients in the wild sniff this exact prefix; keep the shape.
    assert!(
        e.message.starts_with("unknown method `"),
        "got: {}",
        e.message
    );
    Ok(())
}

#[tokio::test]
async fn two_sessions_on_one_harness_stay_independent() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (a, _) = session(&mut h).await?;
    let (b, _) = session(&mut h).await?;
    assert_ne!(a, b);

    let write = |n: u32, path: &str| {
        serde_json::to_value(ToolCallRequestParams {
            tool_id: ToolId::new("fs.write"),
            call_id: ToolCallId::new(format!("multi-{n}")),
            args: json!({ "path": path, "contents": "x" }),
        })
        .unwrap()
    };
    h.ok(Method::ToolCall, write(1, "a.txt"), Some(&a)).await?;
    h.ok(Method::ToolCall, write(2, "b.txt"), Some(&b)).await?;

    // Both sessions share one computer, which is the documented behaviour and
    // the reason Bots are not a security boundary.
    let v = h
        .ok(
            Method::ToolCall,
            serde_json::to_value(ToolCallRequestParams {
                tool_id: ToolId::new("fs.list"),
                call_id: ToolCallId::new("multi-3"),
                args: json!({}),
            })?,
            Some(&a),
        )
        .await?;
    let r: ToolCallResult = serde_json::from_value(v)?;
    let names: Vec<String> = r.output["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"a.txt".to_string()) && names.contains(&"b.txt".to_string()));
    Ok(())
}

#[tokio::test]
async fn a_failed_call_does_not_leak_routing_state() -> anyhow::Result<()> {
    let (url, _dir, hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    // Every entry in the in-flight map must be retired by a terminal response.
    // A failed tool is an ordinary terminal, not an exceptional one; reading
    // the call id back off a success payload would silently leak every
    // failure.
    for n in 0..25 {
        let params = serde_json::to_value(ToolCallRequestParams {
            tool_id: ToolId::new("fs.read"),
            call_id: ToolCallId::new(format!("leak-{n}")),
            args: json!({ "path": "../outside-the-workspace" }),
        })?;
        match h.call(Method::ToolCall, params, Some(&sid)).await? {
            Outcome::Error(_) => {}
            Outcome::Result(v) => panic!("escape should have failed, got {v}"),
        }
    }

    assert_eq!(
        hub.inflight_calls().await,
        0,
        "in-flight calls leaked after failures"
    );
    assert_eq!(
        hub.pending_relays().await,
        0,
        "relays leaked after failures"
    );

    // And the same must hold for the success path.
    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("fs.write"),
        call_id: ToolCallId::new("leak-ok"),
        args: json!({ "path": "fine.txt", "contents": "ok" }),
    })?;
    h.ok(Method::ToolCall, params, Some(&sid)).await?;
    assert_eq!(hub.inflight_calls().await, 0);
    assert_eq!(hub.pending_relays().await, 0);
    Ok(())
}

#[tokio::test]
async fn progress_arrives_while_the_tool_is_still_running() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;
    let (sid, _) = session(&mut h).await?;

    // Buffering progress and flushing it at the end would satisfy the ordering
    // rule while giving the caller nothing for the length of a slow command.
    // Assert on timing, which is the property that matters: the first
    // progress frame must land well before the terminal.
    let slow = if cfg!(windows) {
        "ping -n 3 127.0.0.1 >NUL"
    } else {
        "sleep 2"
    };
    let params = serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("shell.exec"),
        call_id: ToolCallId::new("slow-1"),
        args: json!({ "command": slow }),
    })?;

    let started = std::time::Instant::now();
    let mut first_progress_at = None;
    let id = RpcId::Num(h.next_id);
    h.next_id += 1;
    let req = Request::new(id.clone(), Method::ToolCall, Some(params)).in_session(sid);
    h.sock
        .send(Message::Text(Frame::Request(req).encode()))
        .await?;

    loop {
        let Some(Ok(Message::Text(t))) = h.sock.next().await else {
            anyhow::bail!("socket closed")
        };
        match Frame::decode(&t)? {
            Frame::Notification(n) if n.parsed_method() == Some(Method::ToolCallProgress) => {
                first_progress_at.get_or_insert_with(|| started.elapsed());
            }
            Frame::Response(r) if r.id == id => {
                let terminal_at = started.elapsed();
                let progress_at =
                    first_progress_at.expect("no progress frame arrived before the terminal");
                let lead = terminal_at.saturating_sub(progress_at);
                assert!(
                    lead > Duration::from_millis(500),
                    "progress was batched with the terminal: progress at {progress_at:?}, \
                     terminal at {terminal_at:?} (lead {lead:?})"
                );
                return Ok(());
            }
            _ => {}
        }
    }
}

/// A client cannot issue the methods the hub sends to a guest.
///
/// `required_role` returns `None` for these because they are not dispatched
/// as incoming requests at all; the dispatch refuses anything it does not
/// name. That is the claim under test: `tool_call_request` is how the hub
/// tells a guest to run something, and it is issued after `tool_call` has
/// evaluated policy. A harness able to send one directly would reach the
/// guest with no gate in front of it: no approval, no rule, no record.
///
/// An unparseable method name takes a different branch and a different
/// message (`an_unknown_method_is_reported_in_the_legacy_shape`). This is the
/// valid-but-not-dispatched case, quantified over every method in that group
/// rather than a chosen one, so a method added to the group is covered by
/// being added.
#[tokio::test]
async fn a_harness_cannot_issue_the_hubs_own_verbs() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut h, _) = Harness::connect(&url).await?;

    // Every wire name the hub uses to drive a guest or answer itself.
    const HUB_ONLY: [&str; 9] = [
        "tool_call_request",
        "approval.request",
        "secret.request",
        "session.bind",
        "session.unbind",
        "tool_call_progress",
        "tools_changed",
        "hook.reply",
        "hook",
    ];

    for (n, wire) in HUB_ONLY.iter().enumerate() {
        let id = RpcId::Num(7000 + n as i64);
        let raw = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": wire, "params": {},
        });
        h.sock.send(Message::Text(raw.to_string())).await?;

        let Some(Ok(Message::Text(t))) = h.sock.next().await else {
            anyhow::bail!("no reply to `{wire}`")
        };
        let Frame::Response(Response {
            outcome, id: got, ..
        }) = Frame::decode(&t)?
        else {
            anyhow::bail!("expected a response to `{wire}`, got {t}")
        };
        assert_eq!(got, id, "the reply to `{wire}` is for another request");
        let Outcome::Error(e) = outcome else {
            anyhow::bail!("`{wire}` was accepted from a harness; it reaches a guest ungated")
        };
        assert_eq!(
            e.code,
            codes::METHOD_NOT_FOUND,
            "`{wire}` was refused for the wrong reason: {}",
            e.message
        );
    }
    Ok(())
}

/// Every method the table reserves for a harness is refused to a guest.
///
/// `a_tool_server_may_not_act_as_a_harness` covers the four most dangerous
/// by hand. This covers all of them, by walking `Method::ALL` and asking the
/// table itself which are restricted, so a method added to the harness-only
/// group is covered the moment it is added.
///
/// The claim is `required_role`'s own: a guest issuing these would not need
/// to escape its sandbox to do harm. It could open its own session, drive
/// another computer, or take one over and lock the person out of their own
/// keyboard.
#[tokio::test]
async fn every_harness_only_method_is_refused_to_a_guest() -> anyhow::Result<()> {
    let (url, _dir, _hub) = boot().await?;
    let (mut sock, _) = connect_async(&url).await?;
    sock.send(Message::Text(serde_json::to_string(
        &Hello::tool_server("impostor").with_description("a guest with ideas"),
    )?))
    .await?;
    let _ack: HelloAck = match sock.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t)?,
        other => anyhow::bail!("bad handshake: {other:?}"),
    };

    let restricted: Vec<Method> = Method::ALL
        .iter()
        .copied()
        .filter(|m| {
            openbotd::hub::required_role(*m) == Some(openbot_proto::ConnectionKind::Harness)
        })
        .collect();
    assert!(
        restricted.len() >= 12,
        "the table restricts {} methods, fewer than expected; check the group \
         was not emptied by accident",
        restricted.len()
    );

    for (n, m) in restricted.iter().enumerate() {
        let id = RpcId::Num(8000 + n as i64);
        sock.send(Message::Text(
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": m.as_wire_str(), "params": {},
            })
            .to_string(),
        ))
        .await?;

        let Some(Ok(Message::Text(t))) = sock.next().await else {
            anyhow::bail!("no reply to `{m}`")
        };
        let Frame::Response(Response {
            outcome, id: got, ..
        }) = Frame::decode(&t)?
        else {
            anyhow::bail!("expected a response to `{m}`, got {t}")
        };
        assert_eq!(got, id, "the reply to `{m}` is for another request");
        let Outcome::Error(e) = outcome else {
            anyhow::bail!("a guest was allowed to issue `{m}`")
        };
        // `FORBIDDEN`, not merely an error. Sending `{}` for params means
        // several of these would fail to parse anyway, and a test satisfied by
        // any failure would pass with the role check deleted. The refusal has
        // to be the authorization one, which happens before dispatch.
        assert_eq!(
            e.code,
            codes::FORBIDDEN,
            "`{m}` was refused, but not for being issued by a guest: {}",
            e.message
        );
    }
    Ok(())
}
