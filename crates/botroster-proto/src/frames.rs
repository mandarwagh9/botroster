//! Method payloads: the `params` and `result` bodies for each wire method.
//!
//! Shapes track `xai-tool-protocol::frames` (Apache-2.0) so an unmodified
//! upstream harness can talk to `botrosterd`. See `../../../PROVENANCE.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ServerId, SessionId, ToolCallId, ToolId};

/// A tool as advertised to the hub and, through it, to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescription {
    pub tool_id: ToolId,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    #[serde(default)]
    pub input_schema: Value,
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

/// Hub asks a server to start serving a session. The session id travels in the
/// request envelope, so the body is empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindParams {}

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

/// Hub → tool server. Same body as the harness's `tool.call`; the hub rewrites
/// the JSON-RPC id so it can correlate the reply back to the originating
/// harness request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequestParams {
    pub tool_id: ToolId,
    pub call_id: ToolCallId,
    #[serde(default)]
    pub args: Value,
}

/// Terminal payload of a successful call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub output: Value,
}

/// Streamed progress. A notification: zero or more per call, always before
/// the terminal response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallProgressFrame {
    pub call_id: ToolCallId,
    pub payload: Value,
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
        assert_eq!(j["tools"][0]["tool_id"], "fs.read");
        let back: ServeParams = serde_json::from_value(j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn empty_bodies_serialise_as_objects_not_null() {
        // A tool server that expects `{}` must not receive `null`.
        assert_eq!(
            serde_json::to_value(SessionBindParams {}).unwrap(),
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
