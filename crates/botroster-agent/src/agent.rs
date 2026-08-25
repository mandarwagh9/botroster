//! The agent loop: model, tool calls, results, model again, until done.
//!
//! Everything the loop does is also emitted as an [`AgentEvent`]. That stream
//! is the single source of truth for any view of a session: the terminal
//! renderer, the read-only web session view, and the desktop client all
//! consume it rather than re-deriving state. If something is visible in one
//! surface and not another, the event is missing, not the surface.

use std::sync::Arc;
use std::time::Instant;

use botroster_proto::frames::ToolDescription;
use botroster_proto::ToolCallId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::hub_client::{HubClient, HubError};
use crate::model::{
    Content, Message, Model, ModelError, Role, StopReason, ToolUseId, TurnRequest, Usage,
};

/// Cap on how much of a tool result is fed back to the model. Tool output can
/// be arbitrarily large; a context window cannot.
const RESULT_CHAR_LIMIT: usize = 8_000;

/// Rough ceiling on the whole conversation, in characters.
///
/// Characters, not tokens. Counting tokens properly needs the vendor's
/// tokenizer, and guessing with somebody else's is worse than an open
/// approximation. At roughly 4 characters per token this is around 30k tokens;
/// the aim is to stay well clear of the edge, not to pack a window.
///
/// Without a ceiling a run only grows: every turn resends the whole
/// conversation, each tool result is capped at [`RESULT_CHAR_LIMIT`] but they
/// accumulate, and a 24-step run can carry ~48k tokens of results. On a large
/// model that is fine; on a 32k one the run dies around step five with a
/// vendor error about context length, which reads as a model failure rather
/// than a run that went on too long, and lands in a routine nobody is
/// watching.
const CONVERSATION_CHAR_BUDGET: usize = 120_000;

/// How many recent messages are never compacted.
///
/// The model needs what just happened in full; it is the older results it can
/// re-fetch. Six covers roughly the last three exchanges.
const KEEP_RECENT: usize = 6;

/// What replaces a dropped result. Phrased as an instruction so the model can
/// run the tool again rather than guessing what the result said.
const DROPPED: &str =
    "[earlier result dropped to fit the context — run the tool again if you still need it]";

/// Shrink old tool results until the conversation fits the budget.
///
/// Contents are replaced, never messages removed. Every `tool_use` must keep
/// its matching `tool_result`, in order, or a vendor rejects the whole
/// request, so compaction that deletes messages trades a context error for a
/// 400. Only tool results are touched: the task and the assistant's reasoning
/// are small and are the thread of the run, while a result is bulky and, by
/// construction, something a tool can produce again.
///
/// The `&mut [Message]` parameter type is load-bearing: a slice cannot lose an
/// element, so the invariant above is enforced by the signature. Do not widen
/// it to a `Vec`. `compaction_never_breaks_a_tool_call_from_its_result` holds
/// the invariant and also catches a reorder.
///
/// Returns how many results were dropped, for the caller to report.
fn compact(messages: &mut [Message], budget: usize, keep_recent: usize) -> usize {
    let size = |m: &[Message]| -> usize {
        m.iter()
            .flat_map(|m| m.content.iter())
            .map(|c| match c {
                Content::Text { text } => text.len(),
                Content::ToolUse { input, .. } => input.to_string().len(),
                Content::ToolResult { content, .. } => content.len(),
            })
            .sum()
    };

    if size(messages) <= budget {
        return 0;
    }

    let stop = messages.len().saturating_sub(keep_recent);
    let mut dropped = 0;
    for i in 0..stop {
        for c in &mut messages[i].content {
            if let Content::ToolResult { content, .. } = c {
                if content != DROPPED {
                    *content = DROPPED.to_owned();
                    dropped += 1;
                }
            }
        }
        if size(messages) <= budget {
            break;
        }
    }
    dropped
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    Started {
        task: String,
        model: String,
        tools: Vec<String>,
    },
    /// A model turn is in flight. Renders as the "thinking" state.
    Thinking {
        step: u32,
    },
    AssistantText {
        step: u32,
        text: String,
    },
    ToolCallStarted {
        step: u32,
        id: ToolUseId,
        call_id: ToolCallId,
        tool: String,
        args: Value,
    },
    ToolProgress {
        call_id: ToolCallId,
        payload: Value,
    },
    ToolCallFinished {
        id: ToolUseId,
        call_id: ToolCallId,
        ok: bool,
        output: Value,
        elapsed_ms: u64,
    },
    /// Something the person said while the turn was running, now part of it.
    Redirected {
        text: String,
    },
    Finished {
        reason: FinishReason,
        steps: u32,
        /// What the run cost, in tokens, across every turn.
        #[serde(default)]
        usage: Usage,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FinishReason {
    /// The model ended its turn without asking for more tools.
    Completed,
    /// The step budget was exhausted. Not a success.
    StepLimit {
        max_steps: u32,
    },
    /// The provider truncated a turn.
    Truncated,
    ModelFailed {
        message: String,
        /// Whether trying the same run again shortly could work.
        ///
        /// Decided here, while the typed error still exists. A routine asks
        /// this to know whether an outage cost it the day or a few minutes; by
        /// the time this reason reaches it the error is a string, and
        /// searching a string for "429" is unreliable.
        #[serde(default)]
        transient: bool,
    },
    HubFailed {
        message: String,
    },
    /// The run reached its token budget and stopped between turns.
    TokenBudget {
        spent: u64,
        budget: u64,
    },
    /// The model declined the request. Anthropic's `refusal`, the
    /// OpenAI-compatible dialects' `content_filter`.
    ///
    /// Distinct from `Completed`: a refusal and a finished answer would be
    /// indistinguishable on screen if both rendered as completed, and they
    /// mean opposite things. Distinct from `NothingApproved` too: that is an
    /// absence on this side, nobody there to ask; this is a judgement the
    /// model made about the request.
    Declined,
    /// Nothing could be approved, because nobody was there to approve it.
    ///
    /// Distinct from a run where a person answered "no": that is a decision
    /// about one action, and trying a different approach is reasonable. This
    /// is an absence, it cannot change mid-run, and a nightly routine left on
    /// the default would otherwise spend its whole budget rediscovering that.
    NothingApproved {
        message: String,
        /// The tool whose call was refused.
        ///
        /// Carried because the advice a client gives depends on it, and the
        /// alternative is a renderer inferring it from prose. `--approve auto`
        /// is the answer for a gated action and cannot be the answer for a
        /// credential: `ApprovalHandler::supply` refuses by default precisely
        /// so no unattended mode invents one. A client that offered it anyway
        /// would be telling somebody to run a command that cannot work.
        tool: String,
    },
    /// A person has taken the computer, so the run stopped rather than
    /// hammering it.
    ///
    /// Separate from a failed tool because it is not information the model can
    /// act on: no rephrasing gets past it, and retrying until the step budget
    /// runs out spends a whole run's tokens to end in `StepLimit`, which reads
    /// as "the agent could not do it" rather than "somebody was using the
    /// computer". For a routine it also means the work should be tried again
    /// later, not marked as done.
    ComputerBusy {
        message: String,
    },
    /// Somebody asked it to stop, and it stopped.
    ///
    /// Distinct from every other reason here because it is the only one that
    /// is not a finding: nothing was wrong, nobody ran out of anything, the
    /// work was simply no longer wanted. A routine should not retry it and a
    /// person should not be shown an error for pressing the button they were
    /// given.
    Cancelled {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub system: String,
    /// Hard bound on model turns. A loop that cannot terminate is the default
    /// failure mode of this architecture, not an edge case.
    pub max_steps: u32,
    /// Stop once the run has spent this many tokens, in and out together.
    ///
    /// `max_steps` bounds turns, not spend, and those are not the same thing:
    /// every turn resends the whole conversation, so late turns cost far more
    /// than early ones. A run that stays well inside its step budget can still
    /// cost several times what its operator expected.
    ///
    /// Two limits:
    ///
    /// * It is checked between turns, because usage is only known after a
    ///   turn returns. One turn can therefore overshoot. The check runs before
    ///   the next turn rather than after the last one because the next turn is
    ///   the expensive one, since the context has grown.
    /// * It cannot be enforced against a provider that reports no usage. That
    ///   is warned about the first time rather than left as a limit that
    ///   silently never fires.
    pub token_budget: Option<u64>,
    /// Rough ceiling on the conversation, in characters.
    ///
    /// Configurable because models differ by an order of magnitude in how much
    /// they can hold: the default is wrong for a 32k model in one direction
    /// and a million-token one in the other. See [`CONVERSATION_CHAR_BUDGET`]
    /// for why this counts characters and not tokens.
    pub context_budget: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system: default_system_prompt(),
            max_steps: 24,
            // Off by default: a cap that surprises someone mid-task is its own
            // kind of failure, and the step budget already bounds a runaway.
            token_budget: None,
            context_budget: CONVERSATION_CHAR_BUDGET,
        }
    }
}

pub fn default_system_prompt() -> String {
    "You are a botroster Bot: a persistent teammate with your own cloud computer.\n\
     You have a workspace with a filesystem and a shell. Use the tools to do real work \
     rather than describing what you would do.\n\
     Work in small verifiable steps. Check results before moving on.\n\
     Stop and report when the task is done, or when you need a decision only the user can make.\n\
     Never take a consequential external action without being asked to."
        .to_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    pub reason: FinishReason,
    pub steps: u32,
    /// The assistant's final text.
    pub text: String,
    pub transcript: Vec<Message>,
    /// Tokens across every turn of this run.
    ///
    /// Summed rather than kept per turn: what an operator wants is the bill
    /// for the run, and the per-turn detail is in the transcript. Without it a
    /// nightly routine has no way to report what it cost.
    ///
    /// Tokens, not money: prices differ per model, change without notice, and
    /// a number this code invented would be believed.
    pub usage: Usage,
}

impl AgentOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self.reason, FinishReason::Completed)
    }
}

pub struct Agent {
    model: Arc<dyn Model>,
    hub: Arc<HubClient>,
    config: AgentConfig,
    /// Conversation the run starts from.
    ///
    /// This is what makes a Bot a teammate rather than a chat box: the task
    /// arrives on the end of everything that came before, so "do that again
    /// for the other region" means something.
    history: Vec<Message>,
    /// Flipped by whoever wants this run to stop.
    ///
    /// A `watch` receiver rather than an `AtomicBool` so the run can be
    /// interrupted rather than only polled: a model turn can take a minute,
    /// and a stop button that waits a minute is one people press twice and
    /// then distrust.
    cancel: Option<tokio::sync::watch::Receiver<bool>>,
    redirects: Option<Redirects>,
}

/// Instructions a person sends while a turn is running.
///
/// A direct message takes priority over background work and can redirect the
/// current turn. botroster runs one turn at a time per session, and the answer is
/// not to remove that lock (two turns answering one conversation is what the
/// lock exists to prevent) but to let a running turn be told something without
/// a second one starting.
///
/// Delivered at a step boundary, never mid-tool-call: a tool that has been
/// approved and started runs to its end, because interrupting between the
/// approval and the act is the one place an interruption could do harm.
#[derive(Clone, Debug, Default)]
pub struct Redirects(Arc<std::sync::Mutex<Vec<String>>>);

impl Redirects {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Say something to the turn in flight. Queued, and picked up at the next
    /// step.
    pub fn send(&self, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.0.lock().expect("redirects lock").push(text);
    }

    /// Everything said since the last look, in the order it was said.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("redirects lock"))
    }

    /// What was said but never reached the turn.
    ///
    /// A redirect is picked up at the top of a step, so anything sent after
    /// the last one (during the final model call, which can take seconds) is
    /// still queued when the turn ends. Whoever sent it has to be told: a
    /// message accepted and quietly dropped is worse than one refused, because
    /// the person believes it landed and the Bot never heard it.
    #[must_use]
    pub fn undelivered(&self) -> Vec<String> {
        self.take()
    }
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, hub: Arc<HubClient>, config: AgentConfig) -> Self {
        Self {
            model,
            hub,
            config,
            history: Vec::new(),
            cancel: None,
            redirects: None,
        }
    }

    /// Start this run from an existing conversation.
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    /// Let somebody stop this run.
    ///
    /// Send `true` on the paired sender and the run ends at the next boundary
    /// it can end at, or immediately if it is waiting on the model. Opt-in, so
    /// a caller with no stop button is unaffected.
    pub fn with_cancel(mut self, cancel: tokio::sync::watch::Receiver<bool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Accept instructions sent while a turn is running.
    ///
    /// Opt-in: a caller with no way to interrupt passes nothing and the loop
    /// is unaffected.
    #[must_use]
    pub fn with_redirects(mut self, redirects: Redirects) -> Self {
        self.redirects = Some(redirects);
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| *c.borrow())
    }

    /// One model turn, abandoned if the run is cancelled while it is in
    /// flight. `None` means cancelled.
    async fn model_turn_or_cancel(
        &self,
        req: &crate::model::TurnRequest,
    ) -> Option<Result<crate::model::TurnResponse, ModelError>> {
        let Some(cancel) = self.cancel.clone() else {
            return Some(self.model.turn(req).await);
        };
        let mut cancel = cancel;
        tokio::select! {
            // Biased so that an already-set flag wins over a model call that
            // happens to be ready in the same poll. Stopping when asked takes
            // priority over one more turn's output.
            biased;
            _ = async { while !*cancel.borrow_and_update() { if cancel.changed().await.is_err() { std::future::pending::<()>().await; } } } => None,
            t = self.model.turn(req) => Some(t),
        }
    }

    /// How many messages the run began with, so a caller can tell which of
    /// `AgentOutcome::transcript` are new and persist only those.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Run a task to completion.
    ///
    /// `progress` is the receiver handed back by [`HubClient::connect`]; the
    /// agent forwards it into the event stream so a caller has one channel to
    /// subscribe to rather than two to interleave.
    pub async fn run(
        &self,
        task: &str,
        tools: Vec<ToolDescription>,
        mut progress: mpsc::UnboundedReceiver<botroster_proto::frames::ToolCallProgressFrame>,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> AgentOutcome {
        let forwarder = {
            let events = events.clone();
            tokio::spawn(async move {
                while let Some(p) = progress.recv().await {
                    if events
                        .send(AgentEvent::ToolProgress {
                            call_id: p.call_id,
                            payload: p.payload,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        };

        let outcome = self.drive(task, tools, &events).await;

        let _ = events.send(AgentEvent::Finished {
            reason: outcome.reason.clone(),
            steps: outcome.steps,
            text: outcome.text.clone(),
            usage: outcome.usage,
        });
        forwarder.abort();
        outcome
    }

    async fn drive(
        &self,
        task: &str,
        tools: Vec<ToolDescription>,
        events: &mpsc::UnboundedSender<AgentEvent>,
    ) -> AgentOutcome {
        let mut spent = Usage::default();
        let _ = events.send(AgentEvent::Started {
            task: task.to_owned(),
            model: self.model.name().to_owned(),
            tools: tools
                .iter()
                .map(|t| t.tool_id.as_str().to_owned())
                .collect(),
        });

        let mut messages = self.history.clone();
        messages.push(Message::user(task));
        let mut step = 0u32;

        loop {
            // Shrink the oldest results if the conversation has outgrown its
            // budget. Before the token check, since compaction changes what
            // the next turn will cost.
            let dropped = compact(&mut messages, self.config.context_budget, KEEP_RECENT);
            if dropped > 0 {
                tracing::info!(
                    dropped,
                    step,
                    "dropped older tool results to stay inside the context"
                );
            }

            // Asked to stop. Checked here, beside the other reasons to stop
            // before spending anything, because this is the cheapest moment to
            // notice: no model call has been made for this step and no tool
            // has been touched.
            if self.cancelled() {
                return self.finish(
                    FinishReason::Cancelled {
                        message: "stopped at your request".into(),
                    },
                    step,
                    &messages,
                    spent,
                );
            }

            // Anything the person said while this turn was running. Appended
            // here, at a step boundary, so it reaches the model before the
            // next call and never lands between a tool's approval and the tool
            // running. After the cancel check, because "stop" outranks
            // "instead, do this".
            if let Some(redirects) = &self.redirects {
                for text in redirects.take() {
                    let _ = events.send(AgentEvent::Redirected { text: text.clone() });
                    messages.push(Message::user(text));
                }
            }

            // Checked before the next turn, not after the last one: the next
            // turn carries the whole grown conversation and is the expensive
            // one to have avoided.
            if let Some(budget) = self.config.token_budget {
                let so_far = spent.input_tokens + spent.output_tokens;
                if so_far >= budget {
                    return self.finish(
                        FinishReason::TokenBudget {
                            spent: so_far,
                            budget,
                        },
                        step,
                        &messages,
                        spent,
                    );
                }
            }

            if step >= self.config.max_steps {
                return self.finish(
                    FinishReason::StepLimit {
                        max_steps: self.config.max_steps,
                    },
                    step,
                    &messages,
                    spent,
                );
            }
            step += 1;
            let _ = events.send(AgentEvent::Thinking { step });

            let req = TurnRequest {
                system: self.config.system.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
            };
            // The one await long enough to interrupt. Everything else in this
            // loop is a tool call, which finishes on its own and whose result
            // should be kept; abandoning a model turn costs only the tokens
            // already spent on it.
            let turn = match self.model_turn_or_cancel(&req).await {
                Some(Ok(t)) => t,
                Some(Err(e)) => return self.finish(model_failed(e), step, &messages, spent),
                None => {
                    return self.finish(
                        FinishReason::Cancelled {
                            message: "stopped at your request, mid-turn".into(),
                        },
                        step,
                        &messages,
                        spent,
                    )
                }
            };

            // Counted before anything can return: a run that stops early still
            // spent what it spent, and that is exactly the run someone will
            // ask about.
            match &turn.usage {
                Some(u) => {
                    spent.input_tokens += u.input_tokens;
                    spent.output_tokens += u.output_tokens;
                }
                None if self.config.token_budget.is_some() && step == 1 => {
                    // A budget that can never fire is worse than no budget:
                    // the operator believes there is a cap. Say so once, at
                    // the first turn, rather than at every one.
                    tracing::warn!(
                        model = %self.model.name(),
                        "a token budget is set but this provider reports no usage, \
                         so the budget cannot be enforced"
                    );
                }
                None => {}
            }

            let assistant = Message::assistant(turn.content.clone());
            let text = assistant.text();
            if !text.is_empty() {
                let _ = events.send(AgentEvent::AssistantText { step, text });
            }
            messages.push(assistant.clone());

            match turn.stop_reason {
                StopReason::EndTurn => {
                    return self.finish(FinishReason::Completed, step, &messages, spent)
                }
                StopReason::MaxTokens => {
                    return self.finish(FinishReason::Truncated, step, &messages, spent)
                }
                // The model declined. Reported as its own ending rather than
                // folded into `Completed`, because those look identical to a
                // person and mean opposite things: one is the task done, the
                // other is the task refused with whatever partial text the
                // model produced before declining.
                StopReason::Declined => {
                    return self.finish(FinishReason::Declined, step, &messages, spent)
                }
                StopReason::ToolUse => {}
            }

            let uses: Vec<(ToolUseId, String, Value)> = assistant
                .tool_uses()
                .map(|(id, name, input)| (id.clone(), name.to_owned(), input.clone()))
                .collect();

            if uses.is_empty() {
                // The provider said "tool_use" but sent none. Treating this as
                // completion would silently drop the task, so end explicitly
                // and say why.
                return self.finish(
                    FinishReason::ModelFailed {
                        message: "provider reported stop_reason=tool_use with no tool_use block"
                            .into(),
                        // A provider contradicting itself is not an outage.
                        // The same request would get the same answer, and a
                        // routine retrying it every ten minutes would only
                        // spend money proving that.
                        transient: false,
                    },
                    step,
                    &messages,
                    spent,
                );
            }

            // Two calls in one turn must be distinguishable. A result is
            // paired back to its call by id, both by the provider on the next
            // turn and by this loop, which builds each `ToolCallId` as
            // `<id>-<step>`. Two uses sharing an id produce two results
            // carrying that id and two calls carrying one `ToolCallId`; the
            // model can then be handed the wrong answer to the wrong question,
            // and the transcript records one call where there were two.
            //
            // In practice this arrives as an empty id rather than a repeated
            // one. Both dialects read the id with `unwrap_or_default`, so a
            // block without one becomes `""`, and a turn asking for two tools
            // gets `""` twice. Anthropic and OpenAI always send ids; a gateway
            // or a local server answering in their shape (the `--base-url`
            // case) may not.
            //
            // A single id-less call is intentionally accepted: nothing is
            // ambiguous with one, and refusing it would break a working setup
            // for a hazard that is not present.
            {
                let mut seen = std::collections::HashSet::new();
                if let Some((id, name, _)) = uses.iter().find(|(id, _, _)| !seen.insert(id.clone()))
                {
                    let which = if id.as_str().is_empty() {
                        "no id at all".to_owned()
                    } else {
                        format!("the id `{}`", id.as_str())
                    };
                    return self.finish(
                        FinishReason::ModelFailed {
                            message: format!(
                                "the provider asked for {} tools in one turn and gave two of them \
                                 {which} (`{name}` among them), so their results cannot be told \
                                 apart",
                                uses.len()
                            ),
                            // The same request gets the same malformed answer.
                            transient: false,
                        },
                        step,
                        &messages,
                        spent,
                    );
                }
            }

            let mut results = Vec::with_capacity(uses.len());
            for (id, name, args) in uses {
                let call_id = ToolCallId::new(format!("{}-{}", id.as_str(), step));
                let _ = events.send(AgentEvent::ToolCallStarted {
                    step,
                    id: id.clone(),
                    call_id: call_id.clone(),
                    tool: name.clone(),
                    args: args.clone(),
                });

                let started = Instant::now();
                let outcome = self.hub.call_tool(&name, &call_id, args).await;
                let elapsed_ms = started.elapsed().as_millis() as u64;

                let (ok, output, rendered) = match outcome {
                    Ok(v) => {
                        let r = render(&v);
                        (true, v, r)
                    }
                    // A failed tool is information for the model, not a fatal
                    // error: it should read the message and try something else.
                    // Only a dead connection ends the run.
                    Err(HubError::Closed) => {
                        return self.finish(
                            FinishReason::HubFailed {
                                message: "the hub connection closed".into(),
                            },
                            step,
                            &messages,
                            spent,
                        )
                    }
                    // A person is at the keyboard. Nothing the model tries
                    // will get through, so stop and say so.
                    Err(HubError::Rpc { code, message })
                        if code == botroster_proto::codes::TAKEN_OVER =>
                    {
                        return self.finish(
                            FinishReason::ComputerBusy { message },
                            step,
                            &messages,
                            spent,
                        )
                    }
                    // Gated, and this connection has no approver at all. A
                    // person saying no is information for the model; an empty
                    // chair is not, and cannot become so during the run.
                    Err(HubError::Rpc { code, message })
                        if code == botroster_proto::codes::APPROVAL_DENIED
                            && self.hub.approvals_are_impossible() =>
                    {
                        return self.finish(
                            FinishReason::NothingApproved {
                                message,
                                tool: name.clone(),
                            },
                            step,
                            &messages,
                            spent,
                        )
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        (false, Value::String(msg.clone()), msg)
                    }
                };

                let _ = events.send(AgentEvent::ToolCallFinished {
                    id: id.clone(),
                    call_id,
                    ok,
                    output,
                    elapsed_ms,
                });

                results.push(Content::ToolResult {
                    id,
                    content: rendered,
                    is_error: !ok,
                });
            }

            messages.push(Message {
                role: Role::User,
                content: results,
            });
        }
    }

    fn finish(
        &self,
        reason: FinishReason,
        steps: u32,
        messages: &[Message],
        usage: Usage,
    ) -> AgentOutcome {
        let text = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.text().is_empty())
            .map(|m| m.text())
            .unwrap_or_default();
        AgentOutcome {
            reason,
            steps,
            text,
            transcript: messages.to_vec(),
            usage,
        }
    }
}

fn model_failed(e: ModelError) -> FinishReason {
    FinishReason::ModelFailed {
        transient: crate::transient::model_failure(&e),
        message: e.to_string(),
    }
}

/// Render a tool result for the model: compact JSON, bounded.
fn render(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unserialisable>".into()),
    };
    if s.chars().count() <= RESULT_CHAR_LIMIT {
        return s;
    }
    let kept: String = s.chars().take(RESULT_CHAR_LIMIT).collect();
    format!("{kept}\n… [truncated at {RESULT_CHAR_LIMIT} characters]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_passes_short_strings_through_unchanged() {
        assert_eq!(render(&Value::String("hi".into())), "hi");
    }

    #[test]
    fn render_compacts_json() {
        let v = serde_json::json!({ "a": 1, "b": [1, 2] });
        assert_eq!(render(&v), r#"{"a":1,"b":[1,2]}"#);
    }

    fn convo(results: usize, chars: usize) -> Vec<Message> {
        let mut m = vec![Message::user("do the thing")];
        for i in 0..results {
            m.push(Message::assistant(vec![Content::ToolUse {
                id: ToolUseId::new(format!("c{i}")),
                name: "fs.read".into(),
                input: json!({ "path": "notes.md" }),
            }]));
            m.push(Message {
                role: Role::User,
                content: vec![Content::ToolResult {
                    id: ToolUseId::new(format!("c{i}")),
                    content: "x".repeat(chars),
                    is_error: false,
                }],
            });
        }
        m
    }

    #[test]
    fn a_conversation_inside_the_budget_is_left_alone() {
        let mut m = convo(3, 100);
        assert_eq!(compact(&mut m, 100_000, 6), 0);
        assert!(matches!(
            &m[2].content[0],
            Content::ToolResult { content, .. } if content.len() == 100
        ));
    }

    #[test]
    fn old_results_are_dropped_and_recent_ones_kept() {
        let mut m = convo(10, 5_000);
        let before = m.len();
        let dropped = compact(&mut m, 20_000, 6);

        assert!(dropped > 0, "nothing was dropped from an oversized run");
        // Messages are never removed: a tool_use without its tool_result is a
        // 400 from every vendor, which is a worse failure than the one being
        // avoided.
        assert_eq!(m.len(), before, "compaction removed messages");

        // The task survives.
        assert_eq!(m[0].text(), "do the thing");

        // The most recent exchanges are untouched.
        let last = m.last().unwrap();
        assert!(matches!(
            &last.content[0],
            Content::ToolResult { content, .. } if content.len() == 5_000
        ));

        // And an early one is gone, with a note the model can act on.
        assert!(matches!(
            &m[2].content[0],
            Content::ToolResult { content, .. } if content.contains("run the tool again")
        ));
    }

    /// Every `tool_use` keeps its matching `tool_result`, in order.
    ///
    /// This is the reason compaction replaces contents instead of removing
    /// messages: a vendor rejects the whole request when the pairing breaks,
    /// so dropping messages would trade a context overflow for a 400, the
    /// same run failing later with a worse error.
    ///
    /// The `&mut [Message]` signature is what enforces it (a slice cannot lose
    /// an element); this test fails if the parameter is widened to a `Vec` and
    /// compaction starts removing.
    #[test]
    fn compaction_never_breaks_a_tool_call_from_its_result() {
        let mut m = convo(12, 5_000);
        let before = m.len();

        let dropped = compact(&mut m, 8_000, 2);
        assert!(
            dropped > 0,
            "nothing was compacted, so the invariant was not exercised"
        );
        assert_eq!(m.len(), before, "compaction removed a message");

        // Walk the conversation the way a vendor does: every tool_use must be
        // answered by a tool_result carrying the same id, in the next message.
        let mut pairs = 0;
        for (i, msg) in m.iter().enumerate() {
            for c in &msg.content {
                let Content::ToolUse { id, .. } = c else {
                    continue;
                };
                let answered = m.get(i + 1).is_some_and(|next| {
                    next.content
                        .iter()
                        .any(|c| matches!(c, Content::ToolResult { id: rid, .. } if rid == id))
                });
                assert!(answered, "`{id:?}` lost its result at message {i}");
                pairs += 1;
            }
        }
        assert_eq!(pairs, 12, "the conversation under test lost its shape");
    }

    #[test]
    fn compacting_twice_does_not_double_count() {
        // The second pass must not "drop" placeholders it wrote itself, or the
        // reported number climbs forever on a long run.
        let mut m = convo(10, 5_000);
        let first = compact(&mut m, 20_000, 6);
        let second = compact(&mut m, 20_000, 6);
        assert!(first > 0);
        assert!(second < first, "re-dropped its own placeholders: {second}");
    }

    #[test]
    fn assistant_reasoning_is_never_compacted() {
        // The thread of the run is what makes the next step coherent; it is
        // also small. Only bulky, re-fetchable results are dropped.
        let mut m = vec![
            Message::user("task"),
            Message::assistant(vec![Content::text("I will read the notes first.")]),
        ];
        m.extend(convo(8, 5_000).into_iter().skip(1));
        compact(&mut m, 10_000, 2);
        assert_eq!(m[1].text(), "I will read the notes first.");
    }

    #[test]
    fn render_truncates_on_a_char_boundary() {
        let v = Value::String("é".repeat(RESULT_CHAR_LIMIT * 2));
        let out = render(&v);
        assert!(out.contains("truncated"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn finish_reason_serialises_with_its_detail() {
        let j = serde_json::to_value(FinishReason::StepLimit { max_steps: 8 }).unwrap();
        assert_eq!(j["kind"], "step_limit");
        assert_eq!(j["max_steps"], 8);
    }
}
