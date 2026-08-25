//! The Computer Hub: one WebSocket per peer, JSON-RPC 2.0 on top.
//!
//! Two peer roles connect here. A harness (the agent) calls tools; a
//! tool server (the guest daemon, an MCP bridge) owns and executes them.
//! The hub authorises each socket once at upgrade, then routes between them.
//!
//! Routing, end to end:
//!
//! ```text
//! harness                    hub                       tool server
//!   │ session_open ─────────► mint SessionId
//!   │ session_bind_server ──► session.bind ──────────────►│
//!   │                       ◄──────────── SessionBindResult (tool snapshot)
//!   │ ◄──── SessionBindServerResult
//!   │ tool.call ───────────► tool_call_request ──────────►│
//!   │ ◄──── tool_call_progress ◄──── tool_call_progress ──│  (0..n, relayed)
//!   │ ◄──── result           ◄──────────────── result ────│  (exactly 1)
//! ```
//!
//! The hub rewrites JSON-RPC ids when forwarding so it can correlate a reply
//! back to the originating request, and keeps a `call_id → origin` map so
//! progress notifications reach the right harness.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use botroster_proto::approval::{
    ApprovalDecision, ApprovalId, ApprovalRequestParams, Decision, SecretRequestParams,
    SecretRequestResult, SecretStoredResult,
};
use botroster_proto::frames::*;
use botroster_proto::{
    codes, ConnectionId, ConnectionKind, Frame, Hello, HelloAck, Method, Notification, Outcome,
    Principal, Request, Response, RpcError, RpcId, ServerId, SessionId, ToolCallId, ToolId, UserId,
    WorkspaceGonePhase, WorkspaceGoneReason, WorkspaceUnavailable, PROTOCOL_VERSION,
    SCOPE_TOOL_INVOKE,
};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::hooks::{HookVerdict, PreToolUse};
use crate::policy::{Policy, Verdict};

const HUB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long a person has to answer before the hub gives up.
///
/// Expiry denies. An approval that times out because nobody was watching
/// must not become an approval.
pub const DEFAULT_APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a forwarded tool call may go unanswered before the hub ends it.
///
/// A backstop, not a per-tool budget. The precise case is a tool server that is
/// still connected and has stopped answering — a wedged browser, a guest
/// deadlocked on its own lock — where no socket closes and nothing else
/// notices. A server that *crashes* is caught immediately and exactly by the
/// disconnect path; this only covers the silence that leaves the socket up.
///
/// Deliberately long. The guest's own `shell.exec` allows a caller-supplied
/// timeout and advertises a maximum of an hour, so anything shorter here would
/// cancel legitimate work and be worse than the hang it replaces. The value of
/// a backstop is that it exists and is finite, not that it is tight.
pub const DEFAULT_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3900);

/// Methods supported beyond the base protocol, advertised in `hello_ack`.
/// Clients gate fallbacks on membership rather than probing.
const EXTRA_CAPABILITIES: &[&str] = &["session_attach_server", "computer.takeover"];

/// Tools the hub serves itself, rather than routing to a tool server.
///
/// Handing work to another Bot is control-plane state, not something the guest
/// owns, and it is an action, so it has to pass the same approval gate as
/// everything else. Serving it here makes both true. The trait keeps the hub
/// from depending on the bot layer: the binary supplies an implementation, so
/// the dependency points inward.
#[async_trait::async_trait]
pub trait InternalTools: Send + Sync {
    /// Advertised alongside the bound tool server's catalogue.
    fn catalog(&self) -> Vec<ToolDescription>;

    /// Whether this provider serves `tool`.
    fn serves(&self, tool: &str) -> bool {
        self.catalog().iter().any(|t| t.tool_id.as_str() == tool)
    }

    /// Invoke `tool`. `as_bot` is the Bot the calling session acts as, if any.
    async fn invoke(
        &self,
        as_bot: Option<&str>,
        tool: &str,
        args: &serde_json::Value,
    ) -> std::result::Result<serde_json::Value, String>;
}

/// Outbound queue for one connection. Unbounded because the writer task drains
/// it continuously; a slow peer is handled by dropping the socket, not by
/// stalling the router and head-of-line-blocking every other session.
pub type Outbox = mpsc::UnboundedSender<String>;

#[derive(Debug)]
struct Conn {
    kind: ConnectionKind,
    principal: Principal,
    server_id: Option<ServerId>,
    tx: Outbox,
}

#[derive(Debug, Default)]
struct Session {
    owner: ConnectionId,
    server: Option<ServerId>,
    tools: Vec<ToolDescription>,
    /// Evaluated here, in the hub. The harness is a client and cannot be
    /// trusted to gate itself (SPEC 6.0).
    policy: Policy,
    /// Which Bot the session acts as, for attributing handoffs.
    bot: Option<String>,
}

/// A tool call in flight: who asked for it, and who is doing it.
///
/// `server` is checked before a progress notification is relayed. `call_id`
/// is chosen by the caller (agents number them `call-1`, `c0`) and is not a
/// secret, so without the sender check any connected client could write into
/// another session's transcript.
#[derive(Debug)]
struct InFlight {
    origin: ConnectionId,
    server: ConnectionId,
}

struct Relay {
    origin_conn: ConnectionId,
    /// The tool server this went to, and therefore the only connection whose
    /// answer counts. Matching a response by id alone would let any connected
    /// client forge a tool's result (a read that never happened, a write
    /// reported as done).
    target: ConnectionId,
    origin_id: RpcId,
    /// Carried here so the call is retired on every terminal path. Reading it
    /// back off a success payload would leak the entry whenever a tool fails,
    /// which is a normal outcome.
    call_id: ToolCallId,
}

#[derive(Default)]
struct State {
    conns: HashMap<ConnectionId, Conn>,
    /// What each tool server said about itself at handshake, kept so
    /// `servers.list` can echo `Hello::description` and `Hello::metadata`.
    servers: HashMap<ServerId, Registered>,
    sessions: HashMap<SessionId, Session>,
    /// Forwarded-request id → who to answer when the reply lands.
    relays: HashMap<String, Relay>,
    /// Hub-originated request id → completion channel.
    hub_calls: HashMap<String, (ConnectionId, oneshot::Sender<Outcome>)>,
    /// In-flight call id → who asked and who is doing it.
    calls: HashMap<ToolCallId, InFlight>,
    /// Policy handed to each new session.
    default_policy: Policy,
    /// Computers a person has taken over, by tool server.
    takeovers: HashMap<ServerId, Takeover>,
}

/// A person is at the keyboard of this computer.
///
/// The unit is the tool server, not the browser namespace. While someone is
/// typing a password into the guest's Chrome, `shell.exec` and `fs.read` on that
/// same guest are equally able to interfere with them or watch them do it, so
/// the lock covers every tool that computer serves. Hub-served tools (`bot.*`,
/// connectors) are unaffected: they do not touch the computer.
#[derive(Debug, Clone)]
struct Takeover {
    holder: ConnectionId,
    session: SessionId,
    /// Why, for the operator reading this later. Required: the agent is locked
    /// out of its own computer for the duration.
    reason: String,
}

pub struct Hub {
    state: Mutex<State>,
    seq: AtomicU64,
    internal: Option<Arc<dyn InternalTools>>,
    /// Injectable so the fail-closed path is reachable in a test without
    /// waiting two minutes.
    approval_timeout: std::time::Duration,
    /// Injectable for the same reason: an hour is not a test.
    call_timeout: std::time::Duration,
    /// `PreToolUse` hooks, consulted here rather than in the client: a check
    /// the caller evaluates is a check the caller can delete (SPEC §6.0).
    hooks: Option<Arc<dyn PreToolUse>>,
    /// Where a credential the person supplies is put.
    ///
    /// Held by the control plane and never by the guest: `secret.request` is
    /// answered here, stored here, and the caller is told only that it worked.
    secrets: Option<Arc<crate::secrets::SecretStore>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Self::with_policy(Policy::default())
    }

    /// Build a hub whose sessions start from `policy`.
    pub fn with_policy(policy: Policy) -> Self {
        Self {
            state: Mutex::new(State {
                default_policy: policy,
                ..Default::default()
            }),
            seq: AtomicU64::new(1),
            internal: None,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            hooks: None,
            secrets: None,
        }
    }

    /// Serve these tools from the hub itself.
    pub fn with_internal_tools(mut self, tools: Arc<dyn InternalTools>) -> Self {
        self.internal = Some(tools);
        self
    }

    /// Let Bots ask a person for a credential they do not have.
    ///
    /// Without a store the tool is not advertised at all, rather than
    /// advertised and failing: a catalogue entry that cannot work teaches a
    /// model to distrust the list.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Arc<crate::secrets::SecretStore>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Consult these `PreToolUse` hooks before running anything.
    pub fn with_hooks(mut self, hooks: Arc<dyn PreToolUse>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// The tools the hub serves itself, as a session would be offered them.
    ///
    /// `pub(crate)` for the boot tests: whether a tool is reachable is a
    /// different question from whether its provider works, and answering it
    /// requires looking at what a session would see.
    pub(crate) fn internal_catalog(&self) -> Vec<ToolDescription> {
        let mut out: Vec<ToolDescription> = self
            .internal
            .as_ref()
            .map(|i| i.catalog())
            .unwrap_or_default();
        if self.secrets.is_some() {
            out.push(secret_request_tool());
        }
        out
    }

    /// Whether a tool id is the hub's own credential request.
    fn is_secret_request(&self, tool: &ToolId) -> bool {
        self.secrets.is_some() && tool.as_str() == SECRET_REQUEST
    }

    /// How long to wait for an approval before denying.
    pub fn with_call_timeout(mut self, d: std::time::Duration) -> Self {
        self.call_timeout = d;
        self
    }

    pub fn with_approval_timeout(mut self, d: std::time::Duration) -> Self {
        self.approval_timeout = d;
        self
    }

    fn next(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.seq.fetch_add(1, Ordering::Relaxed))
    }

    /// Number of tool calls currently in flight.
    ///
    /// Exposed as the leak indicator: every entry must be retired by a
    /// terminal response, so a number that only grows means some terminal
    /// path is not cleaning up.
    pub async fn inflight_calls(&self) -> usize {
        self.state.lock().await.calls.len()
    }

    /// Number of forwarded requests still awaiting a reply.
    pub async fn pending_relays(&self) -> usize {
        self.state.lock().await.relays.len()
    }

    // ── connection lifecycle ──────────────────────────────────────────

    /// Complete the handshake and register the connection.
    pub async fn register(
        self: &Arc<Self>,
        hello: &Hello,
        principal: Principal,
        tx: Outbox,
    ) -> Result<(ConnectionId, HelloAck), RpcError> {
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(RpcError {
                code: codes::INVALID_REQUEST,
                message: format!(
                    "unsupported protocol_version {:?}; this hub speaks {PROTOCOL_VERSION}",
                    hello.protocol_version
                ),
                data: None,
            });
        }
        if hello.kind == ConnectionKind::ToolServer && hello.server_id.is_none() {
            return Err(RpcError {
                code: codes::INVALID_REQUEST,
                message: "a tool_server hello must carry a server_id".into(),
                data: None,
            });
        }

        let id = ConnectionId::new(self.next("conn"));
        let ack = HelloAck {
            connection_id: id.clone(),
            user_id: principal.user_id.clone(),
            hub_version: HUB_VERSION.to_owned(),
            supported_protocol_versions: vec![PROTOCOL_VERSION.to_owned()],
            capabilities: EXTRA_CAPABILITIES.iter().map(|s| (*s).to_owned()).collect(),
        };

        let mut st = self.state.lock().await;
        if let Some(sid) = &hello.server_id {
            // Last writer wins: a reconnecting server replaces its stale entry.
            st.servers.insert(
                sid.clone(),
                Registered {
                    conn: id.clone(),
                    description: hello.description.clone(),
                    metadata: hello.metadata.clone(),
                },
            );
        }
        st.conns.insert(
            id.clone(),
            Conn {
                kind: hello.kind,
                principal,
                server_id: hello.server_id.clone(),
                tx,
            },
        );
        Ok((id, ack))
    }

    /// Drop a connection and everything routed through it.
    pub async fn disconnect(self: &Arc<Self>, id: &ConnectionId) {
        let mut st = self.state.lock().await;
        let conn = st.conns.remove(id);
        let Some(conn) = conn else { return };

        if let Some(sid) = &conn.server_id {
            // Only clear the registry entry if it still points at us; a newer
            // connection for the same server may have already replaced it.
            if st.servers.get(sid).map(|r| &r.conn) == Some(id) {
                st.servers.remove(sid);
            }
            let orphaned: Vec<SessionId> = st
                .sessions
                .iter()
                .filter(|(_, s)| s.server.as_ref() == Some(sid))
                .map(|(k, _)| k.clone())
                .collect();
            for sess in orphaned {
                if let Some(s) = st.sessions.get_mut(&sess) {
                    s.server = None;
                    s.tools.clear();
                }
            }
        }
        // Release any computer this connection had taken over. This lives on
        // the teardown path every dropped socket runs, rather than in the
        // viewer's clean-shutdown path: a Ctrl-C'd viewer, a closed laptop or
        // a crashed tab would otherwise lock the agent out of its own computer
        // permanently, with no command to clear it.
        let released: Vec<ServerId> = st
            .takeovers
            .iter()
            .filter(|(_, t)| t.holder == *id)
            .map(|(k, _)| k.clone())
            .collect();
        for server in released {
            tracing::info!(%server, "takeover released: the person's connection dropped");
            st.takeovers.remove(&server);
        }

        st.sessions.retain(|_, s| s.owner != *id);
        st.calls.retain(|_, c| c.origin != *id);
        st.relays.retain(|_, r| r.origin_conn != *id);
        // An outstanding approval or session.bind aimed at this peer will
        // never be answered now. Dropping the sender resolves the waiter
        // immediately, which for an approval means denied. Leaving it in
        // place would hold the entry until its timeout for no reason.
        st.hub_calls.retain(|_, (target, _)| target != id);
        drop(st);

        // The three `retain`s above filter by *origin*, which releases what a
        // departing harness was waiting on. This is the other direction: a
        // tool server that died with calls in flight. It runs after the lock is
        // released because answering takes it again.
        self.fail_calls_targeting(id, "the tool server's connection dropped")
            .await;
    }

    async fn send(&self, to: &ConnectionId, frame: &Frame) {
        let st = self.state.lock().await;
        if let Some(c) = st.conns.get(to) {
            // A closed receiver just means the peer went away; the disconnect
            // path will clean up.
            let _ = c.tx.send(frame.encode());
        }
    }

    // ── inbound dispatch ──────────────────────────────────────────────

    /// Handle one decoded frame from `from`.
    ///
    /// Requests are handled on their own task. A `tool.call` may have to wait
    /// on an approval sent back down this same connection; handling it inline
    /// would stop the connection reading, so the answer could never arrive
    /// and every gated call would deadlock until it timed out.
    /// Responses carry ids, so answering out of order is legal.
    pub async fn on_frame(self: &Arc<Self>, from: &ConnectionId, frame: Frame) {
        match frame {
            Frame::Request(req) => {
                let me = Arc::clone(self);
                let from = from.clone();
                tokio::spawn(async move {
                    if let Some(resp) = me.on_request(&from, req).await {
                        me.send(&from, &Frame::Response(resp)).await;
                    }
                });
            }
            Frame::Notification(n) => self.on_notification(from, n).await,
            Frame::Response(r) => self.on_response(from, r).await,
        }
    }

    /// Put a hub-originated question to exactly one connection and wait.
    ///
    /// This is the only place a hub-originated question is addressed;
    /// approvals and credential requests both go through it so the three
    /// properties below cannot drift apart:
    ///
    /// * Addressed, not broadcast. `ApprovalRequestParams` carries the tool's
    ///   `args` verbatim and a secret request carries what is being asked for
    ///   and why. Sending either to every client would leak one session's
    ///   commands, paths and message bodies to all the others, while every
    ///   forgery test would still pass because `on_response` checks the owner.
    /// * The owner is recorded before the question leaves. `on_response`
    ///   matches the answer against it, so a stranger cannot answer.
    /// * Fail-closed. No answer inside the timeout means the entry is dropped
    ///   and the caller is told nothing arrived, never that it was fine.
    ///
    /// `None` means no answer came back in time. The caller decides what that
    /// means for it (denied, or no credential).
    ///
    /// This function must not branch on `method`. Only the approval path is
    /// driven end to end by a test; the credential path is covered because the
    /// two share this code, and a special case here would remove that cover
    /// without failing anything.
    async fn ask_owner(
        self: &Arc<Self>,
        owner: &ConnectionId,
        session: &SessionId,
        method: Method,
        params: serde_json::Value,
    ) -> Option<Outcome> {
        let req_id = self.next("hub");
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.state.lock().await;
            st.hub_calls.insert(req_id.clone(), (owner.clone(), tx));
        }
        let frame = Request::new(RpcId::Str(req_id.clone()), method, Some(params))
            .in_session(session.clone());
        self.send(owner, &Frame::Request(frame)).await;

        match tokio::time::timeout(self.approval_timeout, rx).await {
            Ok(Ok(outcome)) => Some(outcome),
            _ => {
                self.state.lock().await.hub_calls.remove(&req_id);
                None
            }
        }
    }

    /// Ask the person who owns a session for a credential.
    ///
    /// A sibling of [`ask_for_approval`](Self::ask_for_approval). Both put
    /// their question through [`ask_owner`](Self::ask_owner), which is where
    /// the addressing, the owner record and the fail-closed timeout live. Only
    /// the params and the answer differ: an approval returns a decision, this
    /// returns a value.
    ///
    /// `None` for every failure: nobody answered, the answer was unreadable,
    /// the connection went away, or the person declined. A Bot is told the
    /// same thing in all of them, because distinguishing them would say
    /// something about the person rather than about the task.
    async fn ask_for_secret(
        self: &Arc<Self>,
        owner: &ConnectionId,
        session: &SessionId,
        name: &str,
        why: &str,
    ) -> Option<String> {
        let params = SecretRequestParams {
            name: name.to_owned(),
            why: why.to_owned(),
            timeout_secs: self.approval_timeout.as_secs(),
        };
        let outcome = match self
            .ask_owner(
                owner,
                session,
                Method::SecretRequest,
                serde_json::to_value(&params).expect("secret request params serialise"),
            )
            .await
        {
            Some(outcome) => outcome,
            None => {
                tracing::warn!(%name, "a credential was not supplied in time");
                return None;
            }
        };
        match outcome {
            Outcome::Result(v) => match serde_json::from_value::<SecretRequestResult>(v) {
                Ok(r) => r.value.filter(|v| !v.trim().is_empty()),
                Err(e) => {
                    // The payload is not logged: whatever it was, it came from
                    // a prompt asking for a credential.
                    tracing::warn!(%name, error = %e, "the answer could not be read");
                    None
                }
            },
            Outcome::Error(e) => {
                tracing::warn!(%name, error = %e.message, "the person declined or errored");
                None
            }
        }
    }

    /// Ask the harness that owns a session to approve a call.
    ///
    /// Returns the decision, or `None` if nobody answered in time. Every
    /// failure path here resolves to "not approved": a timeout, a dropped
    /// connection, or a malformed answer all deny. Proceeding when the
    /// approver could not be reached is the failure mode an approval gate
    /// exists to prevent.
    async fn ask_for_approval(
        self: &Arc<Self>,
        owner: &ConnectionId,
        session: &SessionId,
        call_id: &ToolCallId,
        tool: &ToolId,
        args: &serde_json::Value,
        reason: &str,
    ) -> Result<ApprovalDecision, String> {
        let params = ApprovalRequestParams {
            approval_id: ApprovalId::new(self.next("apr")),
            call_id: call_id.clone(),
            tool_id: tool.clone(),
            args: args.clone(),
            reason: reason.to_owned(),
            timeout_secs: self.approval_timeout.as_secs(),
        };
        let outcome = match self
            .ask_owner(
                owner,
                session,
                Method::ApprovalRequest,
                serde_json::to_value(&params).expect("approval params serialise"),
            )
            .await
        {
            Some(outcome) => outcome,
            None => {
                tracing::warn!(%tool, "approval was not answered in time; denying");
                return Err("no approval was given in time".into());
            }
        };
        // All three failures deny, but each names its own cause. Reporting an
        // unreadable or errored answer as "not answered in time" would send
        // someone who clicked allow to look at the wrong component.
        match outcome {
            Outcome::Result(v) => serde_json::from_value::<ApprovalDecision>(v).map_err(|e| {
                tracing::warn!(%tool, error = %e, "approver sent an unreadable decision; denying");
                format!("the approver's answer could not be read: {e}")
            }),
            Outcome::Error(e) => {
                tracing::warn!(%tool, error = %e.message, "approver returned an error; denying");
                Err(format!("the approver returned an error: {}", e.message))
            }
        }
    }

    async fn on_request(self: &Arc<Self>, from: &ConnectionId, req: Request) -> Option<Response> {
        let id = req.id.clone();
        let Some(method) = req.parsed_method() else {
            // Mirrors the shape older hubs produce, which some clients sniff.
            return Some(Response::err(
                id,
                codes::METHOD_NOT_FOUND,
                format!("unknown method `{}`", req.method),
            ));
        };

        // The connection's role decides which methods are legal on it. A tool
        // server must not be able to open sessions or invoke tools, and a
        // harness must not be able to push a tool snapshot.
        let kind = {
            let st = self.state.lock().await;
            st.conns.get(from).map(|c| c.kind)
        };
        let Some(kind) = kind else {
            return Some(Response::err(
                id,
                codes::INTERNAL_ERROR,
                "unknown connection",
            ));
        };
        if let Some(required) = required_role(method) {
            if kind != required {
                return Some(Response::err(
                    id,
                    codes::FORBIDDEN,
                    format!("`{method}` is not valid on a {kind:?} connection"),
                ));
            }
        }

        let result = match method {
            Method::Ping => Ok(serde_json::to_value(PongFrame::default()).unwrap()),
            Method::SessionOpen => self.session_open(from, &req).await,
            Method::SessionClose => self.session_close(from, &req).await,
            Method::SessionBindServer => return self.session_bind_server(from, req).await,
            Method::ToolsList => self.tools_list(from, &req).await,
            Method::ToolCall => return self.tool_call(from, req).await,
            Method::ServersList => self.servers_list().await,
            Method::ComputerTakeover => self.computer_takeover(from, &req).await,
            Method::ComputerRelease => self.computer_release(from, &req).await,
            Method::Serve => self.serve(from, &req).await,
            other => Err(RpcError {
                code: codes::METHOD_NOT_FOUND,
                message: format!("method `{other}` is not accepted on this connection"),
                data: None,
            }),
        };

        Some(match result {
            Ok(v) => Response {
                jsonrpc: "2.0".into(),
                id,
                outcome: Outcome::Result(v),
            },
            Err(e) => Response {
                jsonrpc: "2.0".into(),
                id,
                outcome: Outcome::Error(e),
            },
        })
    }

    async fn on_notification(self: &Arc<Self>, from: &ConnectionId, n: Notification) {
        let Some(method) = n.parsed_method() else {
            return;
        };
        if method != Method::ToolCallProgress {
            return;
        }
        let Some(p) = n.params.clone() else { return };
        let Ok(frame) = serde_json::from_value::<ToolCallProgressFrame>(p) else {
            return;
        };

        // Relay to whichever harness owns this call, and only from the server
        // running it. Anything else is a stranger writing into a transcript
        // somebody reads to see what their computer did.
        let target = {
            let st = self.state.lock().await;
            match st.calls.get(&frame.call_id) {
                Some(c) if c.server == *from => Some(c.origin.clone()),
                Some(c) => {
                    tracing::warn!(
                        call_id = %frame.call_id, %from, expected = %c.server,
                        "a connection sent progress for a call it is not running; ignored"
                    );
                    None
                }
                None => None,
            }
        };
        if let Some(t) = target {
            let mut fwd = Notification::new(Method::ToolCallProgress, &frame);
            fwd.session_id = n.session_id;
            self.send(&t, &Frame::Notification(fwd)).await;
        } else {
            tracing::debug!(call_id = %frame.call_id, from = %from, "progress for an unknown call");
        }
    }

    /// A reply to something the hub sent out: either a relayed harness request
    /// or a hub-originated one.
    async fn on_response(self: &Arc<Self>, from: &ConnectionId, resp: Response) {
        let key = rpc_id_key(&resp.id);

        // Only the connection the hub asked may answer it.
        //
        // Hub request ids come from one counter (`hub-0`, `hub-1`, ...), so
        // matching by id alone would let any connected client answer a request
        // it was never sent: approve another session's `shell.exec`, or reply
        // `allow_always` and take that tool off the gate for the rest of the
        // session. The gate is evaluated in the hub so the caller cannot
        // delete it (SPEC §6.0), and that guarantee is only as strong as the
        // answer it waits for.
        //
        // Checked before removing anything: a stranger's reply must not
        // consume the entry the real answer is still on its way to.
        let (relay, waiter) = {
            let mut st = self.state.lock().await;
            let waiter = match st.hub_calls.get(&key) {
                Some((owner, _)) if owner == from => st.hub_calls.remove(&key).map(|(_, tx)| tx),
                Some((owner, _)) => {
                    tracing::warn!(
                        id = %key, %from, expected = %owner,
                        "a connection answered an approval it was not asked; ignored"
                    );
                    None
                }
                None => None,
            };
            let relay = match st.relays.get(&key) {
                Some(r) if r.target == *from => st.relays.remove(&key),
                Some(r) => {
                    tracing::warn!(
                        id = %key, %from, expected = %r.target,
                        "a connection answered a tool call it was not sent; ignored"
                    );
                    None
                }
                None => None,
            };
            (relay, waiter)
        };

        if let Some(tx) = waiter {
            let _ = tx.send(resp.outcome);
            return;
        }

        let Some(relay) = relay else {
            tracing::debug!(id = %key, "response with no pending request");
            return;
        };

        self.finish_relay(relay, resp.outcome).await;
    }

    /// End a forwarded call, whichever of its three endings arrived.
    ///
    /// A tool call could only end one way before this existed: the server
    /// answering. If the server died or simply never replied, the relay stayed
    /// in the map forever and the harness was told nothing — the Bot stopped
    /// mid-task, the transcript ended without an error, and `computer status`
    /// showed a healthy reconnected guest. `Hub::inflight_calls` is documented
    /// as the leak indicator and this was a leak it reported that nothing acted
    /// on.
    ///
    /// The three endings are the server's response, the server's disconnect,
    /// and a deadline. They go through one function because the ordering here
    /// is load-bearing and easy to get subtly different in three places: the
    /// call is retired *before* the answer is sent, so a progress frame that
    /// arrives late is dropped rather than delivered after the terminal.
    ///
    /// Runs on success and failure alike. A failed tool is an ordinary outcome
    /// and must not leak either.
    async fn finish_relay(self: &Arc<Self>, relay: Relay, outcome: Outcome) {
        self.state.lock().await.calls.remove(&relay.call_id);
        self.send(
            &relay.origin_conn,
            &Frame::Response(Response {
                jsonrpc: "2.0".into(),
                id: relay.origin_id,
                outcome,
            }),
        )
        .await;
    }

    /// Answer every call still waiting on a connection that has gone away.
    ///
    /// `disconnect` cleaned up `calls` and `relays` by *origin*, which releases
    /// the entries belonging to a harness that left. A relay whose **target**
    /// disappeared — the tool server crashing, which is the common case, since
    /// it drives a browser — matched neither filter and was left in the map
    /// with nobody coming to answer it.
    ///
    /// `WorkspaceUnavailable` was defined in the protocol for exactly this, with
    /// a `Disconnect` reason and an `InFlightCancelled` phase, and had no uses
    /// anywhere outside `botroster-proto`.
    async fn fail_calls_targeting(self: &Arc<Self>, gone: &ConnectionId, reason: &str) {
        let orphaned: Vec<Relay> = {
            let mut st = self.state.lock().await;
            let keys: Vec<String> = st
                .relays
                .iter()
                .filter(|(_, r)| r.target == *gone)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| st.relays.remove(k)).collect()
        };
        if orphaned.is_empty() {
            return;
        }
        tracing::warn!(
            conn = %gone, calls = orphaned.len(),
            "the tool server went away with calls in flight; failing them"
        );
        let err = WorkspaceUnavailable {
            reason: WorkspaceGoneReason::Disconnect,
            phase: WorkspaceGonePhase::InFlightCancelled,
            detail: Some(reason.to_owned()),
        }
        .to_rpc_error();
        for relay in orphaned {
            self.finish_relay(relay, Outcome::Error(err.clone())).await;
        }
    }

    // ── method handlers ───────────────────────────────────────────────

    async fn session_open(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let p: SessionOpenParams = parse_params(req)?;
        let sid = p
            .session_id
            .unwrap_or_else(|| SessionId::new(self.next("sess")));

        let mut st = self.state.lock().await;
        let policy = st.default_policy.clone();
        st.sessions.insert(
            sid.clone(),
            Session {
                owner: from.clone(),
                server: None,
                tools: Vec::new(),
                policy,
                bot: p.bot.clone(),
            },
        );
        // The principal may now act on the session; dispatch checks it later.
        if let Some(c) = st.conns.get_mut(from) {
            c.principal.session_ids.push(sid.clone());
        }
        Ok(serde_json::to_value(SessionOpenResult { session_id: sid }).unwrap())
    }

    async fn session_close(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let sid = require_session(req)?;
        let mut st = self.state.lock().await;
        match st.sessions.get(&sid) {
            Some(s) if s.owner == *from => {
                st.sessions.remove(&sid);
                Ok(serde_json::json!({}))
            }
            Some(_) => Err(err(codes::FORBIDDEN, "not the owner of this session")),
            None => Err(err(codes::SESSION_NOT_FOUND, "no such session")),
        }
    }

    /// Bind a tool server to a session: ask the server to serve it, wait for its
    /// snapshot, cache it, and hand it back to the harness.
    async fn session_bind_server(
        self: &Arc<Self>,
        from: &ConnectionId,
        req: Request,
    ) -> Option<Response> {
        let origin_id = req.id.clone();
        let bind = || -> Result<(SessionId, ServerId), RpcError> {
            Ok((
                require_session(&req)?,
                parse_required::<SessionBindServerParams>(&req)?.server_id,
            ))
        };
        let (sid, server_id) = match bind() {
            Ok(v) => v,
            Err(e) => {
                return Some(Response {
                    jsonrpc: "2.0".into(),
                    id: origin_id,
                    outcome: Outcome::Error(e),
                })
            }
        };

        let server_conn = {
            let st = self.state.lock().await;
            match st.sessions.get(&sid) {
                Some(s) if s.owner != *from => {
                    return Some(Response::err(
                        origin_id,
                        codes::FORBIDDEN,
                        "not the owner of this session",
                    ))
                }
                None => {
                    return Some(Response::err(
                        origin_id,
                        codes::SESSION_NOT_FOUND,
                        "no such session",
                    ))
                }
                _ => {}
            }
            st.servers.get(&server_id).map(|r| r.conn.clone())
        };
        let Some(server_conn) = server_conn else {
            return Some(Response::err(
                origin_id,
                codes::NO_SERVER_BOUND,
                format!("no tool server registered as `{server_id}`"),
            ));
        };

        // Hub-originated request: the hub awaits the reply itself.
        let call_id = self.next("hub");
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.state.lock().await;
            st.hub_calls
                .insert(call_id.clone(), (server_conn.clone(), tx));
        }
        let bind_req = Request::new(
            RpcId::Str(call_id.clone()),
            Method::SessionBind,
            Some(serde_json::to_value(SessionBindParams {}).unwrap()),
        )
        .in_session(sid.clone());
        self.send(&server_conn, &Frame::Request(bind_req)).await;

        let outcome = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(o)) => o,
            _ => {
                self.state.lock().await.hub_calls.remove(&call_id);
                return Some(Response::err(
                    origin_id,
                    codes::INTERNAL_ERROR,
                    "tool server did not answer session.bind in time",
                ));
            }
        };

        let value = match outcome {
            Outcome::Result(v) => v,
            Outcome::Error(e) => {
                return Some(Response {
                    jsonrpc: "2.0".into(),
                    id: origin_id,
                    outcome: Outcome::Error(e),
                })
            }
        };
        let snapshot: SessionBindResult = match serde_json::from_value(value) {
            Ok(v) => v,
            Err(e) => {
                return Some(Response::err(
                    origin_id,
                    codes::INTERNAL_ERROR,
                    format!("malformed session.bind result: {e}"),
                ))
            }
        };

        {
            let mut st = self.state.lock().await;
            if let Some(s) = st.sessions.get_mut(&sid) {
                s.server = Some(server_id);
                s.tools = snapshot.tools.clone();
            }
        }
        Some(Response::ok(
            origin_id,
            SessionBindServerResult {
                // The guest's tools and the hub's own arrive as one
                // catalogue: a model should not have to know which side of
                // the wire a tool lives on.
                tools: snapshot
                    .tools
                    .into_iter()
                    .chain(self.internal_catalog())
                    .collect(),
            },
        ))
    }

    async fn tools_list(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let sid = require_session(req)?;
        let st = self.state.lock().await;
        let s = st
            .sessions
            .get(&sid)
            .ok_or_else(|| err(codes::SESSION_NOT_FOUND, "no such session"))?;
        if s.owner != *from {
            return Err(err(codes::FORBIDDEN, "not the owner of this session"));
        }
        Ok(serde_json::to_value(ToolsListResult {
            tools: s
                .tools
                .iter()
                .cloned()
                .chain(self.internal_catalog())
                .collect(),
        })
        .unwrap())
    }

    /// Forward a harness `tool.call` to the bound tool server.
    async fn tool_call(self: &Arc<Self>, from: &ConnectionId, req: Request) -> Option<Response> {
        let origin_id = req.id.clone();

        let prep = || -> Result<(SessionId, ToolCallRequestParams), RpcError> {
            Ok((
                require_session(&req)?,
                parse_required::<ToolCallRequestParams>(&req)?,
            ))
        };
        let (sid, params) = match prep() {
            Ok(v) => v,
            Err(e) => {
                return Some(Response {
                    jsonrpc: "2.0".into(),
                    id: origin_id,
                    outcome: Outcome::Error(e),
                })
            }
        };

        // Policy first: nothing is dispatched, and no tool server is even
        // contacted, until the verdict is Allow.
        let (verdict, owner) = {
            let st = self.state.lock().await;
            match st.sessions.get(&sid) {
                Some(s) => (
                    s.policy.evaluate(params.tool_id.as_str(), &params.args),
                    s.owner.clone(),
                ),
                None => {
                    return Some(Response::err(
                        origin_id,
                        codes::SESSION_NOT_FOUND,
                        "no such session",
                    ))
                }
            }
        };

        // A `PreToolUse` hook gets its veto before anyone is asked anything.
        //
        // After a policy deny (nothing to add, it is already refused) and
        // before an approval prompt: a hook that blocks a call should stop it
        // reaching a person at all. Waking someone to approve something a hook
        // was going to refuse trains them to answer prompts on autopilot.
        if !matches!(verdict, Verdict::Deny(_)) {
            if let Some(hooks) = self.hooks.clone() {
                if let HookVerdict::Deny(why) = hooks
                    .check(&sid, params.tool_id.as_str(), &params.args)
                    .await
                {
                    return Some(Response::err(
                        origin_id,
                        codes::APPROVAL_DENIED,
                        format!("refused by a PreToolUse hook: {why}"),
                    ));
                }
            }
        }

        match verdict {
            Verdict::Allow => {}
            Verdict::Deny(reason) => {
                return Some(Response::err(
                    origin_id,
                    codes::APPROVAL_DENIED,
                    format!("refused by policy: {reason}"),
                ))
            }
            Verdict::Ask(reason) => {
                let decision = self
                    .ask_for_approval(
                        &owner,
                        &sid,
                        &params.call_id,
                        &params.tool_id,
                        &params.args,
                        &reason,
                    )
                    .await;
                match decision {
                    Ok(d) if d.decision.permits() => {
                        if d.decision == Decision::AllowAlways {
                            let mut st = self.state.lock().await;
                            if let Some(sess) = st.sessions.get_mut(&sid) {
                                sess.policy.allow_from_now_on(params.tool_id.as_str());
                            }
                        }
                    }
                    Ok(d) => {
                        let note = d.note.unwrap_or_else(|| "denied".into());
                        return Some(Response::err(
                            origin_id,
                            codes::APPROVAL_DENIED,
                            format!("denied by the approver: {note}"),
                        ));
                    }
                    Err(why) => return Some(Response::err(origin_id, codes::APPROVAL_DENIED, why)),
                }
            }
        }

        // The credential request, answered by the hub itself.
        //
        // Served here rather than by an internal-tools implementation because
        // it needs the ask channel, which is the hub's: a `SecretStore` alone
        // could only write, and this tool must ask first.
        //
        // The value goes into the store and the caller is told the name and a
        // fingerprint. `SecretStoredResult` has no field for the value at all:
        // a tool result travels into the conversation, which is written to
        // disk and rendered by every client.
        if self.is_secret_request(&params.tool_id) {
            let name = params
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_owned();
            let why = params
                .args
                .get("why")
                .and_then(|v| v.as_str())
                .unwrap_or("no reason given")
                .to_owned();
            if name.is_empty() {
                return Some(Response::err(
                    origin_id,
                    codes::INVALID_PARAMS,
                    "a credential needs a name to be stored under",
                ));
            }

            let Some(value) = self.ask_for_secret(from, &sid, &name, &why).await else {
                return Some(Response::err(
                    origin_id,
                    codes::APPROVAL_DENIED,
                    // One sentence for refusal, timeout and unreadable answer
                    // alike; see `ask_for_secret`.
                    format!("no credential was supplied for `{name}`"),
                ));
            };
            let store = self.secrets.clone().expect("checked by is_secret_request");
            let secret = crate::secrets::Secret::new(value);
            let fingerprint = secret.fingerprint();
            if let Err(e) = store.set(&name, secret) {
                return Some(Response::err(
                    origin_id,
                    codes::TOOL_FAILED,
                    format!("could not store `{name}`: {e}"),
                ));
            }
            return Some(Response::ok(
                origin_id,
                ToolCallResult {
                    call_id: params.call_id,
                    output: serde_json::to_value(SecretStoredResult { name, fingerprint })
                        .expect("the stored result serialises"),
                },
            ));
        }

        // Internal tools run in the hub. They passed the same policy gate
        // above, so a handoff is approved exactly like a write or a shell
        // command, which is the point of serving it here rather than in a
        // client that could skip the check.
        if let Some(internal) = self.internal.clone() {
            if internal.serves(params.tool_id.as_str()) {
                let as_bot = {
                    let st = self.state.lock().await;
                    st.sessions.get(&sid).and_then(|s| s.bot.clone())
                };
                return Some(
                    match internal
                        .invoke(as_bot.as_deref(), params.tool_id.as_str(), &params.args)
                        .await
                    {
                        Ok(output) => Response::ok(
                            origin_id,
                            ToolCallResult {
                                call_id: params.call_id,
                                output,
                            },
                        ),
                        Err(e) => Response::err(origin_id, codes::TOOL_FAILED, e),
                    },
                );
            }
        }

        let server_conn = {
            let st = self.state.lock().await;
            let Some(conn) = st.conns.get(from) else {
                return Some(Response::err(
                    origin_id,
                    codes::INTERNAL_ERROR,
                    "unknown connection",
                ));
            };
            // Both checks matter: the scope proves this principal may invoke
            // tools at all, the session check proves it may act on this one.
            if !conn.principal.has_scope(SCOPE_TOOL_INVOKE) {
                return Some(Response::err(
                    origin_id,
                    codes::FORBIDDEN,
                    "missing scope tool.invoke",
                ));
            }
            // Ownership is read from the session table rather than the
            // principal's cached id list. Requests are handled concurrently,
            // so a client that pipelines `session_open` and `tool.call`
            // without awaiting could otherwise reach this check before the
            // open had appended the id. The table is written under this same
            // lock, so it cannot race.
            match st.sessions.get(&sid) {
                Some(s) if s.owner == *from => {}
                Some(_) => {
                    return Some(Response::err(
                        origin_id,
                        codes::FORBIDDEN,
                        "not the owner of this session",
                    ))
                }
                None => {
                    return Some(Response::err(
                        origin_id,
                        codes::SESSION_NOT_FOUND,
                        "no such session",
                    ))
                }
            }
            let Some(s) = st.sessions.get(&sid) else {
                return Some(Response::err(
                    origin_id,
                    codes::SESSION_NOT_FOUND,
                    "no such session",
                ));
            };
            let Some(server) = s.server.clone() else {
                return Some(Response::err(
                    origin_id,
                    codes::NO_SERVER_BOUND,
                    "no tool server is bound to this session",
                ));
            };

            // Somebody is at this keyboard. Refuse rather than queue: an agent
            // held until release would resume minutes later and act on a page
            // that had completely changed underneath it. A distinct code lets
            // it report "a person took over" and wait, instead of reading a
            // permission failure and abandoning the task.
            if let Some(t) = st.takeovers.get(&server) {
                if t.session != sid {
                    return Some(Response::err(
                        origin_id,
                        codes::TAKEN_OVER,
                        format!("a person has taken over this computer: {}", t.reason),
                    ));
                }
            }

            match st.servers.get(&server) {
                Some(r) => r.conn.clone(),
                None => {
                    return Some(Response::err(
                        origin_id,
                        codes::NO_SERVER_BOUND,
                        "no tool server is bound to this session",
                    ))
                }
            }
        };

        let fwd_id = self.next("fwd");
        let fwd_id_for_deadline = fwd_id.clone();
        {
            let mut st = self.state.lock().await;
            st.relays.insert(
                fwd_id.clone(),
                Relay {
                    origin_conn: from.clone(),
                    target: server_conn.clone(),
                    origin_id: origin_id.clone(),
                    call_id: params.call_id.clone(),
                },
            );
            st.calls.insert(
                params.call_id.clone(),
                InFlight {
                    origin: from.clone(),
                    server: server_conn.clone(),
                },
            );
        }

        let fwd = Request::new(
            RpcId::Str(fwd_id),
            Method::ToolCallRequest,
            Some(serde_json::to_value(&params).unwrap()),
        )
        .in_session(sid);
        self.send(&server_conn, &Frame::Request(fwd)).await;

        // The backstop. A server that crashes is answered at once by the
        // disconnect path; this is for one that stays connected and stops
        // answering, where no socket closes and nothing else ever notices.
        //
        // A task per call rather than one sweeper: the relay id it closes over
        // is the whole of its state, it exits on the deadline either way, and a
        // sweeper would need its own interval, its own cancellation, and a
        // reason to exist that a `sleep` does not already cover.
        {
            let hub = Arc::clone(self);
            let key = fwd_id_for_deadline;
            let wait = self.call_timeout;
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                // Gone by now in the ordinary case, and `finish_relay` is only
                // reached if this task is the one that removed it, so a call
                // that has already been answered cannot be answered twice.
                let relay = hub.state.lock().await.relays.remove(&key);
                if let Some(relay) = relay {
                    tracing::warn!(
                        id = %key, ?wait,
                        "a tool server accepted a call and never answered; failing it"
                    );
                    let err = WorkspaceUnavailable {
                        reason: WorkspaceGoneReason::IdleTimeout,
                        phase: WorkspaceGonePhase::InFlightCancelled,
                        detail: Some(format!("no answer from the tool server in {wait:?}")),
                    }
                    .to_rpc_error();
                    hub.finish_relay(relay, Outcome::Error(err)).await;
                }
            });
        }

        // No immediate reply: the response arrives via on_response.
        None
    }

    async fn servers_list(&self) -> Result<serde_json::Value, RpcError> {
        let st = self.state.lock().await;
        let servers = st
            .servers
            .iter()
            .map(|(id, r)| ServerInfo {
                server_id: id.clone(),
                description: r.description.clone(),
                metadata: r.metadata.clone(),
            })
            .collect();
        Ok(serde_json::to_value(ServersListResult { servers }).unwrap())
    }

    /// Claim a computer for a person at a keyboard.
    ///
    /// Enforced here rather than in whatever is drawing the screen. A viewer
    /// that only changes its own UI leaves the agent free to click while
    /// someone types a one-time code, which is the moment this feature exists
    /// for.
    async fn computer_takeover(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let p: ComputerTakeoverParams = parse_required(req)?;
        let sid = req
            .session_id
            .clone()
            .ok_or_else(|| err(codes::INVALID_PARAMS, "takeover needs a session"))?;

        let mut st = self.state.lock().await;
        match st.sessions.get(&sid) {
            Some(s) if s.owner == *from => {}
            Some(_) => return Err(err(codes::FORBIDDEN, "not the owner of this session")),
            None => return Err(err(codes::SESSION_NOT_FOUND, "no such session")),
        }
        if !st.servers.contains_key(&p.server_id) {
            return Err(err(codes::NO_SERVER_BOUND, "no such tool server"));
        }

        let claimed = match st.takeovers.get(&p.server_id) {
            // Idempotent: a viewer that reconnects and re-claims should not
            // have to track whether it already holds the lock.
            Some(t) if t.session == sid => false,
            Some(t) => {
                return Err(err(
                    codes::TAKEN_OVER,
                    &format!("someone else already has this computer: {}", t.reason),
                ))
            }
            None => true,
        };
        if claimed {
            tracing::info!(server = %p.server_id, reason = %p.reason, "a person took over");
            st.takeovers.insert(
                p.server_id.clone(),
                Takeover {
                    holder: from.clone(),
                    session: sid,
                    reason: p.reason,
                },
            );
        }
        Ok(serde_json::to_value(ComputerTakeoverResult {
            server_id: p.server_id,
            claimed,
        })
        .unwrap())
    }

    /// Give the computer back.
    ///
    /// Releasing a computer nobody holds is not an error: the caller wanted
    /// the computer free, and it is.
    async fn computer_release(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let p: ComputerReleaseParams = parse_required(req)?;
        let mut st = self.state.lock().await;

        let released = match st.takeovers.get(&p.server_id) {
            Some(t) if t.holder == *from => {
                st.takeovers.remove(&p.server_id);
                tracing::info!(server = %p.server_id, "the person gave the computer back");
                true
            }
            // Only the holder may release, or the agent could release the
            // person mid-password.
            Some(_) => return Err(err(codes::FORBIDDEN, "someone else holds this computer")),
            None => false,
        };
        Ok(serde_json::to_value(ComputerReleaseResult {
            server_id: p.server_id,
            released,
        })
        .unwrap())
    }

    /// A server pushing a fresh full snapshot. Idempotent: it replaces the tool
    /// set, and the hub computes the diff so that logic lives in one place.
    async fn serve(
        &self,
        from: &ConnectionId,
        req: &Request,
    ) -> Result<serde_json::Value, RpcError> {
        let p: ServeParams = parse_params(req)?;
        let sid = require_session(req)?;

        let mut st = self.state.lock().await;
        let Some(session) = st.sessions.get_mut(&sid) else {
            return Err(err(codes::SESSION_NOT_FOUND, "no such session"));
        };

        let before: Vec<_> = session.tools.iter().map(|t| t.tool_id.clone()).collect();
        let after: Vec<_> = p.tools.iter().map(|t| t.tool_id.clone()).collect();
        let added: Vec<_> = after
            .iter()
            .filter(|t| !before.contains(t))
            .cloned()
            .collect();
        let removed: Vec<_> = before
            .iter()
            .filter(|t| !after.contains(t))
            .cloned()
            .collect();

        session.tools = p.tools;
        let owner = session.owner.clone();
        let accepted = after.len();
        drop(st);

        if !added.is_empty() || !removed.is_empty() {
            let n = Notification::new(
                Method::ToolsChanged,
                ToolsChanged {
                    added: added.clone(),
                    removed: removed.clone(),
                },
            )
            .in_session(sid);
            self.send(&owner, &Frame::Notification(n)).await;
        }
        tracing::debug!(%from, accepted, "serve snapshot applied");
        Ok(serde_json::to_value(ServeResult {
            accepted,
            added,
            removed,
        })
        .unwrap())
    }
}

// ── helpers ───────────────────────────────────────────────────────────

/// The tool a Bot calls when it needs a credential the account does not hold.
const SECRET_REQUEST: &str = "secret.request";

/// How the credential request is advertised to a model.
///
/// The description is written for the model rather than for a person: it has
/// to be clear that asking is the only way to get one, or a model that cannot
/// find a token will try to talk somebody through pasting it into the chat,
/// which is the failure the broker exists to prevent.
fn secret_request_tool() -> ToolDescription {
    ToolDescription::new(
        SECRET_REQUEST,
        concat!(
            "Ask the person for a credential you do not have, by name. ",
            "They enter it themselves and it is stored for you; you never see ",
            "the value, and you must never ask for one in conversation. ",
            "Use the stored name with a connector afterwards.",
        ),
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "What to store it as, e.g. `linear-token`."
                },
                "why": {
                    "type": "string",
                    "description": "Why you need it, in one sentence, shown to the person."
                }
            },
            "required": ["name", "why"]
        }),
    )
}

/// A tool server's own account of itself, from its handshake.
struct Registered {
    conn: ConnectionId,
    description: Option<String>,
    metadata: Option<serde_json::Value>,
}

/// Which kind of connection may issue a method, if it is restricted at all.
///
/// `pub` so a test can walk `Method::ALL` and check the table against a real
/// connection rather than against a hand-maintained list. The enforcement is
/// in `on_request`; this is only the decision, and exposing a pure predicate
/// lets `every_harness_only_method_is_refused_to_a_guest` cover a method the
/// moment it is added to the group.
///
/// The match is total, with no `_` arm. This is an authorization table, and a
/// wildcard here fails open: a method added to the dispatch without a
/// decision made about it would be callable by any connection, including a
/// guest, the least-trusted thing attached to the hub. Adding a variant stops
/// the build until it is classified. `carried` in `botroster-cli` makes the same
/// argument for the event stream.
pub fn required_role(m: Method) -> Option<ConnectionKind> {
    use ConnectionKind::*;
    match m {
        // A person's side of the account. A guest issuing any of these would
        // not need to escape the workspace to do harm: it could open its own
        // session, drive another computer, or take one over and lock the
        // person out of their own keyboard.
        Method::SessionOpen
        | Method::SessionClose
        | Method::SessionBindServer
        | Method::SessionUnbindServer
        | Method::SessionAttachServer
        | Method::ToolsList
        | Method::ToolsSearch
        | Method::ToolCall
        | Method::ToolCancel
        | Method::ComputerTakeover
        | Method::ComputerRelease
        | Method::ServersList => Some(Harness),

        // A computer's side. A harness pushing a catalogue would be inventing
        // tools nothing serves.
        Method::Serve | Method::ToolServerStatus => Some(ToolServer),

        // Either side may say these.
        Method::Ping | Method::Pong | Method::Hello | Method::HelloAck => None,

        // Hub-originated, or answers to something the hub sent. They are not
        // dispatched as incoming requests at all (the dispatch refuses
        // anything it does not name), and their authorization is the sender
        // check on the pending entry, not a role.
        Method::ApprovalRequest
        | Method::SecretRequest
        | Method::ToolCallRequest
        | Method::SessionBind
        | Method::SessionUnbind
        | Method::ToolCallProgress
        | Method::ToolsChanged
        | Method::HookReply
        | Method::Hook => None,

        // Operator verbs, wire-compatible and not implemented here. Asking a
        // computer for its status is a read; evicting one is not: a guest able
        // to evict another guest could take a competitor's computer off the
        // account. Neither is dispatched, and whichever implements them owes
        // them a decision above rather than this line.
        Method::ToolServerGetStatus | Method::ToolServerEvict => Some(Harness),

        // Not implemented here. Unrestricted only because the dispatch refuses
        // them by name; if one is ever handled, it needs a decision above
        // rather than this line.
        Method::ToolNotify
        | Method::SystemNotify
        | Method::ToolNotification
        | Method::SubscribeNotifications
        | Method::UnsubscribeNotifications
        | Method::SubscribeAck
        | Method::UnsubscribeAck
        | Method::TracesDonate
        | Method::LogsDonate
        | Method::MetricsDonate => None,
    }
}

fn err(code: i32, msg: &str) -> RpcError {
    RpcError {
        code,
        message: msg.to_owned(),
        data: None,
    }
}

fn rpc_id_key(id: &RpcId) -> String {
    match id {
        RpcId::Num(n) => n.to_string(),
        RpcId::Str(s) => s.clone(),
    }
}

/// Params for a method whose body is optional; absent means "all defaults".
fn parse_params<T: serde::de::DeserializeOwned + Default>(req: &Request) -> Result<T, RpcError> {
    match &req.params {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| err(codes::INVALID_PARAMS, &format!("invalid params: {e}"))),
    }
}

/// Params for a method that is meaningless without a body. Defaulting these
/// would silently invent a tool id or a server id, so absence is an error.
fn parse_required<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, RpcError> {
    let v = req
        .params
        .clone()
        .filter(|v| !v.is_null())
        .ok_or_else(|| err(codes::INVALID_PARAMS, "params are required for this method"))?;
    serde_json::from_value(v)
        .map_err(|e| err(codes::INVALID_PARAMS, &format!("invalid params: {e}")))
}

fn require_session(req: &Request) -> Result<SessionId, RpcError> {
    req.session_id.clone().ok_or_else(|| {
        err(
            codes::INVALID_REQUEST,
            "this method requires an envelope session_id",
        )
    })
}

/// Development principal, used until OIDC validation is wired.
pub fn dev_principal() -> Principal {
    Principal::new(UserId::new("dev-user")).with_scope(SCOPE_TOOL_INVOKE)
}
