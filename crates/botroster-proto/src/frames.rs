//! Method payloads: the `params` and `result` bodies for each wire method.
//!
//! These shapes track `xai-tool-protocol::frames` (Apache-2.0) closely enough
//! that the difference is worth stating precisely: **they are not the same, and
//! an unmodified upstream harness cannot talk to `botrosterd`.** A tool call
//! here is `{tool_id, call_id, args}`; upstream's is
//! `{tool_id, tool_call_id, arguments}`. Field renames are the whole of the
//! difference in this file — nothing here is a different concept wearing a
//! similar name.
//!
//! See `../../../PROVENANCE.md` §1 for the full divergence table, which
//! `../tests/divergence.rs` keeps honest against these types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ServerId, SessionId, ToolCallId, ToolId};

/// A tool as advertised to the hub and, through it, to the model.
///
/// The wire names follow the published protocol (`xai-tool-types/src/types.rs`),
/// because a client that cannot parse a description cannot use any of it:
/// `name` is not optional there, so a snapshot carrying `tool_id` fails with
/// `missing field 'name'` before a single tool is looked at. The Rust field
/// names are unchanged, so every call site in this workspace still says
/// `tool_id` and `input_schema` while the bytes say what a published client
/// expects. The old spellings are kept as read aliases so a stored snapshot or
/// a fixture written before the rename still parses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescription {
    /// Serialised as `name`.
    #[serde(rename = "name", alias = "tool_id")]
    pub tool_id: ToolId,
    pub description: String,
    /// Serialised as `arguments_schema`. JSON Schema for the tool's arguments.
    #[serde(default, rename = "arguments_schema", alias = "input_schema")]
    pub input_schema: Value,
    /// Optional grouping, for a client that renders tools by namespace. Upstream
    /// declares it optional; this hub never sets it, but a snapshot carrying one
    /// must round-trip rather than be silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Display name. A client may derive one from `name` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// High-level tool kind, stable snake_case, for grouping. Never set here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl ToolDescription {
    pub fn new(
        tool_id: impl Into<ToolId>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            tool_id: tool_id.into(),
            description: description.into(),
            input_schema,
            namespace: None,
            title: None,
            kind: None,
        }
    }
}

// ── session lifecycle: harness → hub ──

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionOpenParams {
    /// Optional client-supplied id. The hub mints one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Which Bot the connected session acts as.
    ///
    /// Attribution, not authorisation: it decides who a handoff is from.
    /// A hosted deployment would bind this server-side at session creation
    /// rather than taking the client's word for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    /// Serve this session's tool calls from a past session's record instead of
    /// running them.
    ///
    /// Attribution, not authorisation, like `bot` above: the client says what
    /// it wants and the hub decides what that means. What it means here is
    /// restrictive rather than permissive — a replaying session can do *less*,
    /// not more, because the hub stops forwarding anything to the computer.
    ///
    /// `None` is an ordinary session, which is every session that existed
    /// before this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
}

/// Which past session to answer from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    /// The Bot whose record it is. A record lives beside the Bot it belongs to,
    /// and a session id is only unique within one.
    pub bot: String,
    /// The recorded session.
    pub session: SessionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOpenResult {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCloseParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBindServerParams {
    pub server_id: ServerId,
}

/// Reply to `session_bind_server`: the tool snapshot the server returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindServerResult {
    pub tools: Vec<ToolDescription>,
}

// ── session lifecycle: hub → tool server ──

/// Hub asks a server to start serving a session.
///
/// The session id travels in `params` and **not** on the request envelope, which
/// is the opposite of every other request this hub sends. The reason is upstream's
/// frame router: `xai-computer-hub-sdk/src/demux.rs:387` sends any frame carrying
/// an envelope `session_id` to a per-session inbox, and only a frame without one
/// reaches the notification channel that is the sole thing answering a bind
/// (`server.rs:1583-1649`). Carrying it in both places looks maximally compatible
/// and is exactly why a bind gets dropped — silently, with no error and no reply,
/// so the hub waits out its bind timeout and reports a tool server that "did not
/// answer", a diagnosis pointing at the peer rather than at the disagreement.
///
/// Upstream's own test pins the shape (`connection_tests.rs:2358-2364`):
/// `{"id":"b1","method":"session.bind","params":{"session_id":"s1"}}` must route as
/// a Notification.
///
/// The mirror image of `session_open`, whose published params have no session
/// field at all. Two session-carrying frames, two conventions, one protocol.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindResult {
    pub tools: Vec<ToolDescription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<String>,
}

/// Notification; no response expected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionUnbindParams {}

// ── tool discovery ──

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolsListParams {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDescription>,
}

// ── serve: full idempotent snapshot, server → hub ──

/// Re-sending replaces the whole tool set. The hub diffs it and emits
/// `tools_changed`; the diff therefore lives in exactly one place.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServeParams {
    pub tools: Vec<ToolDescription>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServeResult {
    #[serde(default)]
    pub accepted: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ToolId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ToolId>,
}

/// Hub → harness notification after a snapshot changes the tool set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolsChanged {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ToolId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ToolId>,
}

// ── invocation ──

/// The `kind` a progress frame carries when the producer has nothing more
/// specific to say.
///
/// `kind` is a producer-defined free string upstream and a client groups by it
/// rather than reading it. This hub relays a tool's output stream verbatim, so
/// the honest description is a chunk of that stream, and `log_chunk` is one of
/// the two values upstream names in the field's own documentation. Naming it
/// once here keeps the guest and its tests from drifting apart on a value only
/// a consumer would ever compare.
pub const PROGRESS_KIND_LOG_CHUNK: &str = "log_chunk";

/// Hub → tool server. Same body as the harness's `tool.call`; the hub rewrites
/// the JSON-RPC id so it can correlate the reply back to the originating
/// harness request.
///
/// `call_id` and `args` are serialised as `tool_call_id` and `arguments`. The
/// Rust names are unchanged, so nothing at a call site in this workspace moves;
/// only the bytes do. A tool server reads these with a deserialiser that rejects
/// a missing field — an interop harness refused the whole call as
/// `invalid params: missing field 'call_id'`, which is a server that is alive,
/// connected, and declining the work over a spelling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequestParams {
    pub tool_id: ToolId,
    #[serde(rename = "tool_call_id", alias = "call_id")]
    pub call_id: ToolCallId,
    #[serde(default, rename = "arguments", alias = "args")]
    pub args: Value,
}

/// Terminal payload of a successful call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    #[serde(rename = "tool_call_id", alias = "call_id")]
    pub call_id: ToolCallId,
    pub output: Value,
}

/// Streamed progress. A notification: zero or more per call, always before
/// the terminal response.
///
/// `payload` is serialised as `body`, and upstream also requires a `kind` on
/// every progress frame — not optional there, unlike `body` — so this type
/// carries one too, defaulting on read so a frame written before this hub had a
/// `kind` still parses. Upstream additionally has an optional `dropped_count`
/// for rate-pressure bookkeeping; this hub never drops progress, so there is no
/// such field here rather than a counter that is always `None` and would imply
/// a rate limiter this codebase does not have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallProgressFrame {
    #[serde(rename = "tool_call_id", alias = "call_id")]
    pub call_id: ToolCallId,
    #[serde(default = "default_progress_kind")]
    pub kind: String,
    #[serde(rename = "body", alias = "payload")]
    pub body: Value,
}

fn default_progress_kind() -> String {
    PROGRESS_KIND_LOG_CHUNK.to_owned()
}

// ── connection keepalive ──

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PingFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PongFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

// ── server discovery ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub server_id: ServerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Required by the published protocol
    /// (`xai-tool-protocol/src/frames.rs:328-356`) and absent here until slice
    /// B1, which is why a client built against it could not parse this result.
    ///
    /// `default` on the read side only: this hub always writes it, and a
    /// `default` on write would make a missing status indistinguishable from a
    /// ready one, which is the one value a client must not have to guess.
    #[serde(default)]
    pub status: crate::ToolServerLifecycleStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServersListResult {
    pub servers: Vec<ServerInfo>,
}

/// Claim a computer for a person.
///
/// Naming a reason is required rather than optional: the agent is about to be
/// locked out of its own computer, and the operator reading the log later needs
/// to know whether that was a CAPTCHA or a payment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerTakeoverParams {
    pub server_id: ServerId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerTakeoverResult {
    pub server_id: ServerId,
    /// True if this call took the computer; false if the caller already held
    /// it. Idempotent so a viewer reconnecting does not have to track state.
    pub claimed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerReleaseParams {
    pub server_id: ServerId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerReleaseResult {
    pub server_id: ServerId,
    /// False if nobody held it. Not an error: releasing an unheld computer
    /// leaves it in the requested state.
    pub released: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_snapshot_round_trips() {
        let p = ServeParams {
            tools: vec![ToolDescription::new(
                "fs.read",
                "Read a UTF-8 file from the workspace",
                serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            )],
        };
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["tools"][0]["name"], "fs.read");
        let back: ServeParams = serde_json::from_value(j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn empty_bodies_serialise_as_objects_not_null() {
        // A tool server that expects `{}` must not receive `null`.
        assert_eq!(
            serde_json::to_value(SessionBindParams::default()).unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::to_value(ToolsListParams {}).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn bind_result_tolerates_a_server_without_binary_version() {
        let r: SessionBindResult = serde_json::from_str(r#"{"tools":[]}"#).unwrap();
        assert!(r.binary_version.is_none());
    }
}
