//! Translating OPENBOT's vocabulary into ACP's.
//!
//! ACP is a published protocol (`docs/SPEC.md` §9) and OPENBOT is the Agent
//! side of it. Most of the adapter is transport. This module is the part that
//! is not: OPENBOT knows more about how a turn ended than ACP has words for,
//! and every place a richer vocabulary is squashed into a poorer one is a
//! place where a client is quietly told something untrue.
//!
//! `FinishReason` has more variants than `StopReason`. The mapping is
//! therefore a set of decisions, not a lookup table, and each decision is
//! documented next to the code that makes it.

pub mod serve;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Meta, PermissionOption, PermissionOptionId, PermissionOptionKind,
    SessionUpdate, StopReason, TextContent, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields,
};
use openbot_agent::model::{Content, Message, Role};
use openbot_agent::FinishReason;
use openbot_proto::approval::{ApprovalDecision, Decision};

/// How a prompt turn ended, in terms ACP can carry.
///
/// The split matters more than either arm. `session/prompt` returns a
/// `StopReason`, and every `StopReason` means the turn ran and then stopped,
/// so answering a provider outage with one tells the client the work finished
/// when it did not. Failures become a JSON-RPC error instead, which is the
/// only thing in the protocol that means "this did not happen".
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    /// The turn genuinely ended. `detail` is what ACP has no field for.
    Stopped {
        reason: StopReason,
        detail: Option<String>,
    },
    /// The turn broke. Belongs in an error response, not a stop reason.
    Failed { message: String, transient: bool },
}

/// Translate OPENBOT's reason for stopping into ACP's.
///
/// Where a `detail` is returned, it is because ACP's vocabulary is lossy at
/// that point and the client needs the real reason. It travels in the
/// reserved `_meta` object rather than being dropped; see `docs/SPEC.md` §9.
pub fn turn_end(reason: &FinishReason) -> TurnEnd {
    let stopped = |r: StopReason| TurnEnd::Stopped {
        reason: r,
        detail: None,
    };
    let stopped_because = |r: StopReason, why: String| TurnEnd::Stopped {
        reason: r,
        detail: Some(why),
    };

    match reason {
        FinishReason::Completed => stopped(StopReason::EndTurn),

        // Exact: ACP's own gloss for `max_turn_requests` is "maximum model
        // requests per turn exceeded", which is what a step budget is.
        FinishReason::StepLimit { max_steps } => stopped_because(
            StopReason::MaxTurnRequests,
            format!("stopped after {max_steps} steps"),
        ),

        // Both are "the turn ran out of room", which is what MaxTokens means.
        // The provider truncating and OPENBOT hitting a budget are different
        // events, so the distinction ACP cannot carry goes in the detail.
        FinishReason::Truncated => stopped_because(
            StopReason::MaxTokens,
            "the provider truncated the turn".into(),
        ),
        FinishReason::TokenBudget { spent, budget } => stopped_because(
            StopReason::MaxTokens,
            format!("spent {spent} of a {budget} token budget"),
        ),

        // Not a stop reason. A model or hub failure means the turn did not
        // happen, and there is no StopReason that says so: `end_turn` would
        // claim it completed and `refusal` would blame the agent for someone
        // else's outage. `transient` survives because it is the difference
        // between "try again in a minute" and "give up", and it was decided
        // upstream where the typed error still existed.
        FinishReason::ModelFailed { message, transient } => TurnEnd::Failed {
            message: message.clone(),
            transient: *transient,
        },
        FinishReason::HubFailed { message } => TurnEnd::Failed {
            message: message.clone(),
            transient: false,
        },

        // `Declined` is the one case where `Refusal` is a judgement about the
        // request rather than an absence on the client's side. For
        // `NothingApproved`, nobody was there to approve anything: `refusal`
        // is the nearest ACP has (the agent did decline to continue), but the
        // reason is an absence on the client's side, and a client showing
        // "the agent refused" without that would be misleading.
        FinishReason::Declined => stopped_because(
            StopReason::Refusal,
            "the model declined this request".to_owned(),
        ),
        FinishReason::NothingApproved { message, tool } => stopped_because(
            StopReason::Refusal,
            format!(
                "{message}; {}",
                crate::render::nothing_approved_advice(tool)
            ),
        ),

        // A person took the computer. `cancelled` is ACP's word for a turn
        // ended from outside rather than by the model, which is what a
        // takeover is, even though ACP imagines it arriving as
        // `session/cancel` rather than someone grabbing the mouse.
        FinishReason::ComputerBusy { message } => {
            stopped_because(StopReason::Cancelled, message.clone())
        }

        // Needs no translation. ACP's `cancelled` means exactly this, and it
        // is the answer a client expects after its own `session/cancel`; a
        // stop button that reports anything else looks like it did not work.
        FinishReason::Cancelled { message } => {
            stopped_because(StopReason::Cancelled, message.clone())
        }
    }
}

/// The choices OPENBOT offers a human when a call needs approval.
///
/// ACP's model is that the agent supplies the options and the client renders
/// them, so this list is OPENBOT's decision vocabulary rather than the
/// client's idea of one.
///
/// Answering here does not permit anything. The hub enforces (`docs/SPEC.md`
/// §6.0), and a client selecting `allow_always` cannot satisfy a call that a
/// hub `deny` rule forbids. This is how the human is asked, not where the
/// decision takes effect.
///
/// # Why `reject_always` is not offered
///
/// ACP has four option kinds; OPENBOT's [`Decision`] has three: `AllowOnce`,
/// `AllowAlways` and a single `Deny`. There is no "deny always", because a
/// durable refusal is a policy rule in the hub, not a decision a harness makes
/// about one call.
///
/// A button reading "Never allow this tool" that in fact declines exactly
/// once would misrepresent what happens, discovered when the model asks again
/// a minute later. The option can be added when a durable deny rule can be
/// written from here, and not before.
pub fn permission_options() -> Vec<PermissionOption> {
    vec![
        option("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
        option(
            "allow-always",
            "Allow for the rest of this session",
            PermissionOptionKind::AllowAlways,
        ),
        option(
            "reject-once",
            "Not this time",
            PermissionOptionKind::RejectOnce,
        ),
    ]
}

/// Turn the option a person clicked back into OPENBOT's decision.
///
/// `None` for anything not offered by [`permission_options`]. That is a real
/// case rather than a defensive one: `Selected(option_id)` arrives as a string
/// from another process, and a client that echoes back an id OPENBOT never
/// sent (or a stale one from a previous protocol version) must not be guessed
/// at. The caller denies, because the design fails closed (`docs/SPEC.md` §6).
///
/// The note is what the person's click meant, in OPENBOT's own words, so the
/// audit record says why a call was permitted rather than only that it was.
pub fn decision(option_id: &str) -> Option<ApprovalDecision> {
    let d = |decision: Decision, note: &str| {
        Some(ApprovalDecision {
            decision,
            note: Some(note.to_owned()),
        })
    };
    match option_id {
        "allow-once" => d(Decision::AllowOnce, "allowed once from an ACP client"),
        "allow-always" => d(
            Decision::AllowAlways,
            "allowed for the session from an ACP client",
        ),
        "reject-once" => d(Decision::Deny, "declined from an ACP client"),
        _ => None,
    }
}

pub(crate) fn option(id: &'static str, name: &str, kind: PermissionOptionKind) -> PermissionOption {
    // `PermissionOption` is `#[non_exhaustive]`: it is built through the
    // constructor so that a field added upstream is a compile error here
    // rather than a silently missing value on the wire.
    PermissionOption::new(PermissionOptionId::new(id), name, kind)
}

/// Replay a Bot's stored conversation as `session/update` notifications.
///
/// This is what `session/load` is for, and OPENBOT needs it more than most
/// agents do. A Bot's conversation is the durable thing, while an ACP session
/// is an ephemeral name for it. Without a replay, a client that reconnects
/// shows an empty transcript beside a Bot that remembers everything.
///
/// Order is the contract. A transcript replayed out of order is worse than no
/// transcript: it reads as a conversation that did not happen. Messages come
/// back oldest-first and each message's content is emitted in place.
///
/// Symmetry with the live stream is the rule: the transcript must read the
/// same whether it was watched live or reopened. So a replayed call carries
/// its arguments the way a live one does, and a replayed result carries its
/// status and output the way a live `ToolCallFinished` does.
#[must_use]
pub fn replay(history: &[Message]) -> Vec<SessionUpdate> {
    let mut out = Vec::new();
    for message in history {
        for content in &message.content {
            match content {
                Content::Text { text } if text.trim().is_empty() => {}
                Content::Text { text } => {
                    let chunk =
                        ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())));
                    out.push(match message.role {
                        Role::User => SessionUpdate::UserMessageChunk(chunk),
                        Role::Assistant => SessionUpdate::AgentMessageChunk(chunk),
                    });
                }
                Content::ToolUse { id, name, input } => {
                    // Completed, because it is: this is history. Sending it as
                    // `InProgress` would put a permanent spinner in a
                    // transcript of work that finished days ago.
                    out.push(SessionUpdate::ToolCall(
                        ToolCall::new(ToolCallId::new(id.0.clone()), name.clone())
                            .status(ToolCallStatus::Completed)
                            .raw_input(input.clone()),
                    ));
                }
                Content::ToolResult {
                    id,
                    content,
                    is_error,
                } => {
                    // The stored form is already-rendered text rather than the
                    // structured output the live stream carries, so it travels
                    // as a JSON string.
                    let fields = ToolCallUpdateFields::default()
                        .status(if *is_error {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        })
                        .raw_output(serde_json::Value::String(content.clone()));
                    out.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                        ToolCallId::new(id.0.clone()),
                        fields,
                    )));
                }
            }
        }
    }
    out
}

/// The key OPENBOT claims inside ACP's `_meta`.
const META_NAMESPACE: &str = "openbot";

/// The group a client asked for by name, if it asked.
///
/// Same namespace and same rules as [`requested_bot`], and the two are
/// mutually exclusive at the caller: a session answers in one conversation.
/// A client naming both is asking for something that does not exist, so the
/// Bot wins; it is the narrower request, and the one an editor means.
#[must_use]
pub fn requested_group(meta: Option<&Meta>) -> Option<String> {
    let name = meta?
        .get(META_NAMESPACE)?
        .get("group")?
        .as_str()?
        .trim()
        .to_owned();
    (!name.is_empty()).then_some(name)
}

/// The Bot a client asked for by name, if it asked.
///
/// ACP addresses a session by working directory, which is the right shape for
/// an editor: open a project, get its Bot. A desktop client has a roster
/// instead (choosing a Bot from a sidebar, not choosing a folder), and those
/// two are not the same request.
///
/// `_meta` is the protocol's own answer to this. The schema reserves it for
/// implementations to "attach additional metadata to their interactions", so
/// `{"_meta": {"openbot": {"bot": "Talent Scout"}}}` is an extension rather than
/// a private method, and a client that sends nothing sees no change at all.
///
/// Never an error. The same schema note says implementations "MUST NOT make
/// assumptions about values at these keys", so anything unexpected here
/// degrades to `None` and the caller falls back to the directory. Refusing a
/// session because another client put its own bookkeeping in `_meta` would be
/// reading a shared namespace as if OPENBOT owned it.
#[must_use]
pub fn requested_bot(meta: Option<&Meta>) -> Option<String> {
    let name = meta?
        .get(META_NAMESPACE)?
        .get("bot")?
        .as_str()?
        .trim()
        .to_owned();
    // A Bot named "" or "   " is not a request, it is a mistake, and it would
    // otherwise create a nameless Bot nobody can pick out of a list.
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop_of(r: FinishReason) -> StopReason {
        match turn_end(&r) {
            TurnEnd::Stopped { reason, .. } => reason,
            TurnEnd::Failed { message, .. } => panic!("expected a stop, got failure: {message}"),
        }
    }

    #[test]
    fn a_completed_turn_is_an_end_turn() {
        assert_eq!(stop_of(FinishReason::Completed), StopReason::EndTurn);
    }

    #[test]
    fn a_step_budget_is_max_turn_requests_and_says_how_many() {
        let end = turn_end(&FinishReason::StepLimit { max_steps: 12 });
        let TurnEnd::Stopped { reason, detail } = end else {
            panic!("a step limit is a stop, not a failure");
        };
        assert_eq!(reason, StopReason::MaxTurnRequests);
        assert!(
            detail.expect("the step count must be kept").contains("12"),
            "the number a person needs in order to raise the budget was dropped"
        );
    }

    #[test]
    fn running_out_of_room_is_max_tokens_either_way() {
        assert_eq!(stop_of(FinishReason::Truncated), StopReason::MaxTokens);
        assert_eq!(
            stop_of(FinishReason::TokenBudget {
                spent: 900,
                budget: 1000
            }),
            StopReason::MaxTokens
        );
    }

    /// The two that ACP cannot tell apart must still be distinguishable.
    #[test]
    fn truncation_and_a_budget_do_not_read_as_the_same_event() {
        let a = turn_end(&FinishReason::Truncated);
        let b = turn_end(&FinishReason::TokenBudget {
            spent: 900,
            budget: 1000,
        });
        assert_ne!(
            a, b,
            "both became MaxTokens with nothing to tell them apart"
        );
    }

    /// An outage is not a completed turn.
    #[test]
    fn a_model_failure_is_not_reported_as_a_finished_turn() {
        let end = turn_end(&FinishReason::ModelFailed {
            message: "429 from the provider".into(),
            transient: true,
        });
        match end {
            TurnEnd::Failed { message, transient } => {
                assert!(message.contains("429"));
                assert!(transient, "transience decided upstream was thrown away");
            }
            TurnEnd::Stopped { reason, .. } => {
                panic!("an outage was reported as {reason:?}: the client is being told the work finished")
            }
        }
    }

    #[test]
    fn a_hub_failure_is_a_failure_too_and_is_not_assumed_retryable() {
        match turn_end(&FinishReason::HubFailed {
            message: "connection reset".into(),
        }) {
            TurnEnd::Failed { transient, .. } => assert!(
                !transient,
                "nothing established that a hub failure clears up on its own"
            ),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// `NothingApproved` reads as the agent refusing unless the reason
    /// survives, and the reason is that nobody was there to ask.
    #[test]
    fn an_unattended_run_explains_itself_rather_than_looking_like_a_refusal() {
        let end = turn_end(&FinishReason::NothingApproved {
            message: "no approval handler is attached".into(),
            tool: "fs.write".into(),
        });
        let TurnEnd::Stopped { reason, detail } = end else {
            panic!("an absent approver is a stop, not a crash");
        };
        assert_eq!(reason, StopReason::Refusal);
        assert!(
            detail.is_some_and(|d| d.contains("approval")),
            "a bare `refusal` blames the agent for the operator's absence"
        );
    }

    #[test]
    fn a_takeover_is_cancelled_and_says_who_cancelled_it() {
        let end = turn_end(&FinishReason::ComputerBusy {
            message: "a person is driving the computer".into(),
        });
        let TurnEnd::Stopped { reason, detail } = end else {
            panic!("a takeover is a stop");
        };
        assert_eq!(reason, StopReason::Cancelled);
        assert!(detail.is_some_and(|d| d.contains("person")));
    }

    #[test]
    fn every_permission_option_kind_is_offered_exactly_once() {
        let opts = permission_options();
        // Three, not ACP's four: there is no durable deny to put behind
        // `reject_always`. The number follows what OPENBOT can honour, not what
        // the protocol has room for.
        let mut kinds: Vec<_> = opts.iter().map(|o| format!("{:?}", o.kind)).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(
            kinds.len(),
            opts.len(),
            "the same kind is offered twice, so two buttons do the same thing"
        );
        // Ids are what come back in `Selected(option_id)`, so a duplicate
        // would make the answer ambiguous.
        let mut ids: Vec<_> = opts.iter().map(|o| o.option_id.0.to_string()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            opts.len(),
            "two options share an id; the answer is ambiguous"
        );
    }

    /// Every id OPENBOT puts on the wire has to come back as something it can
    /// act on. A rename on one side and not the other turns every approval
    /// into a silent denial, which looks like a cautious agent rather than a
    /// bug.
    #[test]
    fn every_option_offered_maps_back_to_a_decision() {
        for o in permission_options() {
            let id = o.option_id.0.to_string();
            assert!(
                decision(&id).is_some(),
                "offered `{id}` and cannot interpret the answer"
            );
        }
    }

    #[test]
    fn the_two_allow_options_actually_permit_and_the_reject_does_not() {
        assert_eq!(
            decision("allow-once").unwrap().decision,
            Decision::AllowOnce
        );
        assert_eq!(
            decision("allow-always").unwrap().decision,
            Decision::AllowAlways
        );
        assert!(decision("allow-always").unwrap().decision.permits());
        assert_eq!(decision("reject-once").unwrap().decision, Decision::Deny);
        assert!(!decision("reject-once").unwrap().decision.permits());
    }

    /// An id OPENBOT never sent is not a decision. It arrives as a string from
    /// another process, and guessing at it is how a fail-closed design stops
    /// being one.
    #[test]
    fn an_id_that_was_never_offered_is_not_interpreted() {
        for stranger in ["allow", "reject-always", "", "ALLOW-ONCE", "allow_once"] {
            assert!(
                decision(stranger).is_none(),
                "`{stranger}` was treated as a real answer"
            );
        }
    }

    /// Offering it would be a button that does not do what it says: ACP has
    /// `reject_always`, OPENBOT has no durable deny to back it with.
    #[test]
    fn reject_always_is_not_offered_while_openbot_cannot_honour_it() {
        let kinds: Vec<_> = permission_options()
            .iter()
            .map(|o| format!("{:?}", o.kind))
            .collect();
        assert!(
            !kinds.iter().any(|k| k.contains("RejectAlways")),
            "offering `Never allow this tool` promises a durable rule that \
             openbot's Decision enum cannot express"
        );
    }

    #[test]
    fn every_decision_openbot_can_make_is_reachable_from_some_option() {
        // The other direction: a decision no button produces is a capability
        // the person cannot actually use.
        let made: Vec<Decision> = permission_options()
            .iter()
            .filter_map(|o| decision(&o.option_id.0))
            .map(|d| d.decision)
            .collect();
        for want in [Decision::AllowOnce, Decision::AllowAlways, Decision::Deny] {
            assert!(
                made.contains(&want),
                "no offered option produces {want:?}, so a person cannot make that choice"
            );
        }
    }

    #[test]
    fn a_decision_carries_a_note_so_the_audit_record_says_why() {
        let d = decision("allow-always").expect("offered");
        assert!(
            d.note.is_some_and(|n| n.contains("session")),
            "an audit trail that records only `allowed` cannot be reviewed"
        );
    }

    // ---- requested_bot ----

    fn meta(value: serde_json::Value) -> Meta {
        match value {
            serde_json::Value::Object(m) => m.into_iter().collect(),
            _ => panic!("meta is an object"),
        }
    }

    #[test]
    fn a_client_that_asks_for_nothing_gets_the_directory_behaviour() {
        assert_eq!(requested_bot(None), None);
        assert_eq!(requested_bot(Some(&meta(serde_json::json!({})))), None);
    }

    #[test]
    fn a_client_can_ask_for_a_bot_by_name() {
        let m = meta(serde_json::json!({ "openbot": { "bot": "Talent Scout" } }));
        assert_eq!(requested_bot(Some(&m)), Some("Talent Scout".to_owned()));
    }

    /// `_meta` is a shared namespace and the schema says implementations must
    /// not assume anything about it. Somebody else's bookkeeping must not
    /// break a session.
    #[test]
    fn another_implementations_metadata_is_not_read_as_ours() {
        for other in [
            serde_json::json!({ "zed": { "bot": "not ours" } }),
            serde_json::json!({ "openbot": "a string, not an object" }),
            serde_json::json!({ "openbot": { "something-else": 1 } }),
            serde_json::json!({ "openbot": { "bot": 42 } }),
            serde_json::json!({ "openbot": { "bot": null } }),
        ] {
            assert_eq!(
                requested_bot(Some(&meta(other.clone()))),
                None,
                "{other} should fall back to the directory, not be refused"
            );
        }
    }

    /// A blank name would create a Bot nobody can pick out of a sidebar.
    #[test]
    fn a_blank_name_is_a_mistake_not_a_request() {
        for blank in ["", "   ", "	"] {
            let m = meta(serde_json::json!({ "openbot": { "bot": blank } }));
            assert_eq!(requested_bot(Some(&m)), None, "{blank:?} is not a name");
        }
    }

    #[test]
    fn a_name_is_taken_without_the_whitespace_around_it() {
        let m = meta(serde_json::json!({ "openbot": { "bot": "  Expense Manager  " } }));
        assert_eq!(
            requested_bot(Some(&m)),
            Some("Expense Manager".to_owned()),
            "a padded name would sit beside its own twin in the sidebar"
        );
    }

    #[test]
    fn a_client_can_ask_for_a_group_by_name() {
        let m = meta(serde_json::json!({ "openbot": { "group": "Website Launch" } }));
        assert_eq!(requested_group(Some(&m)), Some("Website Launch".to_owned()));
        assert_eq!(requested_bot(Some(&m)), None, "a group is not a Bot");
    }

    /// A session answers in one conversation. Asking for both is asking for
    /// something that does not exist, and the Bot is the narrower request.
    #[test]
    fn asking_for_a_bot_and_a_group_resolves_to_the_bot() {
        let m = meta(serde_json::json!({ "openbot": { "bot": "Scout", "group": "Launch" } }));
        assert_eq!(requested_bot(Some(&m)), Some("Scout".to_owned()));
        assert_eq!(requested_group(Some(&m)), Some("Launch".to_owned()));
        // The precedence itself lives at the call site in `serve.rs`; this
        // pins that both are readable so it has something to choose between.
    }

    #[test]
    fn a_blank_group_name_is_not_a_request() {
        let m = meta(serde_json::json!({ "openbot": { "group": "  " } }));
        assert_eq!(requested_group(Some(&m)), None);
    }

    // ---- replay ----

    fn said(update: &SessionUpdate) -> Option<(&'static str, String)> {
        match update {
            SessionUpdate::UserMessageChunk(c) => match &c.content {
                ContentBlock::Text(t) => Some(("user", t.text.clone())),
                _ => None,
            },
            SessionUpdate::AgentMessageChunk(c) => match &c.content {
                ContentBlock::Text(t) => Some(("agent", t.text.clone())),
                _ => None,
            },
            SessionUpdate::ToolCall(c) => Some(("tool", c.title.clone())),
            _ => None,
        }
    }

    #[test]
    fn nothing_to_replay_is_not_an_error() {
        assert!(replay(&[]).is_empty());
    }

    /// Who said what, in the order they said it.
    #[test]
    fn a_conversation_replays_in_the_order_it_happened() {
        let history = vec![
            Message::user("first"),
            Message::assistant(vec![Content::text("second")]),
            Message::user("third"),
            Message::assistant(vec![Content::text("fourth")]),
        ];
        let got: Vec<_> = replay(&history).iter().filter_map(said).collect();
        assert_eq!(
            got,
            vec![
                ("user", "first".to_owned()),
                ("agent", "second".to_owned()),
                ("user", "third".to_owned()),
                ("agent", "fourth".to_owned()),
            ],
            "a transcript out of order reads as a conversation that did not happen"
        );
    }

    /// A replayed tool call is history, not work in progress. `InProgress`
    /// would leave a permanent spinner in a transcript of a finished turn.
    #[test]
    fn a_replayed_tool_call_is_not_still_running() {
        let history = vec![Message::assistant(vec![Content::ToolUse {
            id: openbot_agent::ToolUseId::new("t1"),
            name: "fs.write".to_owned(),
            input: serde_json::json!({ "path": "notes.md" }),
        }])];
        let updates = replay(&history);
        let SessionUpdate::ToolCall(call) = &updates[0] else {
            panic!("expected a tool call, got {:?}", updates[0]);
        };
        assert_eq!(call.status, ToolCallStatus::Completed);
        assert_eq!(call.title, "fs.write", "the title is what a person reads");
        assert!(
            call.raw_input
                .as_ref()
                .is_some_and(|v| v["path"] == "notes.md"),
            "a replayed tool call with no arguments says less than the live one did"
        );
    }

    /// A result is replayed the way the live stream sends one: a status and
    /// the output, on the call it belongs to. The rule is symmetry: the
    /// transcript must read the same live and on reopen.
    #[test]
    fn a_replayed_result_says_what_a_live_one_says() {
        let history = vec![Message::assistant(vec![Content::ToolResult {
            id: openbot_agent::ToolUseId::new("t1"),
            content: "wrote notes.md".to_owned(),
            is_error: false,
        }])];
        let updates = replay(&history);
        let SessionUpdate::ToolCallUpdate(update) = &updates[0] else {
            panic!("expected a tool call update, got {:?}", updates[0]);
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
        assert_eq!(
            update.fields.raw_output,
            Some(serde_json::Value::String("wrote notes.md".to_owned()))
        );
    }

    /// A tool that failed must still look failed on replay. Replaying every
    /// result as completed would rewrite history into a run where nothing
    /// went wrong.
    #[test]
    fn a_replayed_failure_is_still_a_failure() {
        let history = vec![Message::assistant(vec![Content::ToolResult {
            id: openbot_agent::ToolUseId::new("t1"),
            content: "no such file".to_owned(),
            is_error: true,
        }])];
        let SessionUpdate::ToolCallUpdate(update) = &replay(&history)[0] else {
            panic!("expected a tool call update");
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
    }

    /// Empty assistant turns are real (a turn that only called a tool stores
    /// no text), and an empty bubble in the transcript is a rendering bug.
    #[test]
    fn empty_text_is_not_replayed_as_an_empty_bubble() {
        let history = vec![
            Message::assistant(vec![Content::text("")]),
            Message::assistant(vec![Content::text("   ")]),
            Message::user("real"),
        ];
        let got: Vec<_> = replay(&history).iter().filter_map(said).collect();
        assert_eq!(got, vec![("user", "real".to_owned())]);
    }

    /// One message can hold several pieces. All of them belong to the speaker
    /// of that message, in order.
    #[test]
    fn every_piece_of_a_message_is_replayed_under_its_own_speaker() {
        let history = vec![Message::assistant(vec![
            Content::text("about to write"),
            Content::ToolUse {
                id: openbot_agent::ToolUseId::new("t1"),
                name: "fs.write".to_owned(),
                input: serde_json::json!({}),
            },
            Content::text("done"),
        ])];
        let got: Vec<_> = replay(&history).iter().filter_map(said).collect();
        assert_eq!(
            got,
            vec![
                ("agent", "about to write".to_owned()),
                ("tool", "fs.write".to_owned()),
                ("agent", "done".to_owned()),
            ]
        );
    }

    #[test]
    fn no_permission_option_is_unlabelled() {
        for o in permission_options() {
            assert!(
                !o.name.trim().is_empty(),
                "an unlabelled button is unclickable"
            );
        }
    }
}
