//! The HTTP provider against a real server, in both dialects.
//!
//! The dialect translations are unit-tested against hand-written JSON, which
//! proves the shape and nothing about whether a real request/response cycle
//! works. A first run that fails on turn one is how a user decides the project
//! does not work.
//!
//! These tests use a real HTTP server on loopback that speaks each vendor's
//! dialect, the real `HttpModel` talking to it over TCP, and the real agent
//! loop driving a real hub and a real guest that writes real files. The only
//! thing that is not live is the vendor, because a live one is
//! non-deterministic, costs money, and cannot run in CI.
//!
//! They assert on what the vendor received, not just what came back. The
//! interesting failures are in the conversation the agent builds up: whether
//! tool results are sent back in the shape the vendor expects, whether the
//! assistant's own tool call is echoed, whether the catalogue is offered at
//! all. A provider that gets turn one right and turn two wrong looks fine in a
//! unit test.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use openbot_agent::model::{Model, TurnRequest};
use openbot_agent::providers::http::{Dialect, HttpModel, HttpModelConfig};
use openbot_agent::{Agent, AgentConfig, AllowAll, HubClient};
use openbot_proto::frames::ToolDescription;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// A vendor that answers with a fixed script and remembers what it was sent.
struct Vendor {
    url: String,
    seen: Arc<Mutex<Vec<Value>>>,
    auth: Arc<Mutex<Vec<String>>>,
}

/// One scripted reply: an HTTP status and a body.
#[derive(Clone)]
struct Reply(u16, Value);

impl Vendor {
    async fn serving(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let auth: Arc<Mutex<Vec<String>>> = Arc::default();

        let (s, a) = (Arc::clone(&seen), Arc::clone(&auth));
        tokio::spawn(async move {
            let mut turn = 0usize;
            while let Ok((mut sock, _)) = listener.accept().await {
                let (s, a) = (Arc::clone(&s), Arc::clone(&a));
                let reply = replies.get(turn).cloned().unwrap_or(Reply(
                    500,
                    json!({ "error": "the agent asked for more turns than the script has" }),
                ));
                turn += 1;

                tokio::spawn(async move {
                    // Read headers, then exactly Content-Length bytes: a body
                    // split across packets is normal and a single read would
                    // truncate it into invalid JSON.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = find(&buf, b"\r\n\r\n") {
                            break i;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            (k.trim().eq_ignore_ascii_case("content-length"))
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    for l in head.lines() {
                        let low = l.to_ascii_lowercase();
                        if low.starts_with("authorization:") || low.starts_with("x-api-key:") {
                            a.lock().unwrap().push(l.trim().to_owned());
                        }
                    }
                    let mut body = buf[head_end + 4..].to_vec();
                    while body.len() < len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => body.extend_from_slice(&chunk[..n]),
                        }
                    }
                    if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                        s.lock().unwrap().push(v);
                    }

                    let out = reply.1.to_string();
                    let head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.0,
                        out.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        Vendor {
            url: format!("http://{addr}"),
            seen,
            auth,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
    fn auth_headers(&self) -> Vec<String> {
        self.auth.lock().unwrap().clone()
    }

    fn model(&self, dialect: Dialect) -> HttpModel {
        HttpModel::new(HttpModelConfig {
            dialect,
            // The OpenAI dialect appends `/chat/completions`, the Anthropic one
            // `/v1/messages`; both land on this server.
            base_url: self.url.clone(),
            api_key: "test-key-not-a-real-one".into(),
            model: "test-model".into(),
            max_tokens: 1024,
            timeout: Duration::from_secs(10),
        })
        .unwrap()
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

// ── the scripts ─────────────────────────────────────────────────────────

/// OpenAI: ask for a write, then answer.
fn openai_tool_call(id: &str, name: &str, args: Value) -> Reply {
    Reply(
        200,
        json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": id, "type": "function",
                        "function": { "name": name, "arguments": args.to_string() }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        }),
    )
}

fn openai_text(text: &str) -> Reply {
    Reply(
        200,
        json!({
            "id": "cmpl-2",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": text }
            }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 8 }
        }),
    )
}

fn anthropic_tool_call(id: &str, name: &str, args: Value) -> Reply {
    Reply(
        200,
        json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "stop_reason": "tool_use",
            "content": [{ "type": "tool_use", "id": id, "name": name, "input": args }],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }),
    )
}

fn anthropic_text(text: &str) -> Reply {
    Reply(
        200,
        json!({
            "id": "msg_2", "type": "message", "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": text }],
            "usage": { "input_tokens": 20, "output_tokens": 8 }
        }),
    )
}

// ── the rig: a real hub, a real guest, real files ───────────────────────

struct Rig {
    hub: Arc<HubClient>,
    progress: mpsc::UnboundedReceiver<openbot_proto::frames::ToolCallProgressFrame>,
    tools: Vec<ToolDescription>,
    workspace: tempfile::TempDir,
}

async fn rig() -> anyhow::Result<Rig> {
    let hub_state = Arc::new(openbotd::hub::Hub::with_policy(
        openbotd::policy::Policy::allow_all(),
    ));
    let (listener, addr) = openbotd::server::Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(openbotd::server::Server::new(hub_state)).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let workspace = tempfile::tempdir()?;
    let ws = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(workspace.path(), true)?,
        workspace.path().join(".browser-profile"),
    ));
    tokio::spawn(async move {
        let _ = openbot_guest::run(
            openbot_guest::GuestConfig {
                hub_url: url.clone(),
                server_id: "openbot-workspace".into(),
                description: "vendor test guest".into(),
            },
            ws,
        )
        .await;
    });

    let (hub, progress) =
        HubClient::connect_with(&format!("ws://{addr}/v1/tools"), Arc::new(AllowAll)).await?;
    hub.open_session().await?;

    let mut tools = Vec::new();
    for _ in 0..200 {
        if let Ok(t) = hub.bind_server("openbot-workspace").await {
            tools = t;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::ensure!(!tools.is_empty(), "the guest never registered");

    Ok(Rig {
        hub,
        progress,
        tools,
        workspace,
    })
}

async fn drive(
    rig: Rig,
    model: HttpModel,
    task: &str,
) -> (openbot_agent::agent::AgentOutcome, tempfile::TempDir) {
    let agent = Agent::new(
        Arc::new(model),
        Arc::clone(&rig.hub),
        AgentConfig {
            max_steps: 8,
            ..Default::default()
        },
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = agent.run(task, rig.tools, rig.progress, tx).await;
    (outcome, rig.workspace)
}

// ── tests ───────────────────────────────────────────────────────────────

/// A full multi-turn conversation in the OpenAI dialect: the model asks for a
/// tool, the result goes back, the model answers.
#[tokio::test]
async fn an_openai_vendor_drives_a_real_tool_call_to_completion() -> anyhow::Result<()> {
    let vendor = Vendor::serving(vec![
        openai_tool_call(
            "call_1",
            "fs__write",
            json!({ "path": "vendor.md", "contents": "written by a real turn" }),
        ),
        openai_text("Done — I wrote vendor.md."),
    ])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let (outcome, workspace) = drive(r, model, "write a note").await;

    assert!(outcome.succeeded(), "the run failed: {outcome:?}");
    assert!(outcome.text.contains("wrote vendor.md"), "{}", outcome.text);

    // Everything above is only meaningful if the tool actually ran against
    // the real guest.
    let written = std::fs::read_to_string(workspace.path().join("vendor.md"))?;
    assert_eq!(written, "written by a real turn");

    let reqs = vendor.requests();
    assert_eq!(reqs.len(), 2, "expected two turns, got {}", reqs.len());

    // Turn one must offer the catalogue, or the model has nothing to call.
    let names: Vec<&str> = reqs[0]["tools"]
        .as_array()
        .expect("no tools offered")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    // Dotted ids are illegal in a vendor function name, so they go out escaped.
    assert!(
        names.contains(&"fs__write"),
        "catalogue missing fs.write: {names:?}"
    );

    // Turn two is where a provider usually breaks: the assistant's own tool
    // call has to be echoed back, and the result attached to its id.
    let msgs = reqs[1]["messages"].as_array().expect("no messages");
    let assistant = msgs
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the assistant turn was not replayed");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    let result = msgs
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the tool result was never sent back");
    assert_eq!(result["tool_call_id"], "call_1");
    assert!(
        result["content"]
            .as_str()
            .unwrap_or_default()
            .contains("vendor.md"),
        "the tool result carried nothing useful: {result}"
    );

    // The key went out as a bearer token, once per turn.
    let auth = vendor.auth_headers();
    assert_eq!(auth.len(), 2);
    assert!(auth[0].to_lowercase().starts_with("authorization: bearer"));
    Ok(())
}

/// The same conversation in the Anthropic dialect, where the shapes differ in
/// every detail that matters.
#[tokio::test]
async fn an_anthropic_vendor_drives_a_real_tool_call_to_completion() -> anyhow::Result<()> {
    let vendor = Vendor::serving(vec![
        anthropic_tool_call(
            "toolu_1",
            "fs__write",
            json!({ "path": "claude.md", "contents": "anthropic dialect" }),
        ),
        anthropic_text("Written."),
    ])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::AnthropicMessages);
    let (outcome, workspace) = drive(r, model, "write a note").await;

    assert!(outcome.succeeded(), "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("claude.md"))?,
        "anthropic dialect"
    );

    let reqs = vendor.requests();
    assert_eq!(reqs.len(), 2);
    // Anthropic takes tools as a flat list with `input_schema`, not nested
    // under `function`; getting this wrong is a 400 on the first real call.
    let tool_names: Vec<&str> = reqs[0]["tools"]
        .as_array()
        .expect("no tools offered")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(tool_names.contains(&"fs__write"), "{tool_names:?}");
    assert!(
        reqs[0]["tools"][0]["input_schema"].is_object(),
        "tools must carry input_schema: {}",
        reqs[0]["tools"][0]
    );

    // The result goes back as a user message containing a tool_result block
    // that references the call id.
    let msgs = reqs[1]["messages"].as_array().unwrap();
    let found = msgs.iter().any(|m| {
        m["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "toolu_1")
            })
            .unwrap_or(false)
    });
    assert!(found, "no tool_result referencing toolu_1: {msgs:#?}");

    // Anthropic authenticates with x-api-key, not a bearer token.
    let auth = vendor.auth_headers();
    assert!(
        auth.iter()
            .all(|h| h.to_lowercase().starts_with("x-api-key:")),
        "{auth:?}"
    );
    Ok(())
}

/// A rejected key is the single most likely first-run failure, and it has to
/// arrive as something a person can act on.
#[tokio::test]
async fn a_rejected_key_says_so_plainly() {
    let vendor = Vendor::serving(vec![Reply(
        401,
        json!({ "error": { "message": "Incorrect API key provided" } }),
    )])
    .await;

    let model = vendor.model(Dialect::OpenAiChat);
    let err = model
        .turn(&TurnRequest {
            system: "you are a test".into(),
            messages: vec![openbot_agent::model::Message::user("hello")],
            tools: vec![],
        })
        .await
        .expect_err("a 401 must not look like success");

    let text = err.to_string();
    assert!(text.contains("401"), "the status is missing: {text}");
    assert!(
        text.to_lowercase().contains("api key"),
        "the vendor's explanation was thrown away: {text}"
    );
}

/// A body of an unexpected shape must fail loudly rather than being read as
/// an empty answer: an agent that "finishes" with nothing looks like a model
/// that had nothing to say.
#[tokio::test]
async fn a_malformed_body_is_an_error_not_an_empty_answer() {
    let vendor = Vendor::serving(vec![Reply(200, json!({ "not": "a completion" }))]).await;
    let model = vendor.model(Dialect::OpenAiChat);
    let out = model
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("hello")],
            tools: vec![],
        })
        .await;
    assert!(out.is_err(), "a nonsense body was accepted: {out:?}");
}

/// Arguments arrive as a JSON string in the OpenAI dialect, and vendors do
/// emit broken ones. The turn must still parse, so the agent can report the
/// failure instead of the process ending.
#[tokio::test]
async fn unparseable_tool_arguments_do_not_take_the_run_down() {
    let vendor = Vendor::serving(vec![Reply(
        200,
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_x", "type": "function",
                        "function": { "name": "fs.write", "arguments": "{not json" }
                    }]
                }
            }]
        }),
    )])
    .await;

    let model = vendor.model(Dialect::OpenAiChat);
    let out = model
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("write something")],
            tools: vec![],
        })
        .await;

    match out {
        // Either is acceptable; what must not happen is a panic or a silent
        // success carrying arguments nobody can read.
        Ok(t) => {
            let text = format!("{:?}", t.content);
            assert!(
                !text.contains("{not json"),
                "unparsed arguments were passed on as if valid: {text}"
            );
        }
        Err(e) => assert!(!e.to_string().is_empty()),
    }
}

/// A connector tool must survive the round trip through a vendor.
///
/// Connector tools are namespaced `<connector>__<tool>`, matching MCP. The
/// provider escapes dots as `__` on the way out; undoing that textually on the
/// way back would turn `linear__create_issue` into `linear.create_issue`, a
/// tool the hub has never heard of, making every connector unreachable from a
/// real model while `openbot call` (which never goes through a provider) still
/// worked. Testing the two features separately would not catch this.
#[tokio::test]
async fn a_connector_tool_name_survives_a_vendor_round_trip() {
    for dialect in [Dialect::OpenAiChat, Dialect::AnthropicMessages] {
        let reply = match dialect {
            Dialect::OpenAiChat => {
                openai_tool_call("c1", "linear__create_issue", json!({ "title": "it broke" }))
            }
            Dialect::AnthropicMessages => {
                anthropic_tool_call("c1", "linear__create_issue", json!({ "title": "it broke" }))
            }
        };
        let vendor = Vendor::serving(vec![reply]).await;
        let model = vendor.model(dialect);

        let turn = model
            .turn(&TurnRequest {
                system: String::new(),
                messages: vec![openbot_agent::model::Message::user("file an issue")],
                tools: vec![ToolDescription::new(
                    openbot_proto::ToolId::new("linear__create_issue"),
                    "File an issue",
                    json!({ "type": "object" }),
                )],
            })
            .await
            .expect("turn");

        let called: Vec<&str> = turn
            .content
            .iter()
            .filter_map(|c| match c {
                openbot_agent::model::Content::ToolUse { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            called,
            vec!["linear__create_issue"],
            "{dialect:?} mangled a connector tool name"
        );
    }
}

/// A name that was never offered still comes back as something the hub can
/// refuse intelligibly, rather than panicking or silently changing shape.
#[tokio::test]
async fn a_tool_name_we_never_offered_is_left_recognisable() {
    let vendor = Vendor::serving(vec![openai_tool_call("c1", "made__up", json!({}))]).await;
    let model = vendor.model(Dialect::OpenAiChat);
    let turn = model
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("do a thing")],
            tools: vec![],
        })
        .await
        .expect("turn");

    let names: Vec<String> = turn
        .content
        .iter()
        .filter_map(|c| match c {
            openbot_agent::model::Content::ToolUse { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 1);
    // Either spelling is fine; what matters is that it is a name, not a panic.
    assert!(!names[0].is_empty());
}

/// Two different tools must never share one wire name.
///
/// `fs.read` sanitizes to `fs__read`. So does a connector named `fs` offering
/// `read`. If both are in one catalogue, a model asking to read a local file
/// could be routed to a remote SaaS API instead, silently, because both names
/// are legal and a naive map keeps whichever was inserted last.
#[tokio::test]
async fn a_wire_name_collision_never_silently_reroutes_a_call() {
    let vendor = Vendor::serving(vec![openai_tool_call(
        "c1",
        "fs__read",
        json!({ "path": "x" }),
    )])
    .await;
    let model = vendor.model(Dialect::OpenAiChat);

    let turn = model
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("read x")],
            tools: vec![
                ToolDescription::new(
                    openbot_proto::ToolId::new("fs.read"),
                    "Read a file in the workspace",
                    json!({ "type": "object" }),
                ),
                // A connector the operator called `fs`, offering `read`.
                ToolDescription::new(
                    openbot_proto::ToolId::new("fs__read"),
                    "Read from a remote service",
                    json!({ "type": "object" }),
                ),
            ],
        })
        .await
        .expect("turn");

    let called: Vec<&str> = turn
        .content
        .iter()
        .filter_map(|c| match c {
            openbot_agent::model::Content::ToolUse { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    // Whatever it resolves to, it must not be the local file tool: the model
    // asked for the name the connector owns. Guessing here is how a read of a
    // workspace file becomes a call to somebody's API.
    assert_eq!(
        called,
        vec!["fs__read"],
        "an ambiguous wire name was resolved by guessing"
    );

    // The same catalogue in the other order. If resolution depends on which
    // tool happened to be listed first, this is where a request to a remote
    // service silently becomes a read of a local file.
    let vendor = Vendor::serving(vec![openai_tool_call(
        "c2",
        "fs__read",
        json!({ "path": "x" }),
    )])
    .await;
    let model = vendor.model(Dialect::OpenAiChat);
    let turn = model
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("read x")],
            tools: vec![
                ToolDescription::new(
                    openbot_proto::ToolId::new("fs__read"),
                    "Read from a remote service",
                    json!({ "type": "object" }),
                ),
                ToolDescription::new(
                    openbot_proto::ToolId::new("fs.read"),
                    "Read a file in the workspace",
                    json!({ "type": "object" }),
                ),
            ],
        })
        .await
        .expect("turn");
    let called: Vec<&str> = turn
        .content
        .iter()
        .filter_map(|c| match c {
            openbot_agent::model::Content::ToolUse { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        called,
        vec!["fs__read"],
        "listing order decided which tool a call went to"
    );
}

/// What a run cost has to survive the whole loop, including a run that stopped
/// early, which is exactly the run someone asks about.
#[tokio::test]
async fn a_run_reports_the_tokens_it_spent() -> anyhow::Result<()> {
    let vendor = Vendor::serving(vec![
        openai_tool_call(
            "call_1",
            "fs__write",
            json!({ "path": "cost.md", "contents": "x" }),
        ),
        openai_text("Done."),
    ])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let (outcome, _workspace) = drive(r, model, "write a note").await;

    // The scripts report 10/5 then 20/8; both must be summed into the outcome.
    assert_eq!(
        outcome.usage.input_tokens, 30,
        "input tokens were not summed"
    );
    assert_eq!(
        outcome.usage.output_tokens, 13,
        "output tokens were not summed"
    );
    Ok(())
}

/// A vendor that reports no usage must not turn into a zero that reads as
/// "this run was free"; it reads as "unknown", and the renderer omits it.
#[tokio::test]
async fn a_vendor_that_reports_nothing_leaves_the_count_at_zero() -> anyhow::Result<()> {
    let vendor = Vendor::serving(vec![Reply(
        200,
        json!({ "choices": [{ "finish_reason": "stop",
                              "message": { "role": "assistant", "content": "hi" } }] }),
    )])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let (outcome, _w) = drive(r, model, "say hi").await;
    assert_eq!(outcome.usage.input_tokens, 0);
    assert_eq!(outcome.usage.output_tokens, 0);
    Ok(())
}

/// A budget stops the run between turns.
///
/// `max_steps` bounds turns, not spend, and those diverge: every turn resends
/// the whole conversation, so late turns cost far more than early ones. A run
/// can sit well inside its step budget and still cost several times what its
/// operator expected.
#[tokio::test]
async fn a_token_budget_stops_a_run_that_would_keep_spending() -> anyhow::Result<()> {
    // Each turn reports 10 in / 5 out; the budget is 20, so the run should
    // stop before the third rather than after the eighth.
    let vendor = Vendor::serving(vec![
        openai_tool_call("c1", "fs__list", json!({ "path": "." })),
        openai_tool_call("c2", "fs__list", json!({ "path": "." })),
        openai_tool_call("c3", "fs__list", json!({ "path": "." })),
        openai_tool_call("c4", "fs__list", json!({ "path": "." })),
        openai_text("done"),
    ])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let agent = Agent::new(
        Arc::new(model),
        Arc::clone(&r.hub),
        AgentConfig {
            max_steps: 8,
            token_budget: Some(20),
            ..Default::default()
        },
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = agent.run("list things", r.tools, r.progress, tx).await;

    match outcome.reason {
        openbot_agent::FinishReason::TokenBudget { spent, budget } => {
            assert_eq!(budget, 20);
            assert!(
                spent >= 20,
                "stopped before the budget was reached: {spent}"
            );
            // Checked between turns, so the overshoot is one turn at most, as
            // documented on `AgentConfig::token_budget`.
            assert!(spent < 40, "overshot by more than a turn: {spent}");
        }
        other => panic!("expected the budget to stop the run, got {other:?}"),
    }
    assert!(
        outcome.steps < 8,
        "the step budget stopped it, not the tokens"
    );
    Ok(())
}

/// No budget means no cap; the default must not quietly limit anything.
#[tokio::test]
async fn without_a_budget_a_run_is_not_capped() -> anyhow::Result<()> {
    let vendor = Vendor::serving(vec![
        openai_tool_call("c1", "fs__list", json!({ "path": "." })),
        openai_text("done"),
    ])
    .await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let (outcome, _w) = drive(r, model, "list things").await;
    assert!(outcome.succeeded(), "{outcome:?}");
    assert!(outcome.usage.input_tokens > 0);
    Ok(())
}

/// A compacted conversation must still be a conversation the vendor accepts.
///
/// Compaction shrinks old tool results so a long run does not outgrow the
/// context. The danger is trading one failure for a worse one: every
/// `tool_use` must keep its matching `tool_result`, in order, or the vendor
/// rejects the entire request with a 400. This drives enough turns to trigger
/// compaction and then checks what the vendor was actually sent.
#[tokio::test]
async fn compaction_never_breaks_the_tool_call_pairing() -> anyhow::Result<()> {
    // Write something substantial, then read it back repeatedly: an empty
    // workspace lists in about thirty characters, which never fills anything.
    let big = "x".repeat(4_000);
    let mut script = vec![openai_tool_call(
        "c0",
        "fs__write",
        json!({ "path": "notes.md", "contents": big }),
    )];
    script
        .extend((1..10).map(|i| {
            openai_tool_call(&format!("c{i}"), "fs__read", json!({ "path": "notes.md" }))
        }));
    script.push(openai_text("done"));
    let vendor = Vendor::serving(script).await;

    let r = rig().await?;
    let model = vendor.model(Dialect::OpenAiChat);
    let agent = Agent::new(
        Arc::new(model),
        Arc::clone(&r.hub),
        AgentConfig {
            max_steps: 12,
            // Intentionally small: a real budget would need a run far longer
            // than a test should be, and the property under test is the shape
            // of a compacted conversation, not the size that triggers it.
            context_budget: 2_000,
            ..Default::default()
        },
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = agent
        .run("list it repeatedly", r.tools, r.progress, tx)
        .await;
    assert!(outcome.succeeded(), "{outcome:?}");

    // Look at the last request: by then compaction has certainly run.
    let reqs = vendor.requests();
    let last = reqs.last().expect("no requests");
    let msgs = last["messages"].as_array().expect("no messages");

    let mut expecting: Vec<String> = Vec::new();
    for m in msgs {
        if let Some(calls) = m["tool_calls"].as_array() {
            for c in calls {
                expecting.push(c["id"].as_str().unwrap_or_default().to_owned());
            }
        }
        if m["role"] == "tool" {
            let id = m["tool_call_id"].as_str().unwrap_or_default().to_owned();
            let at = expecting.iter().position(|e| *e == id);
            assert!(
                at.is_some(),
                "a tool result with no matching call: {id}; a vendor 400 is a \
                 worse failure than the context overflow this avoids"
            );
            expecting.remove(at.unwrap());
        }
    }
    assert!(
        expecting.is_empty(),
        "these calls were sent with no result: {expecting:?}"
    );

    // Compaction must actually have happened, or the test is vacuous.
    let text = last.to_string();
    assert!(
        text.contains("run the tool again"),
        "the conversation never got large enough to compact; the test is not \
         exercising what it claims"
    );
    Ok(())
}

#[tokio::test]
async fn an_error_delivered_with_a_200_is_still_an_error() {
    // Gateways in front of these APIs answer 200 with `{"error": ...}` rather
    // than an HTTP status; OpenAI-compatible proxies do it routinely, and
    // `--base-url` exists so people can point openbot at one.
    //
    // The status check cannot see it. Without a body-level check the dialect
    // parser looks for the success keys, does not find them, and reports
    // "response has no choices", which reads like a bug in openbot rather than
    // the expired key it actually is, and discards the provider's own message.
    for (dialect, body) in [
        (
            Dialect::OpenAiChat,
            json!({"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}}),
        ),
        (
            Dialect::AnthropicMessages,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ] {
        let vendor = Vendor::serving(vec![Reply(200, body)]).await;
        let err = vendor
            .model(dialect)
            .turn(&TurnRequest {
                system: "you are a test".into(),
                messages: vec![openbot_agent::model::Message::user("hello")],
                tools: vec![],
            })
            .await
            .expect_err("an error body must not look like a successful turn");

        let text = err.to_string();
        assert!(
            text.contains("Incorrect API key provided") || text.contains("Overloaded"),
            "the provider said what was wrong and it was dropped: {text}"
        );
        assert!(
            !text.contains("no choices") && !text.contains("no content array"),
            "reported as a malformed response instead of the provider's error: {text}"
        );
    }
}

#[tokio::test]
async fn a_real_answer_carrying_an_error_field_is_still_an_answer() {
    // The error check only fires when the success keys are absent: a body
    // with `choices` is a response, whatever else is in it. A looser rule
    // ("has an error key") would fail perfectly good turns, and silently: the
    // run would stop with the provider apparently complaining.
    let vendor = Vendor::serving(vec![Reply(
        200,
        json!({
            "error": {"message": "one upstream replica timed out"},
            "choices": [{
                "message": {"content": "the error you asked about is on line 3"},
                "finish_reason": "stop"
            }]
        }),
    )])
    .await;

    let turn = vendor
        .model(Dialect::OpenAiChat)
        .turn(&TurnRequest {
            system: String::new(),
            messages: vec![openbot_agent::model::Message::user("what is the error?")],
            tools: vec![],
        })
        .await
        .expect("a response with choices is a response");

    let said = format!("{:?}", turn.content);
    assert!(said.contains("on line 3"), "{said}");
}
