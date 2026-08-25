//! Computer Hub wire protocol: JSON-RPC 2.0 over a single WebSocket.
//!
//! A harness (the agent) and one or more tool servers (the guest daemon, MCP
//! bridges) each hold one socket to the hub. The hub routes `tool.call` from a
//! harness to the tool server bound to that session, as `tool_call_request`.
//!
//! Reimplemented for wire compatibility with the protocol published in
//! `xai-org/grok-build` (`crates/common/xai-tool-protocol`, Apache-2.0).
//! See `../../PROVENANCE.md`. The structure derives from that published
//! interface; the implementation is independent.

#![forbid(unsafe_code)]

pub mod approval;
pub mod frames;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─────────────────────────── version ───────────────────────────

/// Wire-protocol version both ends speak.
///
/// Bumped only for an incompatible schema change. Additive methods go through
/// capability negotiation ([`HelloAck::capabilities`]) instead: clients gate
/// per-call fallbacks on membership rather than probing.
pub const PROTOCOL_VERSION: &str = "1.0.0";

// ─────────────────────────── identifiers ───────────────────────────

macro_rules! id_newtype {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self { Self(s.to_owned()) }
        }
    )*};
}

id_newtype! {
    /// Hub-assigned, unique per WebSocket connection.
    ConnectionId,
    /// Derived by the hub from the upgrade credential; never announced by the client.
    UserId,
    /// A logical work session. One connection may bind several over its life.
    SessionId,
    /// Stable identity of a tool server, for `servers.list` / `session_bind_server`.
    ServerId,
    /// Namespaced tool identifier.
    ToolId,
    /// Correlates a call with its progress frames and terminal result.
    ToolCallId,
}

// ─────────────────────────── connection roles ───────────────────────────

/// Role of a WebSocket connection. The hub uses this to decide which methods
/// are legal on a given socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    /// The agent. Calls tools.
    Harness,
    /// Owns tools and executes them. The guest daemon is one of these.
    ToolServer,
}

// ─────────────────────────── handshake ───────────────────────────

/// First frame the client sends after the WebSocket upgrade succeeds.
///
/// Carries no session ids: a connection starts with an empty bound-session set
/// and binds sessions dynamically over its lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: String,
    pub kind: ConnectionKind,
    /// Set only for [`ConnectionKind::ToolServer`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<ServerId>,
    /// One-line description surfaced in `servers.list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Opaque metadata, echoed in `ServerInfo.metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// The hub's per-home token, read from `OPENBOT_HUB_TOKEN` or from
    /// `<home>/hub.token`.
    ///
    /// # What this defends, and what it does not
    ///
    /// The hub used to hand every connection `dev_principal()`. Approvals are
    /// authorised on socket identity, so a connection that opens its own
    /// session is the owner of it — and is therefore the one the hub asks for
    /// permission. Anything that could open a socket could approve its own
    /// `shell.exec`.
    ///
    /// A shared secret raises that bar from *anything that can open a socket*
    /// to *anything that can read this user's files*. That stops a remote
    /// caller, another user on the machine, and any program that is not
    /// looking for the file.
    ///
    /// **It is not isolation from a program running as this user.** Such a
    /// program can read `<home>/hub.token` exactly as the desktop client does.
    /// The backlog entry that asked for this called it "what defends against a
    /// local process", and that is true only for a local process belonging to
    /// somebody else. Nothing here changes what a process running as you can
    /// reach; `CLAUDE.md` is explicit that this repository does not claim
    /// isolation it does not have.
    ///
    /// `Option` because a hub that has not been given a token accepts a
    /// connection without one, which is what every client did before this
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Hello {
    pub fn harness() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            kind: ConnectionKind::Harness,
            server_id: None,
            description: None,
            metadata: None,
            token: hub_token(),
        }
    }

    pub fn tool_server(server_id: impl Into<ServerId>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            kind: ConnectionKind::ToolServer,
            server_id: Some(server_id.into()),
            description: None,
            metadata: None,
            token: hub_token(),
        }
    }

    /// Present a token explicitly, for a caller that found one somewhere other
    /// than the environment.
    #[must_use]
    pub fn with_token(mut self, token: Option<String>) -> Self {
        if token.is_some() {
            self.token = token;
        }
        self
    }

    pub fn with_description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    pub fn with_metadata(mut self, m: Value) -> Self {
        self.metadata = Some(m);
        self
    }
}

/// The `metadata` key a tool server reports its workspace root under.
///
/// Only the running guest knows its own workspace root. The root is chosen at
/// startup (a durable volume by default, or any directory given to
/// `openbot up --workspace`), so reconstructing it from flags or from the store
/// layout is wrong whenever the other option was chosen, and wrong silently: a
/// file copied to the volume while the guest runs on a plain directory is
/// invisible to the Bot, yet the copy reports success.
///
/// `servers.list` echoes this key, so clients can query the root instead of
/// assuming it.
pub const META_WORKSPACE: &str = "workspace";

/// The hub's reply to [`Hello`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub connection_id: ConnectionId,
    /// Resolved by the hub from the upgrade credential (JWT `sub`, dev hash, …).
    /// The client never announces its own identity.
    pub user_id: UserId,
    pub hub_version: String,
    pub supported_protocol_versions: Vec<String>,
    /// Methods this hub supports beyond the base protocol. Additive; gate
    /// fallbacks on membership, do not probe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

// ─────────────────────────── authorisation ───────────────────────────

/// Scope required to invoke a tool.
pub const SCOPE_TOOL_INVOKE: &str = "tool.invoke";

/// Authenticated identity, established once at connect.
///
/// Subsequent dispatch carries no further credentials; the router narrows by
/// [`SessionId`] at the per-call boundary instead. This is what lets an
/// untrusted guest invoke tools it holds no tokens for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Principal {
    pub user_id: UserId,
    /// Plural: one credential may authorise several sessions.
    pub session_ids: Vec<SessionId>,
    pub scopes: Vec<String>,
    pub audiences: Vec<String>,
}

impl Principal {
    pub fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            ..Default::default()
        }
    }
    pub fn with_session(mut self, s: SessionId) -> Self {
        self.session_ids.push(s);
        self
    }
    pub fn with_scope(mut self, s: impl Into<String>) -> Self {
        self.scopes.push(s.into());
        self
    }
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
    pub fn authorizes_session(&self, session_id: &SessionId) -> bool {
        self.session_ids.iter().any(|s| s == session_id)
    }
}

// ─────────────────────────── methods ───────────────────────────

macro_rules! methods {
    ($($(#[$m:meta])* $variant:ident => $wire:literal),* $(,)?) => {
        /// Every JSON-RPC method on the wire. Direction is enforced by the hub,
        /// not by this enum.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Method {
            $($(#[$m])* #[serde(rename = $wire)] $variant),*
        }

        impl Method {
            pub const ALL: &'static [Method] = &[$(Self::$variant),*];

            pub const fn as_wire_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),* }
            }

            pub fn from_wire_str(s: &str) -> Option<Self> {
                match s { $($wire => Some(Self::$variant),)* _ => None }
            }
        }
    };
}

methods! {
    // ── harness → hub ──
    SessionOpen => "session_open",
    SessionClose => "session_close",
    SessionBindServer => "session_bind_server",
    SessionUnbindServer => "session_unbind_server",
    /// Attach to an EXISTING session as an observer. Answered hub-locally from
    /// routing the owner already established; never forwarded to the server.
    SessionAttachServer => "session_attach_server",
    ToolsList => "tools.list",
    ToolsSearch => "tools.search",
    ToolCall => "tool.call",
    /// Sugar: translated to a `hook` frame carrying a cancel event. There is no
    /// distinct `tool.cancel` frame on the wire.
    ToolCancel => "tool.cancel",
    ToolNotify => "tool.notify",
    SystemNotify => "system.notify",
    SubscribeNotifications => "subscribe_notifications",
    UnsubscribeNotifications => "unsubscribe_notifications",
    Hook => "hook",
    Hello => "hello",
    HelloAck => "hello_ack",
    Ping => "ping",
    Pong => "pong",
    ServersList => "servers.list",
    /// Claim exclusive control of a computer for a person at a keyboard.
    ///
    /// A openbot extension, not part of the studied protocol. While held, every
    /// tool call routed to that tool server from any other session is refused.
    /// See `codes::TAKEN_OVER`.
    ComputerTakeover => "computer.takeover",
    /// Give the computer back. Also happens automatically when the holding
    /// connection drops.
    ComputerRelease => "computer.release",

    // ── tool server → hub ──
    ToolCallProgress => "tool_call_progress",
    ToolNotification => "tool.notification",
    HookReply => "hook_reply",
    /// Fire-and-forget telemetry donation. No `id`, no response.
    TracesDonate => "traces.donate",
    LogsDonate => "logs.donate",
    MetricsDonate => "metrics.donate",

    // ── hub → harness (approvals) ──
    /// The hub asks the harness for a decision before running a call the
    /// policy flagged. Enforcement is the hub's; the harness only answers.
    ApprovalRequest => "approval.request",
    /// Hub → harness: a Bot needs a credential the account does not hold.
    SecretRequest => "secret.request",

    // ── hub → tool server ──
    ToolCallRequest => "tool_call_request",
    /// Hub asks the server to start serving a session; server replies with its
    /// tool snapshot.
    SessionBind => "session.bind",
    /// Notification; no response expected.
    SessionUnbind => "session.unbind",

    // ── hub → harness ──
    ToolsChanged => "tools_changed",
    SubscribeAck => "subscribe_ack",
    UnsubscribeAck => "unsubscribe_ack",

    // ── tool server lifecycle ──
    /// Full, idempotent tool snapshot. Re-sending replaces the set; the hub
    /// diffs it and emits `tools_changed`.
    Serve => "serve",
    ToolServerStatus => "tool_server.status",
    ToolServerGetStatus => "tool_server.get_status",
    ToolServerEvict => "tool_server.evict",
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

// ─────────────────────────── JSON-RPC envelope ───────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Num(i64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub id: RpcId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Present on session-scoped calls; absent on connection-scoped ones and on
    /// `metrics.donate` (metrics are process-aggregate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// A notification is a request with no `id`; no response is produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Result(Value),
    Error(RpcError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

fn jsonrpc_version() -> String {
    "2.0".to_owned()
}

/// Anything that can arrive on the socket. `Response` is tried first because a
/// response and a request are distinguished only by which fields are present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Frame {
    Response(Response),
    Request(Request),
    Notification(Notification),
}

impl Frame {
    pub fn decode(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
    pub fn encode(&self) -> String {
        // Frames are plain data; serialisation cannot fail in practice.
        serde_json::to_string(self)
            .unwrap_or_else(|e| unreachable!("frame serialisation must not fail: {e}"))
    }
}

impl Notification {
    pub fn new(method: Method, params: impl Serialize) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            method: method.as_wire_str().to_owned(),
            params: serde_json::to_value(params).ok(),
            session_id: None,
        }
    }
    pub fn in_session(mut self, s: SessionId) -> Self {
        self.session_id = Some(s);
        self
    }
    pub fn parsed_method(&self) -> Option<Method> {
        Method::from_wire_str(&self.method)
    }
}

impl Response {
    pub fn ok(id: RpcId, result: impl Serialize) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            outcome: Outcome::Result(serde_json::to_value(result).unwrap_or(Value::Null)),
        }
    }
    pub fn err(id: RpcId, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            outcome: Outcome::Error(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Standard JSON-RPC 2.0 codes, plus our reserved application range.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    /// The named session does not exist, or this principal may not act on it.
    pub const SESSION_NOT_FOUND: i32 = -32002;
    /// No tool server is bound to the session.
    pub const NO_SERVER_BOUND: i32 = -32003;
    /// The principal lacks the scope required for this call.
    pub const FORBIDDEN: i32 = -32004;
    /// A tool executed and failed. Distinct from a transport or routing fault.
    pub const TOOL_FAILED: i32 = -32010;
    /// The call was refused by policy, denied by the approver, or not answered
    /// in time. A terminal outcome alongside ok and error: the tool never ran.
    pub const APPROVAL_DENIED: i32 = -32005;
    /// A person has taken over this computer; the call was not run.
    ///
    /// Intentionally distinct from [`FORBIDDEN`]: that means "you may never do
    /// this", while this means "not right now". An agent that receives it can
    /// report the takeover and wait, rather than concluding it lacks permission
    /// and abandoning the task.
    pub const TAKEN_OVER: i32 = -32006;
}

impl Request {
    pub fn new(id: RpcId, method: Method, params: Option<Value>) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            method: method.as_wire_str().to_owned(),
            params,
            session_id: None,
        }
    }
    pub fn in_session(mut self, s: SessionId) -> Self {
        self.session_id = Some(s);
        self
    }
    /// `None` when the peer sent a method this build does not know.
    pub fn parsed_method(&self) -> Option<Method> {
        Method::from_wire_str(&self.method)
    }
}

// ─────────────────────────── tool call ───────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub tool_id: ToolId,
    pub call_id: ToolCallId,
    #[serde(default)]
    pub args: Value,
}

/// One item of a tool's output stream.
///
/// Invariant: zero or more [`Self::Progress`] followed by exactly one terminal
/// ([`Self::Ok`] or [`Self::Err`]).
///
/// The hub does not use this type: it relays `tool_call_progress`
/// notifications and the terminal response as separate frames, and enforces
/// the invariant by construction (progress is retired from the routing table
/// before the terminal is forwarded). The type is offered to clients that
/// prefer to model the stream as one typed sequence rather than reassemble it
/// from frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolStreamItem {
    Progress {
        call_id: ToolCallId,
        payload: Value,
    },
    Ok {
        call_id: ToolCallId,
        output: Value,
    },
    Err {
        call_id: ToolCallId,
        error: RpcError,
    },
}

impl ToolStreamItem {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Progress { .. })
    }
}

/// How the registered tool set is presented to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolDefinitionMode {
    /// Every tool description goes to the model.
    Full,
    /// Only a meta-tool pair is sent; the rest is reachable through search.
    /// This is how a large tool set stays affordable.
    Concise {
        meta_search: ToolId,
        meta_call: ToolId,
    },
}

// ─────────────── workspace unavailable ───────────────

/// JSON-RPC code reserved for a workspace that cannot be reached. Distinct from
/// a tool that merely failed: it drives the recover / update / reset UI.
pub const WORKSPACE_UNAVAILABLE_CODE: i32 = -32001;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceGoneReason {
    IdleTimeout,
    Disconnect,
    Shutdown,
    /// No owner has bound a tool server for the connected session yet: an attach-time
    /// miss, as opposed to a workspace that was bound and then lost.
    NotBound,
    /// The target hub's liveness key is absent.
    InstanceGone,
    #[serde(other)]
    Unknown,
}

/// When, relative to the failing call, the loss was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceGonePhase {
    InFlightCancelled,
    RouteMissing,
    Attach,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceUnavailable {
    pub reason: WorkspaceGoneReason,
    pub phase: WorkspaceGonePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WorkspaceUnavailable {
    pub fn to_rpc_error(&self) -> RpcError {
        RpcError {
            code: WORKSPACE_UNAVAILABLE_CODE,
            message: "workspace unavailable".to_owned(),
            data: serde_json::to_value(self).ok(),
        }
    }
}

// ─────────────────────────── limits ───────────────────────────

pub mod limits {
    pub const MAX_DONATION_BYTES: usize = 1024 * 1024;
    pub const MAX_SPANS_PER_DONATION: usize = 512;
    pub const MAX_LOG_RECORDS_PER_DONATION: usize = 512;
    pub const MAX_METRICS_PER_DONATION: usize = 512;
    pub const MAX_SYSTEM_NOTIFY_PAYLOAD_BYTES: usize = 256 * 1024;
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_round_trips_through_its_wire_string() {
        for m in Method::ALL {
            assert_eq!(Method::from_wire_str(m.as_wire_str()), Some(*m), "{m}");
        }
    }

    /// Pins every wire string to a literal.
    ///
    /// `every_method_round_trips_through_its_wire_string` cannot detect a
    /// rename: `as_wire_str` and `from_wire_str` are generated from the same
    /// table, so renaming `session_open` to `session.open` keeps the round trip
    /// intact while breaking every peer that is not this build. Wire
    /// compatibility with a protocol defined elsewhere is the purpose of this
    /// crate, so the strings are compared against literals here.
    ///
    /// The mixed conventions (`tool.call`, `session_open`, `tool_server.status`)
    /// are inherited from the upstream protocol and must not be normalised.
    ///
    /// Quantified over `Method::ALL` in both directions, so adding a method
    /// fails this test until its wire string is pinned.
    #[test]
    fn every_method_wire_string_is_pinned() {
        const WIRE: &[(Method, &str)] = &[
            (Method::SessionOpen, "session_open"),
            (Method::SessionClose, "session_close"),
            (Method::SessionBindServer, "session_bind_server"),
            (Method::SessionUnbindServer, "session_unbind_server"),
            (Method::SessionAttachServer, "session_attach_server"),
            (Method::ToolsList, "tools.list"),
            (Method::ToolsSearch, "tools.search"),
            (Method::ToolCall, "tool.call"),
            (Method::ToolCancel, "tool.cancel"),
            (Method::ToolNotify, "tool.notify"),
            (Method::SystemNotify, "system.notify"),
            (Method::SubscribeNotifications, "subscribe_notifications"),
            (
                Method::UnsubscribeNotifications,
                "unsubscribe_notifications",
            ),
            (Method::Hook, "hook"),
            (Method::Hello, "hello"),
            (Method::HelloAck, "hello_ack"),
            (Method::Ping, "ping"),
            (Method::Pong, "pong"),
            (Method::ServersList, "servers.list"),
            (Method::ComputerTakeover, "computer.takeover"),
            (Method::ComputerRelease, "computer.release"),
            (Method::ToolCallProgress, "tool_call_progress"),
            (Method::ToolNotification, "tool.notification"),
            (Method::HookReply, "hook_reply"),
            (Method::TracesDonate, "traces.donate"),
            (Method::LogsDonate, "logs.donate"),
            (Method::MetricsDonate, "metrics.donate"),
            (Method::ApprovalRequest, "approval.request"),
            (Method::SecretRequest, "secret.request"),
            (Method::ToolCallRequest, "tool_call_request"),
            (Method::SessionBind, "session.bind"),
            (Method::SessionUnbind, "session.unbind"),
            (Method::ToolsChanged, "tools_changed"),
            (Method::SubscribeAck, "subscribe_ack"),
            (Method::UnsubscribeAck, "unsubscribe_ack"),
            (Method::Serve, "serve"),
            (Method::ToolServerStatus, "tool_server.status"),
            (Method::ToolServerGetStatus, "tool_server.get_status"),
            (Method::ToolServerEvict, "tool_server.evict"),
        ];

        assert_eq!(
            WIRE.len(),
            Method::ALL.len(),
            "a method was added or removed without pinning what it is called on the wire"
        );
        for (m, expected) in WIRE {
            assert_eq!(m.as_wire_str(), *expected, "{m:?}");
            // Serde produces the bytes on the wire; `as_wire_str` is what the
            // code reads. Both are pinned because either can drift alone.
            assert_eq!(
                serde_json::to_value(m).unwrap().as_str(),
                Some(*expected),
                "serde and as_wire_str disagree for {m:?}"
            );
        }
        for m in Method::ALL {
            assert!(
                WIRE.iter().any(|(w, _)| w == m),
                "{m:?} has no pinned wire string"
            );
        }
    }

    #[test]
    fn unknown_methods_do_not_parse() {
        assert_eq!(Method::from_wire_str("not_a_method"), None);
        assert_eq!(Method::from_wire_str(""), None);
    }

    #[test]
    fn method_serde_matches_wire_string() {
        let j = serde_json::to_value(Method::ToolCall).unwrap();
        assert_eq!(j.as_str(), Some("tool.call"));
    }

    #[test]
    fn harness_hello_omits_server_fields() {
        let j = serde_json::to_value(Hello::harness()).unwrap();
        assert_eq!(j["kind"], "harness");
        assert_eq!(j["protocol_version"], PROTOCOL_VERSION);
        assert!(
            j.get("server_id").is_none(),
            "server_id must be omitted for a harness"
        );
    }

    #[test]
    fn tool_server_hello_carries_server_id() {
        let h = Hello::tool_server("workspace-server").with_description("guest workspace");
        let j = serde_json::to_value(&h).unwrap();
        assert_eq!(j["kind"], "tool_server");
        assert_eq!(j["server_id"], "workspace-server");
        assert_eq!(j["description"], "guest workspace");
    }

    #[test]
    fn unknown_workspace_gone_variants_degrade_instead_of_failing() {
        // A newer peer may add variants; older peers must not hard-fail.
        let r: WorkspaceGoneReason = serde_json::from_str("\"some_future_reason\"").unwrap();
        assert_eq!(r, WorkspaceGoneReason::Unknown);
        let p: WorkspaceGonePhase = serde_json::from_str("\"whatever\"").unwrap();
        assert_eq!(p, WorkspaceGonePhase::Unknown);
    }

    #[test]
    fn stream_terminality() {
        let id = ToolCallId::new("c1");
        assert!(!ToolStreamItem::Progress {
            call_id: id.clone(),
            payload: Value::Null
        }
        .is_terminal());
        assert!(ToolStreamItem::Ok {
            call_id: id.clone(),
            output: Value::Null
        }
        .is_terminal());
        assert!(ToolStreamItem::Err {
            call_id: id,
            error: RpcError {
                code: -1,
                message: "x".into(),
                data: None
            }
        }
        .is_terminal());
    }

    #[test]
    fn principal_gates_on_scope_and_session() {
        let p = Principal::new(UserId::new("u1"))
            .with_session(SessionId::new("s1"))
            .with_scope(SCOPE_TOOL_INVOKE);
        assert!(p.has_scope(SCOPE_TOOL_INVOKE));
        assert!(!p.has_scope("admin"));
        assert!(p.authorizes_session(&SessionId::new("s1")));
        assert!(!p.authorizes_session(&SessionId::new("s2")));
    }

    #[test]
    fn request_carries_session_and_parses_method() {
        let r =
            Request::new(RpcId::Num(1), Method::ToolCall, None).in_session(SessionId::new("s1"));
        assert_eq!(r.parsed_method(), Some(Method::ToolCall));
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["jsonrpc"], "2.0");
        assert_eq!(j["method"], "tool.call");
        assert_eq!(j["session_id"], "s1");
    }

    #[test]
    fn workspace_unavailable_maps_to_reserved_code() {
        let e = WorkspaceUnavailable {
            reason: WorkspaceGoneReason::IdleTimeout,
            phase: WorkspaceGonePhase::RouteMissing,
            detail: None,
        }
        .to_rpc_error();
        assert_eq!(e.code, WORKSPACE_UNAVAILABLE_CODE);
        assert_eq!(e.data.unwrap()["reason"], "idle_timeout");
    }
}

/// The environment variable a hub's children are told its token through.
///
/// `up` sets it on the guest it spawns, so a guest — which knows a URL and has
/// never known a home — needs no new argument and no new dependency. See
/// [`Hello::token`] for what the token defends.
pub const HUB_TOKEN_ENV: &str = "OPENBOT_HUB_TOKEN";

/// The token file's name inside a home.
pub const HUB_TOKEN_FILE: &str = "hub.token";

/// The token to present, from the environment or from the default home.
///
/// The environment first, because that is how a hub tells the children it
/// spawned. The file second, because a person in a second terminal has neither
/// the variable nor any reason to know about it — they have a home, and the
/// hub they are talking to wrote its token there.
///
/// `$OPENBOT_HOME` is consulted before the default, because that is the
/// variable the whole command line already means by "which home". Reading the
/// default home while the person's commands all address another one would look
/// exactly like a hub refusing a correct token.
#[must_use]
pub fn hub_token() -> Option<String> {
    if let Some(v) = std::env::var_os(HUB_TOKEN_ENV) {
        let v = v.to_string_lossy().trim().to_owned();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let home = std::env::var_os("OPENBOT_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(default_home, std::path::PathBuf::from);
    hub_token_in(&home)
}

/// The token written in a particular home, if there is one.
#[must_use]
pub fn hub_token_in(home: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join(HUB_TOKEN_FILE)).ok()?;
    let token = raw.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// Write a hub's token into its home, readable only by this user.
///
/// `0600` on unix. Windows has no chmod and its default ACL on a file under
/// the user profile already excludes other users; this does not pretend
/// otherwise, and the token is not what keeps a program running *as* this user
/// out — nothing here does.
///
/// # Errors
/// If the home cannot be created or the file cannot be written.
pub fn write_hub_token(home: &std::path::Path, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let path = home.join(HUB_TOKEN_FILE);
    std::fs::write(&path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Where a person's Bots, secrets and connectors live by default.
///
/// One definition, because two would drift and did: the command line
/// defaulted to `./openbot-data` and the desktop window to `~/.openbot`, so
/// starting a computer in a terminal and opening the window pointed each at a
/// different home. Nothing failed loudly. The window connected to a hub that
/// was serving Bots it could not see, which reads as an empty install.
///
/// Absolute, not relative. A home is long-lived and a relative one moves with
/// whatever directory a person happened to be in, so `openbot bot ls` in two
/// terminals would list two different sets of Bots. `~/.openbot` follows the
/// same convention as the other per-user tool directories.
#[must_use]
pub fn default_home() -> std::path::PathBuf {
    // `USERPROFILE` on Windows and `HOME` everywhere else: what a shell means
    // by `~`.
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    home_under(std::env::var_os(key).as_deref())
}

/// The default home for a given value of the home-directory variable.
///
/// Split from [`default_home`] so it can be tested without setting an
/// environment variable, which is `unsafe` in this edition and racy across
/// threads besides.
///
/// An empty or absent value falls back to `openbot-data` in the working
/// directory: joining onto an empty string yields a bare relative `.openbot`
/// that looks like a real answer and is not one. A machine with no home
/// directory is close to impossible, and this is still better than a tilde
/// nothing expands.
fn home_under(raw: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    raw.filter(|v| !v.is_empty()).map_or_else(
        || std::path::PathBuf::from("openbot-data"),
        |h| std::path::Path::new(h).join(".openbot"),
    )
}

#[cfg(test)]
mod home_tests {
    use super::*;
    use std::ffi::OsStr;

    /// The default is absolute, which is the whole point: a relative home
    /// moves with the shell, so `openbot bot ls` in two directories would list
    /// two different sets of Bots.
    #[test]
    fn the_default_home_does_not_move_with_the_working_directory() {
        let under = home_under(Some(OsStr::new(if cfg!(windows) {
            r"C:\Users\someone"
        } else {
            "/home/someone"
        })));
        assert!(
            under.is_absolute(),
            "a relative default home moves with the shell: {}",
            under.display()
        );
        assert!(under.ends_with(".openbot"), "{}", under.display());
    }

    #[test]
    fn an_empty_home_variable_is_no_home_at_all() {
        assert_eq!(
            home_under(Some(OsStr::new(""))),
            std::path::PathBuf::from("openbot-data")
        );
        assert_eq!(home_under(None), std::path::PathBuf::from("openbot-data"));
    }
}
