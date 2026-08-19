//! Approval frames.
//!
//! Enforcement lives in the hub, never in the harness (SPEC §6.0). The harness
//! is a client: it runs wherever the user runs it and can be modified, so a
//! check inside it is a check the caller can delete. The hub therefore asks
//! the harness for a decision and refuses the call itself if the answer is no
//! or never arrives.
//!
//! ```text
//! harness            hub                      tool server
//!   │ tool.call ────► evaluate policy
//!   │            ◄─── approval.request        (only when the verdict is Ask)
//!   │ decision ─────► allow → tool_call_request ──────►│
//!   │            ◄─── deny  → error APPROVAL_DENIED
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ToolCallId, ToolId};

/// Identifies one approval exchange.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApprovalId(pub String);

impl ApprovalId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hub → harness. Everything a person needs to judge the action without
/// further lookup: what will run, against what, and why they are being asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequestParams {
    pub approval_id: ApprovalId,
    pub call_id: ToolCallId,
    pub tool_id: ToolId,
    /// The exact arguments that will be used. Never a summary: a person
    /// cannot approve what they cannot see.
    pub args: Value,
    /// Why approval is required, in plain language, naming the rule that
    /// matched so the decision is auditable.
    pub reason: String,
    /// Seconds before the hub gives up and denies.
    pub timeout_secs: u64,
}

/// Harness → hub, as the response to [`ApprovalRequestParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub decision: Decision,
    /// Optional note from the person, carried into the audit record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Permit this one call. Nothing is remembered.
    AllowOnce,
    /// Permit this call and remember a matching rule for the session.
    AllowAlways,
    Deny,
}

impl Decision {
    pub fn permits(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

impl ApprovalDecision {
    pub fn allow_once() -> Self {
        Self {
            decision: Decision::AllowOnce,
            note: None,
        }
    }
    pub fn deny() -> Self {
        Self {
            decision: Decision::Deny,
            note: None,
        }
    }
    pub fn with_note(mut self, n: impl Into<String>) -> Self {
        self.note = Some(n.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_use_stable_wire_names() {
        assert_eq!(
            serde_json::to_value(Decision::AllowOnce).unwrap(),
            "allow_once"
        );
        assert_eq!(
            serde_json::to_value(Decision::AllowAlways).unwrap(),
            "allow_always"
        );
        assert_eq!(serde_json::to_value(Decision::Deny).unwrap(), "deny");
    }

    #[test]
    fn only_allow_variants_permit() {
        assert!(Decision::AllowOnce.permits());
        assert!(Decision::AllowAlways.permits());
        assert!(!Decision::Deny.permits());
    }

    #[test]
    fn a_decision_without_a_note_omits_the_field() {
        let j = serde_json::to_value(ApprovalDecision::allow_once()).unwrap();
        assert!(j.get("note").is_none());
    }
}

// ── carrying a credential request over ACP ──

/// The `_meta` key a credential request travels under, in both directions.
///
/// ACP has no free-text prompt. The only request an agent can make of a client
/// is `session/request_permission`, answered with the id of an option the
/// person clicked, so the protocol proper has nowhere to carry typed input.
/// `_meta` is the extension point ACP reserves for this: the request carries
/// `{name, why}` under this key and the answer carries `{value}` back under it.
///
/// Defined in the wire-types crate because it is a wire contract between two
/// crates that never link against each other (`openbot acp` is a separate
/// process the client spawns). Two independent declarations, one per side,
/// would compile and pass every test on both sides, then silently diverge the
/// moment either was renamed: the agent would tag a request the client no
/// longer recognised, the client would render an ordinary approval whose
/// "Provide credential" button supplies nothing, and the Bot would be refused.
/// A single definition removes that failure mode instead of testing for it.
///
/// Namespaced because `_meta` is shared with every other extension a client
/// implements.
pub const SECRET_META: &str = "openbot/secret_request";

/// The option id a client sends back when the person supplied a credential.
///
/// Same reasoning as [`SECRET_META`]: it crosses a process boundary, so it has
/// exactly one definition.
pub const SECRET_PROVIDE: &str = "provide-secret";

// ── asking a person for a credential ──

/// Hub → harness: a Bot needs a credential it does not have.
///
/// A Bot that needs a token cannot be given one by the model, since the broker
/// exists so that the guest can use a credential without ever reading one. The
/// hub therefore asks the person directly, over the same channel and with the
/// same fail-closed timeout as an approval.
///
/// The value that comes back is stored by the control plane. It does not
/// return to the caller, and [`SecretStoredResult`] is shaped so that it
/// cannot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRequestParams {
    /// What the credential will be stored as, and what a connector references.
    pub name: String,
    /// Why it is needed, in the Bot's own words. Shown to the person, who is
    /// being asked to hand over a secret and deserves to know what for.
    pub why: String,
    /// Seconds before the hub gives up. A request nobody answers is refused,
    /// like an approval.
    pub timeout_secs: u64,
}

/// Harness → hub, as the response to [`SecretRequestParams`].
///
/// `None` is a refusal: the person declined or closed the prompt. It is
/// intentionally not distinguished from a timeout: both mean no credential,
/// and a Bot that could tell them apart would learn something about the person
/// rather than about the task.
///
/// `Debug` is implemented by hand and prints `[redacted]`. This is the only
/// protocol type that carries a credential, and it must keep `Serialize`
/// because it is the wire format, so the usual wrapper,
/// `openbotd::secrets::Secret`, cannot be used. That leaves `Debug` as the
/// remaining leak path: secrets escape when a struct holding one derives
/// `Debug` and is later folded into an error, a log line, or a panic message
/// by code unaware of what it carries. Held by
/// `a_supplied_credential_is_redacted_from_debug_but_still_serialises`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRequestResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl std::fmt::Debug for SecretRequestResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Whether a credential was supplied is not itself a secret, and it is
        // the useful bit in a log: it separates "the person refused" from
        // "the person answered and something later went wrong".
        f.debug_struct("SecretRequestResult")
            .field(
                "value",
                match &self.value {
                    Some(_) => &"[redacted]",
                    None => &"None",
                },
            )
            .finish()
    }
}

/// What the model is told after a credential is stored.
///
/// The value is intentionally not a field. A tool result travels back through
/// the hub into the agent's conversation, which is written to disk and
/// rendered in every client, so a result able to carry the credential would
/// put it in both. This type makes that unexpressible rather than merely
/// absent.
///
/// The fingerprint is the same non-reversible hint `secret ls` shows, so a Bot
/// can tell two credentials apart without learning either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretStoredResult {
    pub name: String,
    pub fingerprint: String,
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    /// The one protocol type that carries a credential does not print it.
    ///
    /// It cannot use `openbotd::secrets::Secret`, which intentionally does not
    /// serialise, so `Debug` is the remaining leak path and this test keeps it
    /// closed. Deriving `Debug` again fails this test.
    #[test]
    fn a_supplied_credential_is_redacted_from_debug_but_still_serialises() {
        const VALUE: &str = "sk-live-NEVER-IN-A-PANIC-MESSAGE";
        let supplied = SecretRequestResult {
            value: Some(VALUE.into()),
        };

        let printed = format!("{supplied:?}");
        assert!(
            !printed.contains(VALUE),
            "the credential printed: {printed}"
        );
        assert!(
            !printed.contains("sk-live"),
            "a prefix of the credential printed: {printed}"
        );
        // Still indicates whether a value was present: the difference between
        // a refusal and a later failure is what a log reader needs.
        assert!(printed.contains("[redacted]"), "{printed}");

        // Nested inside another type's derived `Debug`, which is how it would
        // typically reach a log.
        let nested = format!("{:?}", Some(vec![supplied.clone()]));
        assert!(!nested.contains(VALUE), "leaked when nested: {nested}");

        // The wire format is untouched: this is the response the hub parses,
        // and redacting it there would break the feature.
        let wire = serde_json::to_string(&supplied).expect("serialises");
        assert!(
            wire.contains(VALUE),
            "the value must still cross the wire: {wire}"
        );
        assert_eq!(
            serde_json::from_str::<SecretRequestResult>(&wire).expect("round trips"),
            supplied
        );

        // A refusal reads as one.
        let refused = format!("{:?}", SecretRequestResult { value: None });
        assert!(refused.contains("None"), "{refused}");
        assert!(
            !refused.contains("redacted"),
            "a refusal is not a secret: {refused}"
        );
    }
}
