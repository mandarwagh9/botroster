//! An upstream client's approvals, as the permission hook it answers.
//!
//! The gate this file is about is the product's central security control: policy
//! is evaluated in the hub, not in the caller, because a check the caller
//! evaluates is a check the caller can delete (SPEC §6.0). Every test here is
//! also a fail-closed test. An approval that is unreadable, unknown, silent or
//! forged must deny, because the alternative is running a call nobody agreed to.
//!
//! **Why a second dialect exists at all.** A published harness answers a
//! permission request with a `hook` frame and a `hook_reply` notification. This
//! hub asked with `approval.request` and expected the JSON-RPC result of the
//! request itself. An upstream client cannot answer that, so every approval
//! stalled for the full timeout and then denied: an interop harness asking to
//! call a tool got `no approval was given in time` from a hub that was working
//! exactly as designed.
//!
//! So the question is put in the dialect the asker understands, chosen per
//! connection at `register`. BOTROSTER's own clients are untouched: they keep
//! `approval.request` and the legacy path stays what it was, which is what
//! `a_botroster_dialect_client_still_receives_approval_request` and the
//! unchanged `approval_owner.rs` suite are there to prove.
//!
//! The fixture below is hand written from the published shapes rather than
//! importing them, because under D1 = A no upstream code enters this repository.
//! It models two behaviours a naive fixture gets wrong, and both were found by
//! running a real peer rather than by reading a type:
//!
//! * **Routing.** A `hook` reaches the harness only through the session inbox,
//!   which `demux.route` feeds only for a frame carrying an envelope
//!   `session_id` (`demux.rs:387`). A hub that sends the hook without one gets
//!   no reply and no error, so a lenient fixture would pass against a hub that
//!   hangs.
//! * **The reply is a notification, not a response.** The SDK's handler "runs
//!   inline on the shared inbox loop and MUST NOT block" (`harness.rs:1682`), and
//!   it answers with a `hook_reply` notification carrying `hook_id`, never with a
//!   JSON-RPC response to the request's id (`harness.rs:2079`). So this fixture
//!   leaves the request id unanswered forever, exactly as the SDK does, and a
//!   hub that waited on a response would time out.
//!
//! One owner owns its whole socket and answers inline, because only one reader
//! may read a socket. A separate reader task plus a shared outcome map was the
//! first shape and it was wrong: tests in one file share a process, so request
//! ids collided across tests and a test could read another's answer.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Notification, Outcome, Request, Response, RpcId, ServerId,
    SessionId, ToolCallId, ToolCallParams, ToolId,
};
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::secrets::SecretStore;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The wire string a published client announces, and the only way to reach the
/// upstream dialect. Reachable only while the interop switch is on.
const UPSTREAM: &str = "1.0.0";

/// What the person did. Scripted rather than interactive: these tests assert on
/// the decision that reaches the hub, and an interactive prompt would make the
/// file something you had to sit in front of.
#[derive(Clone, Debug)]
enum Answer {
    Approve,
    Reject(String),
    AlwaysApprove,
    /// Silent means no frame at all, so the hub has to deny on its own timeout.
    Silent,
    /// A `result` no honest renderer would send. Upstream reads anything it does
    /// not recognise as a rejection (`reply_to_outcome`'s `_ => RejectOnce`), and
    /// so must this.
    Raw(serde_json::Value),
}

/// One question the hub put to this connection.
#[derive(Clone, Debug)]
struct Asked {
    method: String,
    request_id: String,
    envelope_session: Option<String>,
    params: serde_json::Value,
}

/// A harness that owns a session and answers approvals, in whichever dialect it
/// announced. Records every question, which is what most assertions here read.
struct Owner {
    sock: Sock,
    session: SessionId,
    asked: Arc<std::sync::Mutex<Vec<Asked>>>,
    answers: VecDeque<Answer>,
}

impl Owner {
    async fn start(url: &str, upstream: bool, answers: Vec<Answer>) -> anyhow::Result<Self> {
        let (mut sock, _) = connect_async(url).await?;
        let mut hello = Hello::harness();
        if upstream {
            hello.protocol_version = UPSTREAM.to_owned();
        }
        sock.send(Message::Text(serde_json::to_string(&hello)?))
            .await?;
        match sock.next().await {
            Some(Ok(Message::Text(t))) => {
                let _: HelloAck = serde_json::from_str(&t)?;
            }
            other => anyhow::bail!("bad handshake: {other:?}"),
        }
        let mut me = Self {
            sock,
            session: SessionId::new("unset"),
            asked: Arc::new(std::sync::Mutex::new(Vec::new())),
            answers: answers.into(),
        };
        let outcome = me.exchange(1, Method::SessionOpen, json!({}), None).await?;
        let Outcome::Result(v) = outcome else {
            anyhow::bail!("could not open a session: {outcome:?}")
        };
        me.session = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
        Ok(me)
    }

    /// One request, one answer, answering whatever questions arrive on the way.
    ///
    /// The hub asks for approval *before* it answers the call, so the question and
    /// the reply share this read loop. Anything the hub sends that is not the
    /// awaited response is a question, and is answered in this connection's
    /// dialect.
    async fn exchange(
        &mut self,
        id: i64,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<Outcome> {
        let mut req = Request::new(RpcId::Num(id), method, Some(params));
        if let Some(s) = session {
            req = req.in_session(s.clone());
        }
        self.sock
            .send(Message::Text(Frame::Request(req).encode()))
            .await?;
        loop {
            // Deliberately far longer than any approval timeout used below. The hub's own
            // timeout is the thing under test, so a fixture that trips first
            // replaces a legible "it waited N seconds" with an opaque "the hub
            // never answered", which is how a real timeout regression would hide.
            let msg = tokio::time::timeout(Duration::from_secs(60), self.sock.next())
                .await
                .map_err(|_| anyhow::anyhow!("the hub never answered request {id}"))?;
            let Some(Ok(Message::Text(t))) = msg else {
                anyhow::bail!("socket closed awaiting request {id}");
            };
            let frame = Frame::decode(&t)?;
            match frame {
                Frame::Response(r) if r.id == RpcId::Num(id) => return Ok(r.outcome),
                Frame::Request(r) => self.answer(&r).await?,
                _ => {}
            }
        }
    }

    /// Answer one question from the hub, in the dialect this connection announced.
    async fn answer(&mut self, r: &Request) -> anyhow::Result<()> {
        let params = r.params.clone().unwrap_or(serde_json::Value::Null);
        if let Ok(mut v) = self.asked.lock() {
            v.push(Asked {
                method: r.method.clone(),
                request_id: rpc_key(&r.id),
                envelope_session: r.session_id.as_ref().map(|s| s.as_str().to_owned()),
                params: params.clone(),
            });
        }
        let answer = self.answers.pop_front();
        let out: Option<Message> = match r.parsed_method() {
            // Upstream. The answer is a notification keyed by `hook_id`; the
            // request id is left unanswered, which is what the SDK does.
            Some(Method::Hook) => {
                let hook_id = params
                    .get("hook_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                let result: Option<serde_json::Value> = match answer {
                    Some(Answer::Approve) => Some(json!({ "outcome": "approve" })),
                    Some(Answer::Reject(m)) => {
                        Some(json!({ "outcome": "reject", "followup_message": m }))
                    }
                    Some(Answer::AlwaysApprove) => Some(json!({
                        "outcome": "always_approve",
                        "scope": { "kind": "bash_command", "value": "git status" },
                    })),
                    Some(Answer::Raw(v)) => Some(v),
                    Some(Answer::Silent) | None => None,
                };
                hook_id.zip(result).map(|(hook_id, result)| {
                    Message::Text(
                        Frame::Notification(Notification::new(
                            Method::HookReply,
                            json!({
                                "session_id": self.session.as_str(),
                                "hook_id": hook_id,
                                "result": result,
                            }),
                        ))
                        .encode(),
                    )
                })
            }
            // Legacy. The decision is the JSON-RPC result of the request itself,
            // which is what it always was.
            Some(Method::ApprovalRequest) => {
                let decision = match answer {
                    Some(Answer::Approve) => json!({ "decision": "allow_once" }),
                    Some(Answer::Reject(m)) => json!({ "decision": "deny", "note": m }),
                    Some(Answer::AlwaysApprove) => json!({ "decision": "allow_always" }),
                    Some(Answer::Raw(v)) => v,
                    Some(Answer::Silent) | None => return Ok(()),
                };
                Some(Message::Text(
                    Frame::Response(Response::ok(r.id.clone(), decision)).encode(),
                ))
            }
            _ => None,
        };
        if let Some(m) = out {
            self.sock.send(m).await?;
        }
        Ok(())
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().expect("asked").clone()
    }

    /// The questions asked as a `hook`, which is what nearly every test here is
    /// about. Failing with the methods actually seen beats a filter that quietly
    /// matches nothing.
    fn hooks(&self) -> Vec<Asked> {
        let all = self.asked();
        let hooks: Vec<Asked> = all.iter().filter(|a| a.method == "hook").cloned().collect();
        assert!(
            !hooks.is_empty(),
            "expected the hub to ask with a `hook`, and it asked with {:?}",
            all.iter().map(|a| &a.method).collect::<Vec<_>>()
        );
        hooks
    }

    async fn bind(&mut self, server_id: &ServerId) -> anyhow::Result<Outcome> {
        // Cloned rather than borrowed: `exchange` takes `&mut self`, so handing
        // it `&self.session` would borrow the owner twice.
        let session = self.session.clone();
        self.exchange(
            2,
            Method::SessionBindServer,
            json!({ "server_id": server_id.as_str() }),
            Some(&session),
        )
        .await
    }

    async fn call(&mut self, tool: &ToolId, args: serde_json::Value) -> anyhow::Result<Outcome> {
        let params = ToolCallParams {
            tool_id: tool.clone(),
            call_id: ToolCallId::new("call-1"),
            args,
        };
        let session = self.session.clone();
        self.exchange(
            3,
            Method::ToolCall,
            serde_json::to_value(&params).unwrap(),
            Some(&session),
        )
        .await
    }
}

fn rpc_key(id: &RpcId) -> String {
    match id {
        RpcId::Str(s) => s.clone(),
        RpcId::Num(n) => n.to_string(),
    }
}

/// One tool server, so a call that is approved has somewhere to go. Approving a
/// call that then fails to dispatch would let a hub that dispatched nothing pass
/// a test about what it dispatched.
struct OneTool {
    server_id: ServerId,
}

impl OneTool {
    async fn start(url: &str) -> Self {
        let id = ServerId::new("tool-1");
        let hello = Hello::tool_server(id.as_str());
        let url = url.to_owned();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = connect_async(&url).await else {
                return;
            };
            if sock
                .send(Message::Text(serde_json::to_string(&hello).unwrap()))
                .await
                .is_err()
            {
                return;
            }
            if !matches!(sock.next().await, Some(Ok(Message::Text(_)))) {
                return;
            }
            while let Some(Ok(Message::Text(t))) = sock.next().await {
                let Ok(Frame::Request(r)) = Frame::decode(&t) else {
                    continue;
                };
                let result: Option<serde_json::Value> = match r.parsed_method() {
                    // Params carry the session; the envelope must not also carry
                    // it, or `demux.route` sends this to a session inbox instead of
                    // the notification channel that answers a bind.
                    Some(Method::SessionBind) => {
                        let p = r.params.clone().unwrap_or(json!(null));
                        if p.get("session_id").and_then(|v| v.as_str()).is_none() {
                            continue;
                        }
                        Some(
                            serde_json::to_value(SessionBindResult {
                                tools: vec![ToolDescription::new(
                                    "peer:echo",
                                    "the one tool",
                                    json!({ "type": "object" }),
                                )],
                                binary_version: Some("one-tool-0.1.0".into()),
                            })
                            .unwrap(),
                        )
                    }
                    Some(Method::ToolCallRequest) => {
                        let p: ToolCallRequestParams =
                            serde_json::from_value(r.params.clone().unwrap_or(json!(null)))
                                .unwrap_or_else(|_| ToolCallRequestParams {
                                    tool_id: ToolId::new("peer:echo"),
                                    call_id: ToolCallId::new("unknown"),
                                    args: json!({}),
                                });
                        Some(
                            serde_json::to_value(ToolCallResult {
                                call_id: p.call_id,
                                output: json!({ "ran": p.tool_id.as_str() }),
                            })
                            .unwrap(),
                        )
                    }
                    _ => None,
                };
                let Some(result) = result else { continue };
                let out = Response::ok(r.id.clone(), result);
                if sock
                    .send(Message::Text(Frame::Response(out).encode()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Self { server_id: id }
    }
}

/// A hub whose policy asks about anything no rule covers, which is what makes an
/// uncovered tool the interesting case: a client meets this path on the first
/// tool it has no rule for, having configured nothing.
async fn hub_asking(approval_timeout: Duration) -> anyhow::Result<String> {
    Ok(spawn_hub(
        Hub::with_policy(Policy::default())
            .accepting_upstream_protocol(true)
            .with_call_timeout(Duration::from_secs(10))
            .with_approval_timeout(approval_timeout),
    )
    .await)
}

async fn hub_with(policy: Policy, approval_timeout: Duration) -> anyhow::Result<String> {
    Ok(spawn_hub(
        Hub::with_policy(policy)
            .accepting_upstream_protocol(true)
            .with_call_timeout(Duration::from_secs(10))
            .with_approval_timeout(approval_timeout),
    )
    .await)
}

async fn spawn_hub(hub: Hub) -> String {
    let (listener, addr) = Server::bind("127.0.0.1:0").await.expect("bind");
    tokio::spawn(Arc::new(Server::new(Arc::new(hub))).serve(listener));
    format!("ws://{addr}/v1/tools")
}

fn err_text(o: &Outcome) -> Option<String> {
    match o {
        Outcome::Error(e) => Some(e.message.clone()),
        Outcome::Result(_) => None,
    }
}

fn ran(o: &Outcome) -> bool {
    matches!(o, Outcome::Result(_))
}

/// Owner with a tool server bound, which is the state every test here starts
/// from. The bind is retried because the server connects on its own schedule and
/// a test that binds too early fails with "no tool server registered", which
/// reads like a hub fault rather than like a race.
async fn owner_with_tool(url: &str, upstream: bool, answers: Vec<Answer>) -> anyhow::Result<Owner> {
    let tool = OneTool::start(url).await;
    let mut owner = Owner::start(url, upstream, answers).await?;
    for _ in 0..40 {
        if ran(&owner.bind(&tool.server_id).await?) {
            return Ok(owner);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("the tool server never registered")
}

/// 1. The shape of the question an upstream client is asked.
///
/// The assertions that matter are the ones a renderer depends on:
/// `tool_approval_policy` (so it knows whether to offer "always"),
/// `bash_command` / `edit_file_paths` (so it can *show* what is being approved),
/// and the envelope session (without which the frame never reaches the handler).
#[tokio::test]
async fn an_upstream_client_is_asked_with_a_permission_hook() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_secs(5)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Approve]).await?;

    let _ = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    let hooks = owner.hooks();
    let first = &hooks[0];
    let payload = &first.params["event"]["payload"];

    assert_eq!(first.params["event"]["type"], "Custom");
    assert_eq!(first.params["event"]["kind"], "permission_request");
    assert_eq!(payload["tool_name"], "peer:echo");
    assert!(
        payload["tool_call_id"].is_string(),
        "a person cannot match a decision to a call without its id: {payload}"
    );
    assert_eq!(
        payload["scope"], "write",
        "a mutating call must read as a write: {payload}"
    );
    assert_eq!(
        payload["tool_approval_policy"], "always_prompt",
        "this hub records nothing from an `always` answer, so a renderer that offers \
         one is offering something the hub will ignore. `always_prompt` is how it is \
         told: {payload}"
    );
    // The session rides the envelope or `demux.route` never delivers the frame to
    // the handler. Asserted because a missing one is completely silent.
    assert_eq!(
        first.envelope_session.as_deref(),
        Some(owner.session.as_str()),
        "the hook went out without the session on the envelope, so upstream's router \
         sends it to a session inbox instead of the hook handler"
    );
    assert!(
        !first.params["hook_id"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "the hook has no hook_id, so no reply can be routed back to it"
    );
    // And it is the request id, so one number ties the question to its answer.
    assert_eq!(first.params["hook_id"], first.request_id);
    // BOTROSTER's own fields ride along in the payload, which is safe because
    // upstream's payload is an open value.
    assert!(payload["approval_id"].is_string(), "{payload}");
    assert!(payload["args"].is_object(), "{payload}");
    assert!(payload["timeout_secs"].is_number(), "{payload}");
    Ok(())
}

/// 2. `approve` runs the call once, and asks again next time.
///
/// "Asks again" is the half that matters. An `approve` that quietly became a
/// grant would mean one click removes the gate for the rest of the session.
#[tokio::test]
async fn an_approve_runs_the_call_once_and_asks_again_next_time() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_secs(5)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Approve, Answer::Approve]).await?;

    let first = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    assert!(ran(&first), "an approved call did not run: {first:?}");
    let second = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    assert!(
        ran(&second),
        "the second approved call did not run: {second:?}"
    );
    assert_eq!(
        owner.hooks().len(),
        2,
        "the hub asked once for two calls, so the first `approve` became a grant. \
         That is what the scoped-answer case is for, and nothing here grants."
    );
    Ok(())
}

/// 3. `reject` denies, and the note the person wrote reaches the decision.
#[tokio::test]
async fn a_reject_denies_and_its_followup_message_is_kept() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_secs(5)).await?;
    let note = "not while I am mid-deploy".to_owned();
    let mut owner = owner_with_tool(&url, true, vec![Answer::Reject(note.clone())]).await?;

    let out = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    let message =
        err_text(&out).unwrap_or_else(|| panic!("a rejected call must be refused: {out:?}"));
    assert!(
        message.contains(&note),
        "the person's reason was dropped, so the transcript cannot say why: {message}"
    );
    Ok(())
}

/// 4. Silence denies, on the hub's own timeout.
#[tokio::test]
async fn silence_denies_after_the_timeout() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_millis(700)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Silent]).await?;

    let out = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    let message =
        err_text(&out).unwrap_or_else(|| panic!("an unanswered approval must deny: {out:?}"));
    assert!(
        message.contains("approval"),
        "the refusal should name the approval as its cause: {message}"
    );
    Ok(())
}

/// 5. Every answer the hub cannot read denies, and each says why.
///
/// Upstream's own fallthrough for an outcome it does not recognise is a rejection
/// (`reply_to_outcome`'s `_ => RejectOnce`). This is the same rule, and it is the
/// rule that matters: a hub that guessed "probably meant approve" would turn a
/// broken renderer into a silent bypass.
#[tokio::test]
async fn an_answer_the_hub_cannot_read_denies_and_says_why() -> anyhow::Result<()> {
    for (label, answer) in [
        (
            "an unreadable result",
            Answer::Raw(json!({ "outcome": { "nested": true } })),
        ),
        (
            "an unknown outcome",
            Answer::Raw(json!({ "outcome": "probably_fine" })),
        ),
        (
            "a missing outcome",
            Answer::Raw(json!({ "decision": "allow_once" })),
        ),
    ] {
        let url = hub_asking(Duration::from_secs(5)).await?;
        let mut owner = owner_with_tool(&url, true, vec![answer]).await?;
        let out = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
        let message =
            err_text(&out).unwrap_or_else(|| panic!("{label} was treated as approval: {out:?}"));
        assert!(
            !message.is_empty(),
            "{label} denied, but the refusal named no cause"
        );
    }
    Ok(())
}

/// 6. A scoped `always_approve` is one allow, and remembers nothing.
///
/// Upstream would honour `scope: {kind: "bash_command", value: "git status"}` as a
/// standing grant for that command. This hub has nowhere to store a scoped grant,
/// and turning "always allow `git status`" into a session-wide grant for the tool
/// would be exactly the widening that `tool_approval_policy: always_prompt` tells
/// the renderer not to offer in the first place.
#[tokio::test]
async fn a_scoped_always_approve_allows_once_and_remembers_nothing() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_millis(900)).await?;
    // A second answer is never scripted: if the first had become a grant, the
    // second call would run without anyone being asked.
    let mut owner = owner_with_tool(&url, true, vec![Answer::AlwaysApprove]).await?;

    let first = owner
        .call(
            &ToolId::new("shell.exec"),
            json!({ "command": "git status" }),
        )
        .await?;
    assert!(
        ran(&first),
        "the scoped always-approve did not run: {first:?}"
    );

    let second = owner
        .call(
            &ToolId::new("shell.exec"),
            json!({ "command": "rm -rf build" }),
        )
        .await?;
    assert!(
        err_text(&second).is_some(),
        "`always_approve` with a bash_command scope was remembered as a grant for the \
         whole session, so a different command ran without asking: {second:?}"
    );
    Ok(())
}

/// 7. What the person is shown is what is about to happen.
///
/// A permission card that says "Run peer:echo" for a call about to delete a build
/// directory is not an approval, it is a click. Upstream's renderer reads
/// `bash_command`, `edit_file_paths` and `description`, so those are the three
/// fields this has to fill.
#[tokio::test]
async fn what_a_person_is_shown_is_what_is_about_to_happen() -> anyhow::Result<()> {
    // A shell command.
    let url = hub_asking(Duration::from_millis(700)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Silent]).await?;
    let _ = owner
        .call(
            &ToolId::new("shell.exec"),
            json!({ "command": "rm -rf build" }),
        )
        .await?;
    let payload = owner.hooks()[0].params["event"]["payload"].clone();
    assert_eq!(
        payload["bash_command"], "rm -rf build",
        "the exact command is not in a field a renderer shows: {payload}"
    );

    // A file write. The guest's argument is `path` (`botroster-guest/src/tools.rs`).
    let url = hub_asking(Duration::from_millis(700)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Silent]).await?;
    let _ = owner
        .call(
            &ToolId::new("fs.write"),
            json!({ "path": "notes.md", "contents": "x" }),
        )
        .await?;
    let payload = owner.hooks()[0].params["event"]["payload"].clone();
    assert_eq!(
        payload["edit_file_paths"][0], "notes.md",
        "the path being written is not in a field a renderer shows: {payload}"
    );

    // Anything else: the arguments themselves, in the description.
    let url = hub_asking(Duration::from_millis(700)).await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Silent]).await?;
    let _ = owner
        .call(
            &ToolId::new("peer:echo"),
            json!({ "text": "hello", "n": 3 }),
        )
        .await?;
    let payload = owner.hooks()[0].params["event"]["payload"].clone();
    let description = payload["description"].as_str().unwrap_or_default();
    assert!(
        description.contains("hello"),
        "an unrecognised tool must show its full arguments, and the description does \
         not: {description}"
    );
    Ok(())
}

/// 8. A stranger's forged reply does not decide anything.
///
/// `required_role` is consulted only in `on_request`, so listing `HookReply` there
/// enforces nothing on a notification. The sender check has to live in the
/// notification path itself, or any connected client can answer any pending
/// approval by guessing a `hook_id`.
///
/// The stranger forges a **reject** while the real owner approves. If the forged
/// reply were accepted the call would be refused, so the owner's approve landing
/// is the proof it was ignored.
#[tokio::test]
async fn a_reply_from_a_connection_that_was_not_asked_is_ignored() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_millis(900)).await?;
    let tool = OneTool::start(&url).await;
    let mut owner = Owner::start(&url, true, vec![Answer::Approve]).await?;
    for _ in 0..40 {
        if ran(&owner.bind(&tool.server_id).await?) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut stranger = Owner::start(&url, true, vec![Answer::Approve]).await?;

    // The owner answers with a `hook_reply` from its own reader task; to forge,
    // the stranger sends its own notification for the same hook id. The id is
    // taken from what the hub actually asked, so this is a real attempt and not
    // a guess.
    let session = owner.session.clone();
    let out = tokio::time::timeout(Duration::from_secs(10), async {
        let params = ToolCallParams {
            tool_id: ToolId::new("peer:echo"),
            call_id: ToolCallId::new("call-1"),
            args: json!({}),
        };
        let req = Request::new(
            RpcId::Num(3),
            Method::ToolCall,
            Some(serde_json::to_value(&params).unwrap()),
        )
        .in_session(session.clone());
        owner
            .sock
            .send(Message::Text(Frame::Request(req).encode()))
            .await
            .unwrap();
        loop {
            let Some(Ok(Message::Text(t))) = owner.sock.next().await else {
                anyhow::bail!("closed");
            };
            let frame = Frame::decode(&t)?;
            if let Frame::Request(r) = &frame {
                if r.parsed_method() == Some(Method::Hook) {
                    let hook_id = r
                        .params
                        .as_ref()
                        .map(|p| p["hook_id"].clone())
                        .unwrap_or_default();
                    let hook_id = hook_id.as_str().unwrap_or_default().to_owned();
                    // Forge from the stranger before the owner answers.
                    let forged = Notification::new(
                        Method::HookReply,
                        json!({
                            "session_id": session.as_str(),
                            "hook_id": hook_id,
                            "result": { "outcome": "reject", "followup_message": "forged" },
                        }),
                    );
                    stranger
                        .sock
                        .send(Message::Text(Frame::Notification(forged).encode()))
                        .await
                        .unwrap();
                }
                owner.answer(r).await.unwrap();
            }
            if let Frame::Response(r) = frame {
                if r.id == RpcId::Num(3) {
                    return Ok(r.outcome);
                }
            }
        }
    })
    .await?;

    let out = out?;
    let message = err_text(&out);
    assert!(
        message.is_none(),
        "a forged reject from a connection that was never asked decided the call: {message:?}"
    );
    Ok(())
}

/// 9. A deny is not overridable by anyone, including the owner.
///
/// `Policy::evaluate` short-circuits on a deny before an approver is consulted at
/// all, so this is a property of the gate rather than of the dialect. It is here
/// because the dialect is new, and "the person approved it" is the kind of thing
/// that could quietly become a way around a ban.
#[tokio::test]
async fn a_deny_is_not_overridable_by_any_reply() -> anyhow::Result<()> {
    let url = hub_with(
        Policy {
            rules: vec![botrosterd::policy::Rule::deny("peer:echo", "not this tool")],
            ..Policy::default()
        },
        Duration::from_millis(700),
    )
    .await?;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Approve]).await?;

    let out = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    assert!(
        err_text(&out).is_some(),
        "a denied tool ran because the owner approved it: {out:?}"
    );
    assert!(
        !owner.asked().iter().any(|a| a.method == "hook"),
        "the hub asked for approval on a call its own policy had already denied, \
         which means a person was asked to approve something that was never going \
         to be allowed"
    );
    Ok(())
}

/// 10. BOTROSTER's own clients are untouched.
///
/// The regression that matters most, because the whole point of the per-connection
/// dialect is that nothing else moves. A client that announced `botroster-1` must
/// still be asked with `approval.request` and must still answer with the JSON-RPC
/// result of the request.
#[tokio::test]
async fn a_botroster_dialect_client_still_receives_approval_request() -> anyhow::Result<()> {
    let url = hub_asking(Duration::from_secs(5)).await?;
    let mut owner = owner_with_tool(&url, false, vec![Answer::Approve]).await?;

    let out = owner.call(&ToolId::new("peer:echo"), json!({})).await?;
    assert!(ran(&out), "the legacy approval path broke: {out:?}");

    let asked = owner.asked();
    assert!(
        asked.iter().any(|a| a.method == "approval.request"),
        "a botroster-1 client was not asked with approval.request, it was asked with {:?}",
        asked.iter().map(|a| &a.method).collect::<Vec<_>>()
    );
    assert!(
        asked.iter().all(|a| a.method != "hook"),
        "a botroster-1 client was sent a hook, which is the dialect it cannot answer"
    );
    Ok(())
}

/// 11. A credential request to an upstream-dialect connection fails at once.
///
/// `secret.request` is a BOTROSTER extension with no upstream equivalent, so an
/// upstream client will never answer it. Leaving that to the approval timeout
/// would cost the full timeout on every call needing a credential, and would look
/// like a hung hub rather than a declined one.
#[tokio::test]
async fn a_credential_request_to_an_upstream_client_fails_at_once() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join(format!("botroster-n2-secrets-{}", std::process::id()));
    std::fs::create_dir_all(&home)?;
    let hub = Hub::with_policy(Policy::default())
        .accepting_upstream_protocol(true)
        .with_call_timeout(Duration::from_secs(10))
        // Long, so a request that waited would be obvious rather than ambiguous.
        .with_approval_timeout(Duration::from_secs(20))
        .with_secrets(Arc::new(SecretStore::open(&home)?));
    let url = spawn_hub(hub).await;
    let mut owner = owner_with_tool(&url, true, vec![Answer::Silent]).await?;

    let started = std::time::Instant::now();
    let out = owner
        .call(
            &ToolId::new("secret.request"),
            json!({ "name": "linear-token", "why": "to talk to linear" }),
        )
        .await?;
    let elapsed = started.elapsed();

    let message = err_text(&out)
        .unwrap_or_else(|| panic!("a credential with nobody to answer must fail: {out:?}"));
    assert!(
        message.contains("credential"),
        "the refusal should say there is no credential: {message}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the request waited {elapsed:?} for an answer that cannot exist. A 20-second \
         approval timeout turned a declined credential into a hung hub."
    );
    Ok(())
}
