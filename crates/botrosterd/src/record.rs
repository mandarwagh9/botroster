//! What a session did, written by the hub as it happens.
//!
//! # Why this lives in the hub
//!
//! Every tool call passes through `hub::tool_call`, and that is also where
//! policy is evaluated and a person is asked. So the hub is the only place that
//! can write down not just *what the agent did* but *what it was allowed to do,
//! and by whom* — and it is the only place whose record the recorded thing
//! cannot edit. That is the same argument `docs/SPEC.md` §6.0 makes for keeping
//! the gate here rather than in the agent, and it applies unchanged: a record
//! the agent maintains is a record the agent can quietly stop maintaining.
//!
//! # What is deliberately not here
//!
//! **The model exchanges.** Only the agent sees the request it built and the
//! reply it got, so recording them would mean the agent writing into this file
//! and giving away the property above. They are also not needed by the thing
//! this exists for: replaying a Bot's history against a *changed* brief runs
//! the model live, because what it does now is the question being asked.
//!
//! **Sessions with no Bot.** `session_open`'s `bot` is optional, and a session
//! that names none has nobody to attribute its work to. A bare `botroster call`
//! in a script is a one-off, not a teammate's history. It is not recorded, and
//! that is a decision rather than an oversight.
//!
//! # Fidelity
//!
//! A tool result can be a whole file, so values are capped — but a truncated
//! value that does not say it was truncated is a lie in a file whose only
//! purpose is fidelity. Every captured value therefore carries the full byte
//! length and a hash of the *whole* value beside the prefix that was kept. Two
//! different four-megabyte reads must not compare equal because their first
//! four kilobytes match, and that is exactly the comparison
//! `botroster bot test` will make.

use std::sync::Arc;

use botroster_proto::SessionId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How much of a value is kept verbatim.
///
/// Four kilobytes shows a person what happened — a path, a command, the head of
/// a file, an error — without turning the record into a second copy of
/// everything the Bot has ever read. Beyond it, [`Captured::sha256`] is what
/// carries the difference between two values.
pub const KEPT_BYTES: usize = 4096;

/// A value the hub saw, kept in full or in part, and identified either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Captured {
    /// The first [`KEPT_BYTES`] of the canonical JSON, or all of it.
    pub head: String,
    /// How long the whole value was. Equal to `head`'s length when nothing was
    /// cut, which is how a reader tells a complete value from a prefix without
    /// a second field to keep in step.
    pub bytes: usize,
    /// SHA-256 of the **whole** value, always — including when nothing was cut,
    /// so a comparison never has to care which case it is looking at.
    pub sha256: String,
}

impl Captured {
    /// Capture a JSON value, with object keys in a fixed order.
    ///
    /// The same call must always record the same bytes, or the diff this whole
    /// feature exists for reports a divergence because two clients happened to
    /// serialise the same arguments differently.
    ///
    /// Sorted here rather than left to `serde_json`. Its `Map` is a `BTreeMap`
    /// — sorted, for free — *unless* something in the build turns on
    /// `preserve_order`, and something does: Tauri pulls it in through
    /// `json-patch` and `schemars`, and cargo unifies features across the
    /// workspace, so the shipped binaries get insertion order. The first
    /// version of this relied on the `BTreeMap` and passed its own unit test
    /// when that crate was built alone, then failed in the full workspace
    /// build. A property this depends on cannot be a transitive dependency's
    /// feature flag.
    #[must_use]
    pub fn of(value: &serde_json::Value) -> Self {
        let mut out = String::new();
        canonical(value, &mut out);
        Self::of_str(&out)
    }

    /// Capture text that is already serialised, such as an error message.
    #[must_use]
    pub fn of_str(text: &str) -> Self {
        let sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
        // Cut on a character boundary, not a byte one: `String::truncate`
        // panics mid-codepoint, and a tool result is frequently somebody's
        // prose. `floor_char_boundary` is not stable, so walk to it.
        let head = if text.len() <= KEPT_BYTES {
            text.to_owned()
        } else {
            let mut end = KEPT_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_owned()
        };
        Self {
            head,
            bytes: text.len(),
            sha256,
        }
    }

    /// Whether anything was cut.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.head.len() == self.bytes
    }
}

/// Write `value` with every object's keys in sorted order.
///
/// Scalars go through `serde_json` so escaping, number formatting and Unicode
/// are its problem and not this function's; only the *order* of object members
/// is decided here.
///
/// Recursive, and bounded in practice by the parser that produced the value:
/// `serde_json` refuses input nested deeper than 128 while reading it, and
/// every value reaching here arrived as a parsed frame. Nothing constructs a
/// deeper one by hand.
fn canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(k.clone()).to_string());
                out.push(':');
                canonical(&map[k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(v, out);
            }
            out.push(']');
        }
        // Order is meaningless for a scalar, so `serde_json`'s own rendering is
        // both correct and the one every other part of this product uses.
        other => out.push_str(&other.to_string()),
    }
}

/// How a call was decided, and by whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decided {
    /// The policy allowed it with nobody asked.
    Policy,
    /// A person was asked and permitted it. Carries which permission they gave,
    /// because "allow for the session" is a different fact from "allow once"
    /// and only one of them explains the calls that follow.
    Person(String),
    /// The policy refused it outright.
    RefusedByPolicy(String),
    /// A `PreToolUse` hook refused it.
    RefusedByHook(String),
    /// A person was asked and said no, or was never there to answer.
    RefusedByPerson(String),
}

impl Decided {
    /// Whether the call was allowed to run.
    #[must_use]
    pub fn permitted(&self) -> bool {
        matches!(self, Self::Policy | Self::Person(_))
    }
}

/// How a call that ran ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// The tool ran and answered.
    Ok(Captured),
    /// The tool ran and failed, or was never reached. Carries the hub's own
    /// words for it, which include the case where the tool server went away
    /// with the call in flight.
    Failed(Captured),
    /// It never ran, because [`Step::decided`] says it was refused.
    Refused,
}

/// One tool call, as the hub saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Position in this session's record, from 1. Assigned by the writer, not
    /// the caller, so the number always matches the order of the lines: two
    /// concurrent calls finishing in either order still produce a record whose
    /// sequence and whose file agree.
    pub seq: u64,
    pub tool: String,
    pub args: Captured,
    pub decided: Decided,
    pub ended: Ended,
    /// How long the hub held the call, in milliseconds.
    ///
    /// Deliberately not called `elapsed_ms`. The agent measures a number by
    /// that name (`botroster-agent/src/agent.rs`) and it is a different one:
    /// the agent's covers its own dispatch as well, and the hub's includes the
    /// time a person spent looking at an approval dialog. Giving them the same
    /// name in two files would invite a comparison that means nothing.
    pub hub_ms: u64,
}

/// Somewhere to write what a session did.
///
/// A trait for the same reason [`crate::hub::InternalTools`] is one: the hub
/// must not learn where a Bot's files live. The binary supplies an
/// implementation, so the dependency points inward.
pub trait SessionLog: Send + Sync {
    /// Append one step.
    ///
    /// **Must not block.** This is called on the path a tool call takes, inside
    /// the hub's async machinery, so an implementation that writes a file
    /// directly would stall a worker on every call a Bot makes. [`ToBotStore`]
    /// sends on a channel and lets one task do the writing.
    ///
    /// Infallible by signature. A record that cannot be written must not fail
    /// the call it was recording — the work is the point and the record is
    /// evidence about it — so an implementation logs and drops.
    fn record(&self, bot: &str, session: &SessionId, step: StepDraft);
}

/// A step without its sequence number, which only the writer can assign.
#[derive(Debug, Clone)]
pub struct StepDraft {
    pub tool: String,
    pub args: Captured,
    pub decided: Decided,
    pub ended: Ended,
    pub hub_ms: u64,
}

/// Writes the record into a [`botroster_bots::BotStore`].
///
/// One task does every write, fed by an unbounded channel. Two properties fall
/// out of that and both are load-bearing:
///
/// * **The hub never blocks.** Sending on an unbounded channel does not wait,
///   so a slow disk delays the record and not the Bot.
/// * **The file's order is the record's order.** `tool_call` and `finish_relay`
///   run concurrently for concurrent calls, so without a single writer the
///   sequence numbers and the lines could disagree — and a record whose order
///   cannot be trusted is worse than one that lags.
pub struct ToBotStore {
    tx: tokio::sync::mpsc::UnboundedSender<(String, SessionId, StepDraft)>,
}

impl ToBotStore {
    /// Start the writer task.
    #[must_use]
    pub fn spawn(store: Arc<botroster_bots::BotStore>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, SessionId, StepDraft)>();
        tokio::spawn(async move {
            // Per session, so a Bot's first session starts at 1 and a long one
            // keeps counting. Held here rather than in the hub because this is
            // the only place that knows the order the lines actually reach the
            // file.
            let mut seq: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            while let Some((bot, session, draft)) = rx.recv().await {
                let key = format!("{bot}/{session}");
                let n = seq.entry(key).or_insert(0);
                *n += 1;
                let step = Step {
                    seq: *n,
                    tool: draft.tool,
                    args: draft.args,
                    decided: draft.decided,
                    ended: draft.ended,
                    hub_ms: draft.hub_ms,
                };
                let Ok(line) = serde_json::to_string(&step) else {
                    tracing::warn!(tool = %step.tool, "a step would not serialise; not recorded");
                    continue;
                };
                let store = Arc::clone(&store);
                let id = botroster_bots::BotId(bot.clone());
                let session_name = session.as_str().to_owned();
                // `spawn_blocking` because the append is `std::fs`, and awaited
                // because the ordering above is only true if one write finishes
                // before the next begins.
                let wrote = tokio::task::spawn_blocking(move || {
                    store.append_session(&id, &session_name, &line)
                })
                .await;
                match wrote {
                    Ok(Ok(())) => {}
                    // Logged and dropped, never propagated: see `SessionLog`.
                    Ok(Err(e)) => {
                        tracing::warn!(%bot, %session, error = %e, "could not write the record")
                    }
                    Err(e) => {
                        tracing::warn!(%bot, %session, error = %e, "the record writer panicked")
                    }
                }
            }
        });
        Self { tx }
    }
}

impl SessionLog for ToBotStore {
    fn record(&self, bot: &str, session: &SessionId, step: StepDraft) {
        // The receiver is dropped only when this process is going away, so a
        // failure here is a shutdown and not something to report on every call.
        let _ = self.tx.send((bot.to_owned(), session.clone(), step));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value short enough to keep is kept whole, and says so.
    #[test]
    fn a_small_value_is_complete() {
        let c = Captured::of(&serde_json::json!({"path": "notes.md"}));
        assert!(c.is_complete());
        assert_eq!(c.head, r#"{"path":"notes.md"}"#);
        assert_eq!(c.bytes, c.head.len());
    }

    /// Arguments are canonical, so the same call always records the same bytes.
    ///
    /// Without this, `bot test` would compare two records of an identical call
    /// and report a divergence because one client happened to serialise the
    /// keys in a different order.
    #[test]
    fn the_same_arguments_capture_identically_whatever_order_they_arrive_in() {
        let a = Captured::of(&serde_json::json!({"b": 2, "a": 1}));
        let b = Captured::of(&serde_json::json!({"a": 1, "b": 2}));
        assert_eq!(a, b, "key order changed the record of the same call");
        assert_eq!(a.head, r#"{"a":1,"b":2}"#);
    }

    /// Nested objects are sorted too, and arrays keep their order.
    ///
    /// The shallow case above passes for free when `serde_json::Map` is a
    /// `BTreeMap`, which it is in some builds of this workspace and is not in
    /// others — so the shallow case caught the bug but would not have caught a
    /// fix that only sorted the top level. An array's order is data and must
    /// survive.
    #[test]
    fn sorting_reaches_all_the_way_down_and_leaves_arrays_alone() {
        let v = serde_json::json!({
            "z": {"second": 2, "first": 1},
            "a": [3, 1, 2],
        });
        let c = Captured::of(&v);
        assert_eq!(c.head, r#"{"a":[3,1,2],"z":{"first":1,"second":2}}"#);
    }

    /// Escaping and number formatting stay `serde_json`'s job.
    ///
    /// A hand-rolled writer that got either wrong would produce a record that
    /// does not parse, which is worse than one that sorts badly.
    #[test]
    fn awkward_scalars_survive_being_canonicalised() {
        let v = serde_json::json!({
            "quote": "she said \"no\"",
            "newline": "one\ntwo",
            "unicode": "café ☕",
            "big": 9_007_199_254_740_993i64,
            "nothing": serde_json::Value::Null,
        });
        let c = Captured::of(&v);
        let back: serde_json::Value =
            serde_json::from_str(&c.head).expect("a canonical value must parse");
        assert_eq!(back, v, "canonicalising changed the value");
    }

    /// Two long values that share a prefix are still told apart.
    ///
    /// The reason the hash exists. A record that kept only the first four
    /// kilobytes would make every large read of a changed file look unchanged,
    /// which is the one comparison this whole feature is for.
    #[test]
    fn two_long_values_sharing_a_prefix_do_not_compare_equal() {
        let shared = "x".repeat(KEPT_BYTES * 2);
        let a = Captured::of_str(&format!("{shared}ending one"));
        let b = Captured::of_str(&format!("{shared}ending two"));

        assert_eq!(a.head, b.head, "the kept prefixes should be identical here");
        assert_ne!(a.sha256, b.sha256, "the hash did not tell them apart");
        assert_ne!(a, b);
        assert!(!a.is_complete(), "a value twice the cap reads as complete");
    }

    /// The cut lands on a character boundary.
    ///
    /// `String::truncate` panics in the middle of a codepoint, and a tool
    /// result is frequently prose. A record that can panic on a Bot reading a
    /// file with an accent in it is not a record.
    #[test]
    fn a_cut_never_lands_inside_a_character() {
        // Three bytes each, so the cap falls inside one of them.
        let text = "€".repeat(KEPT_BYTES);
        let c = Captured::of_str(&text);
        assert!(!c.is_complete());
        assert!(c.head.len() <= KEPT_BYTES);
        assert!(
            text.starts_with(&c.head),
            "the kept prefix is not a prefix of the value"
        );
    }

    /// The whole value is hashed, not the part that was kept.
    #[test]
    fn the_hash_covers_what_was_cut_away() {
        let long = "y".repeat(KEPT_BYTES * 3);
        let c = Captured::of_str(&long);
        let whole = format!("{:x}", Sha256::digest(long.as_bytes()));
        assert_eq!(c.sha256, whole);
        assert_ne!(
            c.sha256,
            format!("{:x}", Sha256::digest(c.head.as_bytes())),
            "the hash covers only the kept prefix, so it identifies nothing"
        );
    }

    /// A refusal is not a permission, whichever way it was refused.
    #[test]
    fn only_the_two_permitting_decisions_permit() {
        assert!(Decided::Policy.permitted());
        assert!(Decided::Person("allow_once".into()).permitted());
        for refused in [
            Decided::RefusedByPolicy("no".into()),
            Decided::RefusedByHook("no".into()),
            Decided::RefusedByPerson("no".into()),
        ] {
            assert!(!refused.permitted(), "{refused:?} read as permitted");
        }
    }

    /// A step round-trips, so a record written today is readable by the code
    /// that will diff it.
    #[test]
    fn a_step_survives_the_round_trip_through_json() {
        let step = Step {
            seq: 7,
            tool: "fs.write".into(),
            args: Captured::of(&serde_json::json!({"path": "a.md"})),
            decided: Decided::Person("allow_once".into()),
            ended: Ended::Ok(Captured::of_str("wrote 12 bytes")),
            hub_ms: 41,
        };
        let line = serde_json::to_string(&step).expect("a step serialises");
        let back: Step = serde_json::from_str(&line).expect("and parses");
        assert_eq!(step, back);
        assert!(
            !line.contains('\n'),
            "a step spanning two lines would corrupt every record after it"
        );
    }
}
