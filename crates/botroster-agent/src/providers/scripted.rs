//! A deterministic provider that replays a canned sequence of turns.
//!
//! This is what makes the agent loop testable. A live model is
//! non-deterministic, rate-limited, costs money, and needs credentials, none
//! of which belong in CI. The scripted provider makes the loop's state machine
//! provable: given exactly these turns, the agent must make exactly these tool
//! calls in this order and finish for this reason.
//!
//! It also records every [`TurnRequest`] it received, so a test can assert on
//! what the agent sent: that tool schemas were forwarded, that results were
//! appended in the right shape, that history accumulates.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;

use crate::model::{
    Content, Model, ModelError, StopReason, ToolUseId, TurnRequest, TurnResponse, Usage,
};

pub struct Scripted {
    name: String,
    turns: Mutex<VecDeque<Result<TurnResponse, ModelError>>>,
    seen: Mutex<Vec<TurnRequest>>,
}

impl Scripted {
    pub fn new(turns: Vec<TurnResponse>) -> Self {
        Self::from_results(turns.into_iter().map(Ok).collect())
    }

    /// A script that may fail at a chosen turn.
    pub fn from_results(turns: Vec<Result<TurnResponse, ModelError>>) -> Self {
        Self {
            name: "scripted".into(),
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn builder() -> ScriptBuilder {
        ScriptBuilder::default()
    }

    /// Every request the agent has made, in order.
    pub fn requests(&self) -> Vec<TurnRequest> {
        self.seen.lock().expect("script lock").clone()
    }

    /// How many turns the agent consumed.
    pub fn turns_taken(&self) -> usize {
        self.seen.lock().expect("script lock").len()
    }

    /// Turns still unconsumed. A non-zero value at the end of a test usually
    /// means the agent stopped earlier than the script expected.
    pub fn turns_remaining(&self) -> usize {
        self.turns.lock().expect("script lock").len()
    }
}

#[async_trait::async_trait]
impl Model for Scripted {
    async fn turn(&self, req: &TurnRequest) -> Result<TurnResponse, ModelError> {
        self.seen.lock().expect("script lock").push(req.clone());
        self.turns
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or(Err(ModelError::ScriptExhausted))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Default)]
pub struct ScriptBuilder {
    turns: Vec<Result<TurnResponse, ModelError>>,
    next_id: u32,
}

impl ScriptBuilder {
    fn id(&mut self) -> ToolUseId {
        self.next_id += 1;
        ToolUseId::new(format!("tu{}", self.next_id))
    }

    /// One turn requesting a single tool.
    pub fn call(self, tool: &str, input: Value) -> Self {
        self.calls(&[(tool, input)])
    }

    /// One turn requesting several tools at once.
    pub fn calls(mut self, calls: &[(&str, Value)]) -> Self {
        let content = calls
            .iter()
            .map(|(tool, input)| Content::ToolUse {
                id: self.id(),
                name: (*tool).to_owned(),
                input: input.clone(),
            })
            .collect();
        self.turns.push(Ok(TurnResponse {
            content,
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
        }));
        self
    }

    /// One turn with both commentary and a tool call, the common real shape.
    pub fn say_and_call(mut self, text: &str, tool: &str, input: Value) -> Self {
        let id = self.id();
        self.turns.push(Ok(TurnResponse {
            content: vec![
                Content::text(text),
                Content::ToolUse {
                    id,
                    name: tool.to_owned(),
                    input,
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
        }));
        self
    }

    /// A final turn: text, end of turn.
    pub fn say(mut self, text: &str) -> Self {
        self.turns.push(Ok(TurnResponse {
            content: vec![Content::text(text)],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
        }));
        self
    }

    /// A turn the provider truncated.
    pub fn truncated(mut self, text: &str) -> Self {
        self.turns.push(Ok(TurnResponse {
            content: vec![Content::text(text)],
            stop_reason: StopReason::MaxTokens,
            usage: Some(Usage::default()),
        }));
        self
    }

    /// A malformed turn: claims tool use, sends none. Real providers do this.
    pub fn claims_tool_use_but_sends_none(mut self) -> Self {
        self.turns.push(Ok(TurnResponse {
            content: vec![Content::text("about to use a tool")],
            stop_reason: StopReason::ToolUse,
            usage: None,
        }));
        self
    }

    /// A turn the provider fails outright.
    ///
    /// Makes the failure path reachable mid-script, rather than only by
    /// exhausting it, so a provider outage halfway through a task can be
    /// tested.
    pub fn fails(mut self, e: ModelError) -> Self {
        self.turns.push(Err(e));
        self
    }

    pub fn build(self) -> Scripted {
        Scripted::from_results(self.turns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    fn req() -> TurnRequest {
        TurnRequest {
            system: "s".into(),
            messages: vec![Message::user("go")],
            tools: vec![],
        }
    }

    #[tokio::test]
    async fn turns_are_replayed_in_order() {
        let s = Scripted::builder()
            .call("fs.write", serde_json::json!({"path": "a"}))
            .say("done")
            .build();

        let first = s.turn(&req()).await.unwrap();
        assert_eq!(first.stop_reason, StopReason::ToolUse);
        let second = s.turn(&req()).await.unwrap();
        assert_eq!(second.stop_reason, StopReason::EndTurn);
        assert_eq!(s.turns_taken(), 2);
        assert_eq!(s.turns_remaining(), 0);
    }

    #[tokio::test]
    async fn running_past_the_script_is_an_explicit_error() {
        let s = Scripted::builder().say("only one").build();
        s.turn(&req()).await.unwrap();
        assert!(matches!(
            s.turn(&req()).await,
            Err(ModelError::ScriptExhausted)
        ));
    }

    #[tokio::test]
    async fn requests_are_recorded_for_assertions() {
        let s = Scripted::builder().say("hi").build();
        s.turn(&req()).await.unwrap();
        assert_eq!(s.requests().len(), 1);
        assert_eq!(s.requests()[0].system, "s");
    }

    #[tokio::test]
    async fn parallel_tool_calls_land_in_one_turn() {
        let s = Scripted::builder()
            .calls(&[
                ("fs.read", serde_json::json!({"path": "a"})),
                ("fs.read", serde_json::json!({"path": "b"})),
            ])
            .build();
        let t = s.turn(&req()).await.unwrap();
        assert_eq!(t.content.len(), 2);
        // Ids must be distinct or results cannot be correlated.
        let ids: Vec<_> = t
            .content
            .iter()
            .filter_map(|c| match c {
                Content::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_ne!(ids[0], ids[1]);
    }
}
