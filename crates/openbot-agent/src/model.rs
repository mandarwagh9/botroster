//! The model boundary.
//!
//! Everything above this boundary is provider-agnostic. The conversation is
//! modelled as content blocks, not provider-shaped JSON, because that is the
//! one representation both major tool-calling protocols map onto cleanly:
//!
//! | openbot | Anthropic Messages | OpenAI chat completions |
//! |---|---|---|
//! | `Content::Text` | `{"type":"text"}` | `message.content` |
//! | `Content::ToolUse` | `{"type":"tool_use"}` | `tool_calls[]` |
//! | `Content::ToolResult` | `{"type":"tool_result"}` | `role:"tool"` message |
//!
//! Adapting a provider is then a pure translation at the edge, and the agent
//! loop never learns which one it is talking to.

use std::fmt;

use openbot_proto::frames::ToolDescription;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Identifies one tool invocation across the request, the result, and the
/// transcript. Provider-supplied when the provider supplies one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolUseId(pub String);

impl ToolUseId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolUseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    ToolUse {
        id: ToolUseId,
        name: String,
        input: Value,
    },
    ToolResult {
        id: ToolUseId,
        /// Rendered result. Kept as a string because that is what every
        /// provider ultimately wants; structure lives in the transcript event.
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

impl Content {
    pub fn text(t: impl Into<String>) -> Self {
        Self::Text { text: t.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Content::text(text)],
        }
    }
    pub fn assistant(content: Vec<Content>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// Tool uses requested by this message, in order.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&ToolUseId, &str, &Value)> {
        self.content.iter().filter_map(|c| match c {
            Content::ToolUse { id, name, input } => Some((id, name.as_str(), input)),
            _ => None,
        })
    }

    /// Concatenated text blocks, which is what a transcript renders.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Why the model stopped producing this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer. The loop ends.
    EndTurn,
    /// The model wants tools run. The loop continues.
    ToolUse,
    /// The provider truncated the turn.
    MaxTokens,
    /// The model declined to answer. Anthropic calls this `refusal`, the
    /// OpenAI-compatible dialects `content_filter`.
    ///
    /// Kept distinct from `EndTurn`: a parser that sweeps it into a `_` arm
    /// reports a refused turn as an ordinary, successful finish, when the
    /// provider has said outright that nothing the task asked for happened.
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescription>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnResponse {
    pub content: Vec<Content>,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("provider rejected the request: {0}")]
    Rejected(String),
    /// The provider was busy, rate limited, or briefly broken.
    ///
    /// Separate from [`Self::Rejected`] because the difference decides whether
    /// anything should try again. Its own variant rather than a property read
    /// back out of the message: the status code is known exactly where the
    /// error is built, and sniffing a string for "429" later is unreliable.
    #[error("provider is overloaded: {0}")]
    Overloaded(String),
    #[error("provider returned something unusable: {0}")]
    Malformed(String),
    #[error("the script ran out of turns; the agent asked for more than it was given")]
    ScriptExhausted,
}

/// A conversational model that can request tool calls.
///
/// Intentionally a single method. A provider is a translation layer, not a
/// place for behaviour; anything clever belongs in the agent loop, where it is
/// testable against the scripted provider.
#[async_trait::async_trait]
pub trait Model: Send + Sync {
    async fn turn(&self, req: &TurnRequest) -> Result<TurnResponse, ModelError>;

    /// Human-readable identity, for transcripts and logs.
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_uses_are_extracted_in_order() {
        let m = Message::assistant(vec![
            Content::text("working on it"),
            Content::ToolUse {
                id: ToolUseId::new("a"),
                name: "fs.read".into(),
                input: serde_json::json!({"path": "x"}),
            },
            Content::ToolUse {
                id: ToolUseId::new("b"),
                name: "fs.write".into(),
                input: serde_json::json!({}),
            },
        ]);
        let names: Vec<_> = m.tool_uses().map(|(_, n, _)| n).collect();
        assert_eq!(names, vec!["fs.read", "fs.write"]);
        assert_eq!(m.text(), "working on it");
    }

    #[test]
    fn content_blocks_round_trip_with_a_stable_tag() {
        let c = Content::ToolResult {
            id: ToolUseId::new("t1"),
            content: "ok".into(),
            is_error: false,
        };
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(j["type"], "tool_result");
        assert_eq!(serde_json::from_value::<Content>(j).unwrap(), c);
    }

    #[test]
    fn a_message_with_no_text_renders_empty_not_garbage() {
        let m = Message::assistant(vec![Content::ToolUse {
            id: ToolUseId::new("a"),
            name: "x".into(),
            input: Value::Null,
        }]);
        assert_eq!(m.text(), "");
    }
}
