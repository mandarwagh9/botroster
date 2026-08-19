//! A real model over HTTP, in two dialects.
//!
//! One provider covers most of the field because nearly every vendor speaks
//! either the Anthropic Messages shape or the OpenAI chat-completions shape:
//!
//! - `anthropic`: Claude (`api.anthropic.com`)
//! - `openai`: OpenAI, xAI (`api.x.ai/v1`), Groq, Together, Ollama, vLLM,
//!   and anything else that ships an OpenAI-compatible endpoint
//!
//! Translation happens entirely at this edge. Nothing above [`crate::model`]
//! knows which dialect is in use, which is what lets the scripted provider
//! stand in for either one in tests.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::{
    Content, Model, ModelError, Role, StopReason, ToolUseId, TurnRequest, TurnResponse, Usage,
};

/// Wire dialect. Not a vendor list; several vendors share each shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    AnthropicMessages,
    OpenAiChat,
}

impl std::str::FromStr for Dialect {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" | "messages" => Ok(Self::AnthropicMessages),
            "openai" | "oai" | "xai" | "grok" | "compatible" => Ok(Self::OpenAiChat),
            other => Err(format!(
                "unknown dialect `{other}`; expected `anthropic` or `openai`"
            )),
        }
    }
}

pub struct HttpModel {
    client: reqwest::Client,
    dialect: Dialect,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    label: String,
}

pub struct HttpModelConfig {
    pub dialect: Dialect,
    /// Base URL without a trailing slash, e.g. `https://api.x.ai/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl HttpModel {
    pub fn new(cfg: HttpModelConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            dialect: cfg.dialect,
            base_url: cfg.base_url.trim_end_matches('/').to_owned(),
            api_key: cfg.api_key,
            model: cfg.model.clone(),
            max_tokens: cfg.max_tokens,
            label: cfg.model,
        })
    }

    fn endpoint(&self) -> String {
        match self.dialect {
            Dialect::AnthropicMessages => format!("{}/v1/messages", self.base_url),
            Dialect::OpenAiChat => format!("{}/chat/completions", self.base_url),
        }
    }
}

#[async_trait::async_trait]
impl Model for HttpModel {
    async fn turn(&self, req: &TurnRequest) -> Result<TurnResponse, ModelError> {
        let body = match self.dialect {
            Dialect::AnthropicMessages => anthropic::request(&self.model, self.max_tokens, req),
            Dialect::OpenAiChat => openai::request(&self.model, self.max_tokens, req),
        };

        let mut rb = self.client.post(self.endpoint()).json(&body);
        rb = match self.dialect {
            Dialect::AnthropicMessages => rb
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01"),
            Dialect::OpenAiChat => rb.bearer_auth(&self.api_key),
        };

        // The dialect translations are unit-tested against hand-written JSON,
        // not against live endpoints. When a real provider rejects something,
        // the exact bodies are the only way to see why, so they are logged at
        // debug level rather than requiring a code change to inspect.
        tracing::debug!(
            endpoint = %self.endpoint(),
            body = %serde_json::to_string(&body).unwrap_or_default(),
            "model request"
        );

        let resp = rb
            .send()
            .await
            .map_err(|e| ModelError::Transport(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ModelError::Transport(e.to_string()))?;

        tracing::debug!(%status, body = %truncate(&text, 4000), "model response");

        if !status.is_success() {
            // Keep the provider's own message; it is almost always the useful
            // part (bad key, unknown model, rate limit, context overflow).
            let why = format!("HTTP {status}: {}", truncate(&text, 600));
            // Rate limits, timeouts and a provider having a bad minute all end
            // on their own; a bad key and an unknown model do not. Nothing
            // above this line can tell them apart afterwards, so the
            // distinction is made here, where the status is.
            let transient = matches!(status.as_u16(), 408 | 429) || status.is_server_error();
            return Err(if transient {
                ModelError::Overloaded(why)
            } else {
                ModelError::Rejected(why)
            });
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| ModelError::Malformed(format!("{e}: {}", truncate(&text, 300))))?;

        // A 200 carrying an error is still an error.
        if let Some(why) = provider_error(&v) {
            return Err(ModelError::Rejected(why));
        }

        let mut turn = match self.dialect {
            Dialect::AnthropicMessages => anthropic::response(&v)?,
            Dialect::OpenAiChat => openai::response(&v)?,
        };
        // Map the wire names back to the ids the hub actually routes on.
        restore_names(&mut turn, req);
        Ok(turn)
    }

    fn name(&self) -> &str {
        &self.label
    }
}

/// The provider's own error message, when a `200` carries one.
///
/// Gateways in front of these APIs answer `200` with `{"error": …}` instead of
/// an HTTP status, and OpenAI-compatible proxies do it routinely; `--base-url`
/// exists so people can point openbot at one. The status check cannot see that,
/// and without this step the body reaches the dialect parser, which finds no
/// success keys and reports "response has no choices", a message that
/// describes the parser being confused rather than the expired key it
/// actually was.
///
/// Requiring the success keys to be absent is what makes this safe. A real
/// response that happens to carry an `error` field somewhere still parses as
/// the response it is, so nothing valid can be turned into a failure here.
fn provider_error(v: &Value) -> Option<String> {
    if v.get("content").is_some() || v.get("choices").is_some() {
        return None;
    }
    let e = v.get("error").filter(|e| !e.is_null())?;

    let message = e
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| e.as_str())
        .map(str::to_owned)
        // An error object of unknown shape is handed over whole; guessing at
        // it would lose the only evidence.
        .unwrap_or_else(|| truncate(&e.to_string(), 600));

    match e.get("type").and_then(Value::as_str) {
        Some(kind) => Some(format!("{kind}: {message}")),
        None => Some(message),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    s.chars().take(n).collect::<String>() + "…"
}

// ── Anthropic Messages ────────────────────────────────────────────────

mod anthropic {
    use super::*;

    pub fn request(model: &str, max_tokens: u32, req: &TurnRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content.iter().map(block).collect::<Vec<_>>(),
                })
            })
            .collect();

        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": sanitize(t.tool_id.as_str()),
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "system": req.system,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        body
    }

    fn block(c: &Content) -> Value {
        match c {
            Content::Text { text } => json!({ "type": "text", "text": text }),
            Content::ToolUse { id, name, input } => json!({
                "type": "tool_use", "id": id.as_str(), "name": sanitize(name), "input": input,
            }),
            Content::ToolResult {
                id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": id.as_str(),
                "content": content,
                "is_error": is_error,
            }),
        }
    }

    pub fn response(v: &Value) -> Result<TurnResponse, ModelError> {
        let blocks = v["content"]
            .as_array()
            .ok_or_else(|| ModelError::Malformed("response has no content array".into()))?;

        let mut content = Vec::new();
        for b in blocks {
            match b["type"].as_str() {
                Some("text") => content.push(Content::text(
                    b["text"].as_str().unwrap_or_default().to_owned(),
                )),
                Some("tool_use") => content.push(Content::ToolUse {
                    id: ToolUseId::new(b["id"].as_str().unwrap_or_default()),
                    // Raw wire name; `restore_names` resolves it against the
                    // catalogue, which is the only place that can do it
                    // unambiguously.
                    name: b["name"].as_str().unwrap_or_default().to_owned(),
                    input: b["input"].clone(),
                }),
                // Thinking blocks and anything newer are ignored.
                _ => {}
            }
        }

        let stop_reason = match v["stop_reason"].as_str() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("refusal") => StopReason::Declined,
            // `end_turn`, `stop_sequence`, and anything newer. A wildcard on a
            // wire vocabulary is unavoidable; what it must not do is swallow a
            // named outcome such as `refusal`, which is why that arm is listed
            // explicitly above.
            _ => StopReason::EndTurn,
        };

        Ok(TurnResponse {
            content,
            stop_reason,
            usage: reported_usage(v, "input_tokens", "output_tokens"),
        })
    }
}

/// Usage as the provider reported it, or `None` when it reported none.
///
/// The distinction is the basis of the token budget. `Some(Usage { 0, 0 })`
/// and `None` mean opposite things to the agent loop: the first is "this turn
/// cost nothing", which never accumulates and never trips the cap, and the
/// second is "this provider cannot be metered", which raises a warning saying
/// the budget cannot be enforced.
///
/// Do not collapse a missing field into `0` inside an unconditional `Some`:
/// that turns an unmetered provider into a free one, so a budget set against
/// it is silently not a cap, and the warning for exactly that case can never
/// fire because it tests for `None`. Pointing `base_url` and `dialect` at a
/// local OpenAI-compatible server or a gateway is a supported setup, and those
/// are precisely the providers that leave usage out or send it empty.
///
/// A counter the provider did report as `0` still counts as reported:
/// `as_u64()` answers `Some(0)` there, and "it cost nothing" is not the same
/// claim as "nothing was measured".
fn reported_usage(v: &Value, input: &str, output: &str) -> Option<Usage> {
    let u = v.get("usage")?;
    let input_tokens = u.get(input).and_then(Value::as_u64);
    let output_tokens = u.get(output).and_then(Value::as_u64);
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens: input_tokens.unwrap_or(0),
        output_tokens: output_tokens.unwrap_or(0),
    })
}

// ── OpenAI chat completions ───────────────────────────────────────────

mod openai {
    use super::*;

    pub fn request(model: &str, max_tokens: u32, req: &TurnRequest) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": req.system })];

        for m in &req.messages {
            match m.role {
                Role::Assistant => {
                    let text = m.text();
                    let calls: Vec<Value> = m
                        .tool_uses()
                        .map(|(id, name, input)| {
                            json!({
                                "id": id.as_str(),
                                "type": "function",
                                "function": {
                                    "name": sanitize(name),
                                    // OpenAI wants arguments as a JSON *string*.
                                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                                }
                            })
                        })
                        .collect();
                    let mut msg = json!({ "role": "assistant" });
                    msg["content"] = if text.is_empty() {
                        Value::Null
                    } else {
                        Value::String(text)
                    };
                    if !calls.is_empty() {
                        msg["tool_calls"] = Value::Array(calls);
                    }
                    messages.push(msg);
                }
                Role::User => {
                    // Tool results are not user content here; each becomes its
                    // own `role: "tool"` message keyed by tool_call_id.
                    let mut text = String::new();
                    for c in &m.content {
                        match c {
                            Content::Text { text: t } => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                            Content::ToolResult { id, content, .. } => messages.push(json!({
                                "role": "tool",
                                "tool_call_id": id.as_str(),
                                "content": content,
                            })),
                            Content::ToolUse { .. } => {}
                        }
                    }
                    if !text.is_empty() {
                        messages.push(json!({ "role": "user", "content": text }));
                    }
                }
            }
        }

        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": sanitize(t.tool_id.as_str()),
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        body
    }

    pub fn response(v: &Value) -> Result<TurnResponse, ModelError> {
        let choice = v["choices"]
            .get(0)
            .ok_or_else(|| ModelError::Malformed("response has no choices".into()))?;
        let msg = &choice["message"];

        let mut content = Vec::new();
        if let Some(t) = msg["content"].as_str() {
            if !t.is_empty() {
                content.push(Content::text(t));
            }
        }
        if let Some(calls) = msg["tool_calls"].as_array() {
            for c in calls {
                let raw = c["function"]["arguments"].as_str().unwrap_or("{}");
                // Arguments arrive as a string; a model that emits invalid JSON
                // should get an empty object rather than crash the run.
                let input = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
                content.push(Content::ToolUse {
                    id: ToolUseId::new(c["id"].as_str().unwrap_or_default()),
                    name: c["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    input,
                });
            }
        }

        let stop_reason = match choice["finish_reason"].as_str() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Declined,
            _ => StopReason::EndTurn,
        };

        Ok(TurnResponse {
            content,
            stop_reason,
            usage: reported_usage(v, "prompt_tokens", "completion_tokens"),
        })
    }
}

/// Tool names must match `^[a-zA-Z0-9_-]{1,64}$` for both vendors, but openbot
/// tool ids are dotted (`fs.read`), so the dot becomes `__` on the way out.
fn sanitize(name: &str) -> String {
    name.replace('.', "__")
}

/// The naive inverse, used only for a name that was never offered.
///
/// This is not a true inverse of [`sanitize`], and cannot be. A connector tool
/// is already namespaced with a double underscore (`linear__create_issue`), so
/// this turns it into `linear.create_issue`, a tool the hub has never heard
/// of. Applied unconditionally it would make every connector unreachable from
/// a real model, while `openbot call` still worked because it never goes through
/// a provider.
fn unsanitize(name: &str) -> String {
    name.replace("__", ".")
}

/// Resolve the tool names in a response against the catalogue that was sent.
///
/// The catalogue is the authority, and the order is what makes this safe:
///
/// 1. An exact match on the wire name wins. A connector tool really is
///    called `linear__create_issue`; a model asking for that name means that
///    tool, whatever else might escape to the same string.
/// 2. Otherwise, a name that exactly one offered id escapes to.
/// 3. Otherwise [`unsanitize`], so an invented name still reads sensibly in
///    the "unknown tool" the hub will answer with.
///
/// Step 2 counts before it commits. `fs.read` and a connector named `fs`
/// offering `read` both escape to `fs__read`; a map keyed by wire name that
/// keeps whichever was inserted last would make the tool a call reaches depend
/// on catalogue order, and a model asking a remote service to read something
/// could silently read a local file instead. Ambiguity resolves to the exact
/// name, and `Connector::validate` refuses the ids that could create it in the
/// first place.
fn restore_names(turn: &mut TurnResponse, req: &TurnRequest) {
    use std::collections::HashMap;

    let mut exact: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut escaped: HashMap<String, Vec<&str>> = HashMap::new();
    for t in &req.tools {
        let id = t.tool_id.as_str();
        exact.insert(id);
        escaped.entry(sanitize(id)).or_default().push(id);
    }

    for c in &mut turn.content {
        if let Content::ToolUse { name, .. } = c {
            if exact.contains(name.as_str()) {
                continue; // already the id the hub routes on
            }
            match escaped.get(name.as_str()).map(|v| v.as_slice()) {
                Some([only]) => *name = (*only).to_owned(),
                Some(many) => {
                    // Two tools share this wire name and neither is an exact
                    // match. Guessing would route a call to the wrong system;
                    // leaving it alone makes the hub say it does not exist.
                    tracing::warn!(
                        wire = %name,
                        candidates = ?many,
                        "ambiguous tool name from the model; refusing to guess"
                    );
                }
                None => *name = unsanitize(name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn resolved(wire: &str, offered: &[&str]) -> String {
        let mut turn = TurnResponse {
            content: vec![Content::ToolUse {
                id: ToolUseId::new("c1"),
                name: wire.to_owned(),
                input: json!({}),
            }],
            stop_reason: StopReason::ToolUse,
            usage: None,
        };
        let req = TurnRequest {
            system: String::new(),
            messages: vec![],
            tools: offered
                .iter()
                .map(|n| {
                    ToolDescription::new(
                        openbot_proto::ToolId::new(*n),
                        "",
                        json!({"type":"object"}),
                    )
                })
                .collect(),
        };
        restore_names(&mut turn, &req);
        match &turn.content[0] {
            Content::ToolUse { name, .. } => name.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_escaped_name_resolves_to_the_id_it_came_from() {
        assert_eq!(resolved("fs__read", &["fs.read"]), "fs.read");
        assert_eq!(resolved("shell__exec", &["shell.exec"]), "shell.exec");
    }

    #[test]
    fn a_connector_name_is_returned_untouched() {
        // The id really contains `__`; unescaping it would invent a tool.
        assert_eq!(
            resolved("linear__create_issue", &["linear__create_issue"]),
            "linear__create_issue"
        );
    }

    #[test]
    fn an_exact_match_beats_an_escaped_one_whatever_the_order() {
        // `fs.read` and a connector `fs` offering `read` share one wire name.
        // Resolution must not depend on which was listed first.
        for order in [vec!["fs.read", "fs__read"], vec!["fs__read", "fs.read"]] {
            assert_eq!(
                resolved("fs__read", &order),
                "fs__read",
                "listing order changed where the call went: {order:?}"
            );
        }
    }

    #[test]
    fn a_name_we_never_offered_is_left_readable() {
        // Nothing to resolve against, so the naive inverse is the best guess:
        // the hub will answer "unknown tool" either way, and `made.up` reads
        // like the tool ids in the message.
        assert_eq!(resolved("made__up", &["fs.read"]), "made.up");
    }

    use super::*;
    use crate::model::Message;
    use openbot_proto::frames::ToolDescription;

    fn req() -> TurnRequest {
        TurnRequest {
            system: "be useful".into(),
            messages: vec![
                Message::user("read the file"),
                Message::assistant(vec![
                    Content::text("on it"),
                    Content::ToolUse {
                        id: ToolUseId::new("t1"),
                        name: "fs.read".into(),
                        input: json!({ "path": "a.txt" }),
                    },
                ]),
                Message {
                    role: Role::User,
                    content: vec![Content::ToolResult {
                        id: ToolUseId::new("t1"),
                        content: "hello".into(),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![ToolDescription::new(
                "fs.read",
                "read a file",
                json!({"type":"object"}),
            )],
        }
    }

    #[test]
    fn dotted_tool_names_survive_a_round_trip() {
        assert_eq!(sanitize("fs.read"), "fs__read");
        assert_eq!(unsanitize(&sanitize("fs.read")), "fs.read");
        assert_eq!(unsanitize(&sanitize("shell.exec")), "shell.exec");
    }

    #[test]
    fn anthropic_request_keeps_system_separate_and_blocks_intact() {
        let b = anthropic::request("claude-x", 1024, &req());
        assert_eq!(b["system"], "be useful");
        assert_eq!(b["messages"].as_array().unwrap().len(), 3);
        assert_eq!(b["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(b["messages"][1]["content"][1]["name"], "fs__read");
        assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(b["messages"][2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(b["tools"][0]["name"], "fs__read");
    }

    #[test]
    fn openai_request_hoists_system_and_splits_tool_results_into_their_own_messages() {
        let b = openai::request("grok-x", 1024, &req());
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[2]["role"], "assistant");
        // Arguments must be a JSON string, not an object.
        assert!(msgs[2]["tool_calls"][0]["function"]["arguments"].is_string());
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "t1");
        assert_eq!(b["tools"][0]["function"]["name"], "fs__read");
    }

    #[test]
    fn anthropic_response_maps_stop_reasons() {
        let v = json!({
            "content": [{"type":"text","text":"hi"},
                        {"type":"tool_use","id":"x","name":"fs__read","input":{"path":"a"}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        });
        let r = anthropic::response(&v).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.content.len(), 2);
        match &r.content[1] {
            // The wire name, unresolved: turning it back into an id needs the
            // catalogue, which this layer does not have. `restore_names` owns
            // that, and is the only place that can do it unambiguously.
            Content::ToolUse { name, .. } => assert_eq!(name, "fs__read"),
            other => panic!("expected tool_use, got {other:?}"),
        }
        assert_eq!(r.usage.unwrap().input_tokens, 10);
    }

    #[test]
    fn anthropic_response_ignores_block_types_it_does_not_know() {
        let v = json!({
            "content": [{"type":"thinking","thinking":"..."},{"type":"text","text":"ok"}],
            "stop_reason": "end_turn"
        });
        let r = anthropic::response(&v).unwrap();
        assert_eq!(r.content.len(), 1);
    }

    #[test]
    fn openai_response_parses_tool_calls_and_arguments() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1", "type": "function",
                        "function": {"name": "shell__exec", "arguments": "{\"command\":\"ls\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7}
        });
        let r = openai::response(&v).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        match &r.content[0] {
            Content::ToolUse { name, input, .. } => {
                assert_eq!(name, "shell__exec");
                assert_eq!(input["command"], "ls");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_an_empty_object() {
        let v = json!({
            "choices": [{
                "message": {"tool_calls": [{
                    "id": "c", "function": {"name": "fs__list", "arguments": "not json"}
                }]},
                "finish_reason": "tool_calls"
            }]
        });
        let r = openai::response(&v).unwrap();
        match &r.content[0] {
            Content::ToolUse { input, .. } => assert_eq!(*input, json!({})),
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn a_response_with_no_choices_is_malformed_not_a_panic() {
        assert!(matches!(
            openai::response(&json!({})),
            Err(ModelError::Malformed(_))
        ));
    }

    #[test]
    fn dialects_parse_from_the_names_people_actually_type() {
        for s in ["anthropic", "claude", "Messages"] {
            assert_eq!(s.parse::<Dialect>().unwrap(), Dialect::AnthropicMessages);
        }
        for s in ["openai", "xai", "grok", "compatible"] {
            assert_eq!(s.parse::<Dialect>().unwrap(), Dialect::OpenAiChat);
        }
        assert!("gemini".parse::<Dialect>().is_err());
    }
}

#[cfg(test)]
mod declined_tests {
    use super::*;

    /// A refusal is not a finished answer.
    ///
    /// Both parsers end in `_ => StopReason::EndTurn`. Anthropic's `refusal`
    /// and the OpenAI dialects' `content_filter`, the provider saying outright
    /// that the model declined, must be matched before that arm or a refused
    /// run is reported as completed.
    #[test]
    fn a_provider_refusal_is_not_reported_as_a_finished_turn() {
        let anth = |reason: &str| {
            anthropic::response(&json!({
                "content": [{ "type": "text", "text": "I can't help with that." }],
                "stop_reason": reason,
            }))
            .expect("parses")
            .stop_reason
        };
        assert_eq!(anth("refusal"), StopReason::Declined);
        // The ordinary endings are untouched.
        assert_eq!(anth("end_turn"), StopReason::EndTurn);
        assert_eq!(anth("max_tokens"), StopReason::MaxTokens);
        assert_eq!(anth("tool_use"), StopReason::ToolUse);
        // A stop reason this build has never seen is still an ending, not a
        // refusal: guessing "declined" for the unknown would put words in the
        // model's mouth.
        assert_eq!(anth("stop_sequence"), StopReason::EndTurn);

        let oai = |reason: &str| {
            openai::response(&json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "I can't help with that." },
                    "finish_reason": reason,
                }],
            }))
            .expect("parses")
            .stop_reason
        };
        assert_eq!(oai("content_filter"), StopReason::Declined);
        assert_eq!(oai("stop"), StopReason::EndTurn);
        assert_eq!(oai("length"), StopReason::MaxTokens);
        assert_eq!(oai("tool_calls"), StopReason::ToolUse);
    }
}

#[cfg(test)]
mod reported_usage_tests {
    use super::*;
    use serde_json::json;

    fn anthropic_body(extra: Value) -> Value {
        let mut v = json!({
            "content": [{"type":"text","text":"hi"}],
            "stop_reason": "end_turn"
        });
        if let Some(u) = extra.as_object() {
            for (k, val) in u {
                v[k] = val.clone();
            }
        }
        v
    }

    /// "Nothing was measured" and "it cost nothing" are opposite facts.
    ///
    /// The agent loop treats `None` as "this provider cannot be metered" and
    /// warns that the token budget cannot be enforced; it treats
    /// `Some(Usage { 0, 0 })` as a free turn, which accumulates nothing and
    /// trips no cap, silently. A parser that turns a missing `usage` into the
    /// second makes a budget against such a provider a no-op, and the warning
    /// (which tests for `None`) never fires.
    ///
    /// Reachable by configuration: `base_url` and `dialect` exist so a local
    /// OpenAI-compatible server or a gateway can be used, and those are the
    /// providers that leave `usage` out or send it empty.
    #[test]
    fn a_provider_that_reports_no_usage_reports_none() {
        assert_eq!(
            anthropic::response(&anthropic_body(json!({})))
                .unwrap()
                .usage,
            None,
            "a missing usage object was read as a turn that cost nothing"
        );
        assert_eq!(
            anthropic::response(&anthropic_body(json!({"usage": {}})))
                .unwrap()
                .usage,
            None,
            "an empty usage object was read as a turn that cost nothing"
        );
        let o = json!({
            "choices": [{"message": {"role":"assistant","content":"hi"},
                         "finish_reason": "stop"}]
        });
        assert_eq!(openai::response(&o).unwrap().usage, None);
    }

    /// A counter the provider did send as zero was still measured.
    ///
    /// The other half of the property above: a provider answering `0` has
    /// answered, and the budget should go on trusting it rather than treating
    /// everything as unmetered.
    #[test]
    fn a_zero_the_provider_actually_sent_is_still_a_measurement() {
        let v = anthropic_body(json!({"usage": {"input_tokens": 0, "output_tokens": 0}}));
        assert_eq!(
            anthropic::response(&v).unwrap().usage,
            Some(Usage {
                input_tokens: 0,
                output_tokens: 0
            })
        );
    }

    /// One counter present and the other missing is still a measurement: a
    /// gateway that reports only completion tokens is metering, badly, and
    /// half a number is better than pretending the cap does not exist.
    #[test]
    fn one_counter_is_enough_to_count_as_reported() {
        let v = anthropic_body(json!({"usage": {"output_tokens": 7}}));
        assert_eq!(
            anthropic::response(&v).unwrap().usage,
            Some(Usage {
                input_tokens: 0,
                output_tokens: 7
            })
        );
    }
}
