//! Approval policy: decide, before anything runs, whether a tool call needs a
//! person.
//!
//! Evaluated in the hub, never in the harness (`docs/SPEC.md` §6.0). Three
//! verdicts, ordered: deny beats ask beats allow. A permissive rule can never
//! widen a restrictive one, so adding an `allow` can only reduce prompts,
//! never safety.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it. Nobody is asked.
    Allow,
    /// Stop and ask a person. Carries the reason they will be shown.
    Ask(String),
    /// Refuse outright. No person is asked, because the answer is already no.
    Deny(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    /// Stop and ask the person who owns the session.
    ///
    /// `ask` is accepted as well, and is not a courtesy alias: the product uses
    /// both words for this and a person can only guess which one a given
    /// surface wants. `botroster run --approve ask`, `routine tick --approve ask`
    /// and the approval dialog all say ask; only the rules file said
    /// `require_approval`, and the README's own example said `ask` and was
    /// rejected by the parser that reads it. Writing the word the rest of the
    /// product taught you is not a mistake worth an error.
    ///
    /// `require_approval` stays canonical, so it is what serialises and what
    /// `permission ls` prints, and a file written either way reads back the
    /// same.
    #[serde(alias = "ask")]
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub action: Action,
    /// Glob over the tool id: `fs.read`, `fs.*`, `*`.
    pub tool: String,
    /// Optional narrowing on one argument, e.g. `("path", "/etc/*")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<ArgMatch>,
    /// Shown to the approver. Write these as the reason a person would give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgMatch {
    pub key: String,
    pub glob: String,
}

impl Rule {
    pub fn allow(tool: &str) -> Self {
        Self {
            action: Action::Allow,
            tool: tool.into(),
            when: None,
            reason: None,
        }
    }
    pub fn ask(tool: &str, reason: &str) -> Self {
        Self {
            action: Action::RequireApproval,
            tool: tool.into(),
            when: None,
            reason: Some(reason.into()),
        }
    }
    pub fn deny(tool: &str, reason: &str) -> Self {
        Self {
            action: Action::Deny,
            tool: tool.into(),
            when: None,
            reason: Some(reason.into()),
        }
    }
    pub fn when(mut self, key: &str, glob: &str) -> Self {
        self.when = Some(ArgMatch {
            key: key.into(),
            glob: glob.into(),
        });
        self
    }

    fn matches(&self, tool: &str, args: &Value) -> bool {
        if !glob_match(&self.tool, tool) {
            return false;
        }
        match &self.when {
            None => true,
            Some(m) => args
                .get(&m.key)
                .and_then(|v| v.as_str())
                .map(|v| glob_match(&m.glob, v))
                // A rule that narrows on an argument the call does not carry
                // does not match. Treating a missing argument as a match would
                // make `deny fs.write when path=/etc/*` fire on every write.
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<Rule>,
    /// Verdict for a call no rule matches.
    pub fallback: Action,
    /// Tools the person approved with "always allow" for the lifetime of the session.
    ///
    /// Held separately from `rules` rather than appended to them, because ask
    /// outranks allow: an extra allow rule would never lift the gate it was
    /// meant to lift. A grant is checked after deny and before ask, which is
    /// the precedence a person means by "always allow": it overrides the
    /// prompt, never an outright refusal.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub grants: BTreeSet<String>,
}

impl Default for Policy {
    /// The shipped default: reads are free, changes ask.
    ///
    /// Least privilege by default: start read-only, review the output, then
    /// widen intentionally. An agent that can silently write files and run
    /// shell commands on its first run is not a safe default.
    fn default() -> Self {
        Self {
            rules: vec![
                Rule::allow("fs.read"),
                Rule::allow("fs.list"),
                Rule::ask("fs.write", "writes a file into the workspace"),
                Rule::ask("shell.exec", "runs a shell command on the computer"),
                // Reading the web is browsing; acting on a page is not.
                Rule::allow("browser.open"),
                Rule::allow("browser.read"),
                Rule::allow("browser.links"),
                // Same class as `read`: it describes the page the Bot already
                // has open and changes nothing. It also has to be `allow` for
                // the acting tools to be usable at all — `click` and `fill`
                // are `ask`, and a snapshot that prompted would mean two
                // approvals per interaction, one of them for looking.
                Rule::allow("browser.snapshot"),
                Rule::allow("browser.screenshot"),
                // The live viewer's frame stream. Same risk class as a
                // screenshot (pixels of a page the agent already opened), and
                // prompting for each frame would make the viewer unusable
                // rather than safe.
                Rule::allow("browser.frame"),
                Rule::ask("browser.click", "clicks something on a live web page"),
                Rule::ask("browser.fill", "types into a form on a live web page"),
                // The viewer's input tools, which the guest also offers to a
                // Bot. Explicit rules rather than the fallback, so the
                // approval card says what will happen instead of "no rule
                // covers `browser.type`".
                Rule::ask("browser.click_at", "clicks a point on a live web page"),
                Rule::ask("browser.type", "types into a live web page"),
                Rule::ask("browser.key", "presses a key on a live web page"),
                // Scrolling reads further down a page the Bot already has
                // open. Same class as `browser.read` and the frame stream, and
                // prompting for each scroll would make reading a long page a
                // conversation about scrolling.
                Rule::allow("browser.scroll"),
                // Reading the roster is harmless; putting work in someone
                // else's queue is not.
                Rule::allow("bot.list"),
                Rule::ask("bot.send", "hands work to another Bot"),
                // Asking a person for a credential is not an action to
                // approve; it is the approval. Every other rule here gates
                // something that happens to the world once permitted;
                // `secret.request` only puts a refusable question in front of
                // a person, naming both the credential and the Bot's reason
                // for wanting it. Falling to the `RequireApproval` fallback
                // would cost two prompts for one decision, the first strictly
                // less informative than the second.
                //
                // A rule, not a carve-out in the enforcement path, so an
                // operator who disagrees still wins: deny short-circuits and
                // ask outranks allow, held by
                // `an_operators_own_rule_still_governs_credential_requests`.
                // It is also visible in `botroster policy ls`, which an invisible
                // exemption would not be.
                Rule::allow("secret.request"),
            ],
            fallback: Action::RequireApproval,
            grants: BTreeSet::new(),
        }
    }
}

impl Policy {
    /// A policy that approves everything. For non-interactive runs where the
    /// operator has accepted the risk explicitly; never a default.
    pub fn allow_all() -> Self {
        Self {
            rules: vec![Rule::allow("*")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
        }
    }

    /// Precedence, in order: deny, then a session grant, then ask, then
    /// allow, then the fallback.
    pub fn evaluate(&self, tool: &str, args: &Value) -> Verdict {
        let mut ask: Option<String> = None;
        let mut allowed = false;

        for r in self.rules.iter().filter(|r| r.matches(tool, args)) {
            match r.action {
                // Deny short-circuits: nothing later can rescue it, including
                // a grant. "Always allow" must never override an outright ban.
                Action::Deny => {
                    return Verdict::Deny(
                        r.reason
                            .clone()
                            .unwrap_or_else(|| format!("`{tool}` is denied by policy")),
                    )
                }
                Action::RequireApproval => {
                    ask.get_or_insert_with(|| {
                        r.reason
                            .clone()
                            .unwrap_or_else(|| format!("`{tool}` requires approval"))
                    });
                }
                Action::Allow => allowed = true,
            }
        }

        // The person already answered "always" for this tool earlier in the session.
        if self.grants.contains(tool) {
            return Verdict::Allow;
        }

        // Ask outranks allow, so a broad `allow *` cannot silently swallow a
        // narrow `require approval`.
        if let Some(reason) = ask {
            return Verdict::Ask(reason);
        }
        if allowed {
            return Verdict::Allow;
        }
        match self.fallback {
            Action::Allow => Verdict::Allow,
            Action::RequireApproval => {
                Verdict::Ask(format!("no rule covers `{tool}`; asking to be safe"))
            }
            Action::Deny => Verdict::Deny(format!("no rule covers `{tool}`")),
        }
    }

    /// Record an "always allow" answer for the rest of the session.
    ///
    /// Session-scoped and in-memory by design: a decision made in a hurry to
    /// unblock one task should not silently become permanent policy.
    pub fn allow_from_now_on(&mut self, tool: &str) {
        self.grants.insert(tool.to_owned());
    }
}

/// Glob with a single `*` wildcard, matching any run of characters.
///
/// Intentionally not a regex: policy rules are security-relevant and are read
/// far more often than they are written, so the matching must be obvious to
/// someone skimming them.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((head, tail)) = pattern.split_once('*') else {
        return pattern == value;
    };
    if !value.starts_with(head) {
        return false;
    }
    let rest = &value[head.len()..];
    // Multiple wildcards: recurse on the remainder.
    if tail.contains('*') {
        return (0..=rest.len()).any(|i| glob_match(tail, &rest[i..]));
    }
    rest.len() >= tail.len() && rest.ends_with(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn globs_match_literally_and_with_a_wildcard() {
        assert!(glob_match("fs.read", "fs.read"));
        assert!(!glob_match("fs.read", "fs.write"));
        assert!(glob_match("fs.*", "fs.write"));
        assert!(!glob_match("fs.*", "shell.exec"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.exec", "shell.exec"));
        assert!(glob_match("fs.*e", "fs.write"));
        assert!(!glob_match("fs.*e", "fs.read"));
    }

    #[test]
    fn the_default_policy_allows_reads_and_asks_before_changes() {
        let p = Policy::default();
        assert_eq!(p.evaluate("fs.read", &json!({})), Verdict::Allow);
        assert_eq!(p.evaluate("fs.list", &json!({})), Verdict::Allow);
        assert!(matches!(
            p.evaluate("fs.write", &json!({})),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn an_unknown_tool_asks_rather_than_running() {
        let p = Policy::default();
        // A tool with no rule is the case where guessing is worst.
        assert!(matches!(
            p.evaluate("email.send", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn deny_beats_ask_and_allow_regardless_of_order() {
        let p = Policy {
            rules: vec![
                Rule::allow("shell.exec"),
                Rule::ask("shell.exec", "asks"),
                Rule::deny("shell.exec", "no shell on this account"),
            ],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
        };
        match p.evaluate("shell.exec", &json!({})) {
            Verdict::Deny(r) => assert_eq!(r, "no shell on this account"),
            other => panic!("deny must win, got {other:?}"),
        }

        // ...and the same with the deny listed first.
        let p2 = Policy {
            rules: vec![Rule::deny("shell.exec", "no"), Rule::allow("shell.exec")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
        };
        assert!(matches!(
            p2.evaluate("shell.exec", &json!({})),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn a_broad_allow_cannot_swallow_a_narrow_require() {
        // The common misconfiguration: `allow *` added to stop prompts must
        // not silently disable every approval gate.
        let p = Policy {
            rules: vec![Rule::allow("*"), Rule::ask("shell.exec", "still asks")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
        };
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
        assert_eq!(p.evaluate("fs.read", &json!({})), Verdict::Allow);
    }

    #[test]
    fn argument_narrowing_applies_only_when_the_argument_is_present() {
        let p = Policy {
            rules: vec![
                Rule::allow("fs.write"),
                Rule::deny("fs.write", "not into /etc").when("path", "/etc/*"),
            ],
            fallback: Action::Deny,
            grants: BTreeSet::new(),
        };
        assert!(matches!(
            p.evaluate("fs.write", &json!({"path": "/etc/passwd"})),
            Verdict::Deny(_)
        ));
        assert_eq!(
            p.evaluate("fs.write", &json!({"path": "notes.md"})),
            Verdict::Allow
        );
        // No `path` at all: the narrowed rule must not fire.
        assert_eq!(p.evaluate("fs.write", &json!({})), Verdict::Allow);
    }

    #[test]
    fn allow_all_is_available_but_explicit() {
        let p = Policy::allow_all();
        assert_eq!(p.evaluate("shell.exec", &json!({})), Verdict::Allow);
        assert_eq!(p.evaluate("whatever.new", &json!({})), Verdict::Allow);
    }

    #[test]
    fn always_allow_actually_lifts_the_gate() {
        // Pushing an allow rule would not work, because ask outranks allow.
        // A grant has to be its own tier.
        let mut p = Policy::default();
        assert!(matches!(
            p.evaluate("fs.write", &json!({})),
            Verdict::Ask(_)
        ));
        p.allow_from_now_on("fs.write");
        assert_eq!(p.evaluate("fs.write", &json!({})), Verdict::Allow);
        // Other tools are unaffected.
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn a_grant_never_overrides_an_outright_deny() {
        let mut p = Policy {
            rules: vec![Rule::deny("shell.exec", "no shell on this account")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
        };
        p.allow_from_now_on("shell.exec");
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn grants_are_idempotent() {
        let mut p = Policy::default();
        p.allow_from_now_on("fs.write");
        p.allow_from_now_on("fs.write");
        assert_eq!(p.grants.len(), 1);
    }

    #[test]
    fn a_policy_round_trips_through_json() {
        let p = Policy::default();
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["rules"][0]["action"], "allow");
        assert_eq!(j["fallback"], "require_approval");
        assert_eq!(serde_json::from_value::<Policy>(j).unwrap(), p);
    }

    /// One decision, one prompt.
    ///
    /// If `secret.request` fell to the `RequireApproval` fallback, a person
    /// answering a credential request would answer twice: an approval card
    /// asking whether the Bot may ask, then the box naming the credential and
    /// the reason. The first question is strictly less informative than the
    /// second.
    #[test]
    fn asking_a_person_for_a_credential_is_not_itself_gated() {
        assert_eq!(
            Policy::default().evaluate("secret.request", &json!({"name":"linear-token"})),
            Verdict::Allow,
            "a credential request costs two prompts"
        );
        // Not a blanket exemption: an unknown tool still meets the fallback.
        assert!(matches!(
            Policy::default().evaluate("something.new", &json!({})),
            Verdict::Ask(_)
        ));
    }

    /// An operator's own rule still governs it, in both directions.
    ///
    /// This is why the default is a rule rather than a carve-out in the
    /// enforcement path: precedence already handles disagreement, and a rule
    /// is visible in `botroster policy ls`.
    #[test]
    fn an_operators_own_rule_still_governs_credential_requests() {
        let mut p = Policy::default();
        p.rules.push(Rule::ask(
            "secret.request",
            "this Bot must not collect tokens",
        ));
        assert!(
            matches!(p.evaluate("secret.request", &json!({})), Verdict::Ask(r) if r.contains("must not collect")),
            "an operator's `ask` did not outrank the shipped allow"
        );

        let mut p = Policy::default();
        p.rules
            .push(Rule::deny("secret.request", "no credentials from this Bot"));
        assert!(
            matches!(p.evaluate("secret.request", &json!({})), Verdict::Deny(_)),
            "an operator's `deny` did not beat the shipped allow"
        );
    }
}
