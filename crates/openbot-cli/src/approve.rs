//! The approval card and how a person answers it.
//!
//! This is the one screen where ambiguity can cost someone their data, so the
//! card shows the real arguments rather than a summary, names the reason, and
//! makes deny (not allow) the easiest answer to give by accident.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use openbot_agent::ApprovalHandler;
use openbot_proto::approval::{
    ApprovalDecision, ApprovalRequestParams, Decision, SecretRequestParams,
};
use serde_json::Value;

use crate::render::Style;

/// What to do when the hub asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ApproveMode {
    /// Show the card and wait for a person. Requires a terminal.
    Ask,
    /// Approve everything without asking. For runs where the operator has
    /// explicitly accepted the risk.
    Auto,
    /// Refuse everything gated.
    Deny,
}

/// Resolve the mode actually used, given whether anyone is watching.
///
/// `ask` without a terminal cannot ask, and an unattended request resolves to
/// deny, not allow.
pub fn resolve(mode: ApproveMode, interactive: bool) -> ApproveMode {
    match mode {
        ApproveMode::Ask if !interactive => ApproveMode::Deny,
        other => other,
    }
}

pub fn handler(mode: ApproveMode) -> Arc<dyn ApprovalHandler> {
    match resolve(
        mode,
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
    ) {
        ApproveMode::Ask => Arc::new(TtyApprover { s: Style::detect() }),
        ApproveMode::Auto => Arc::new(openbot_agent::AllowAll),
        ApproveMode::Deny => Arc::new(openbot_agent::DenyAll),
    }
}

struct TtyApprover {
    s: Style,
}

#[async_trait::async_trait]
impl ApprovalHandler for TtyApprover {
    async fn decide(&self, req: &ApprovalRequestParams) -> ApprovalDecision {
        let card = render_card(self.s, req);
        // Reading a line blocks; keep it off the async runtime's worker.
        let answer = tokio::task::spawn_blocking(move || {
            print!("{card}");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => None, // EOF: nobody is there.
                Ok(_) => Some(line),
                Err(_) => None,
            }
        })
        .await
        .unwrap_or(None);

        let decision = match answer
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
        {
            Some(a) if a == "y" || a == "yes" => Decision::AllowOnce,
            Some(a) if a == "a" || a == "always" => Decision::AllowAlways,
            // Everything else is a refusal, including a bare Enter. The
            // default answer to "may I change something" is no.
            _ => Decision::Deny,
        };
        println!();
        ApprovalDecision {
            decision,
            note: None,
        }
    }

    async fn supply(&self, req: &SecretRequestParams) -> Option<String> {
        let card = render_secret_card(self.s, req);
        let typed = tokio::task::spawn_blocking(move || {
            print!("{card}");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(line),
                Err(_) => None,
            }
        })
        .await
        .unwrap_or(None);
        println!();
        typed_secret(typed.as_deref())
    }
}

/// Turn what a person typed into a credential, or a refusal.
///
/// A bare Enter is a refusal, as is EOF (nobody is there): the default answer
/// to "hand me a credential" is no.
///
/// Only trailing whitespace is removed. A token containing a space is taken as
/// typed; a client that silently rewrites a credential produces one that fails
/// later against a service with nothing on this side to explain why. This is
/// `trim_end` rather than `trim` because leading space is far more likely to
/// be part of a pasted value than an accident.
///
/// Kept separate from the read so the decision is testable outside the
/// `spawn_blocking` closure.
fn typed_secret(line: Option<&str>) -> Option<String> {
    line.map(|t| t.trim_end().to_owned())
        .filter(|t| !t.trim().is_empty())
}

/// The card for a credential request.
///
/// A person is being asked to hand over a secret, so the card must say what
/// the secret is for and what happens to it. The second point is the whole
/// argument of the broker (the Bot can use the value and can never read it),
/// and a prompt that omitted it would be asking for trust in a mechanism the
/// person has never been told about.
///
/// It also states that the typing will be visible, for the same reason
/// `read_secret_from_stdin` does: there is no terminal-echo control here, and
/// a prompt that implied masking it does not do is worse than one that says
/// so.
fn render_secret_card(s: Style, req: &SecretRequestParams) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "  {} {}\n",
        s.yellow("⚠"),
        s.bold("a credential is needed")
    ));
    out.push_str(&format!("  {} {}\n", s.dim("│"), s.dim(&req.why)));
    out.push_str(&format!("  {}\n", s.dim("│")));
    out.push_str(&format!(
        "  {} {}\n",
        s.dim("│"),
        s.dim("stored for the Bot to use — it is never shown the value")
    ));
    out.push_str(&format!(
        "  {} {}\n",
        s.dim("│"),
        s.dim("what you type will be visible on screen")
    ));
    out.push_str(&format!("  {}\n", s.dim("│")));
    // The credential is named on the prompt line, not several lines above it.
    // At the moment of pasting a person is looking at the cursor, and "paste
    // it" there requires remembering which `it`. Naming it here also matches
    // the desktop client's layout: reason above, label on the input itself.
    out.push_str(&format!(
        "  {} {}{} ",
        s.dim("› paste"),
        s.cyan(&req.name),
        s.dim(", or press Enter to refuse:")
    ));
    out
}

fn render_card(s: Style, req: &ApprovalRequestParams) -> String {
    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "  {} {}\n",
        s.yellow("⚠"),
        s.bold("approval needed")
    ));
    out.push_str(&format!(
        "  {} {}\n",
        s.dim("│"),
        s.cyan(req.tool_id.as_str())
    ));
    out.push_str(&format!("  {} {}\n", s.dim("│"), s.dim(&req.reason)));
    out.push_str(&format!("  {}\n", s.dim("│")));

    for (k, v) in flatten(&req.args) {
        out.push_str(&format!(
            "  {} {} {}\n",
            s.dim("│"),
            s.dim(&format!("{k:<10}")),
            v
        ));
    }

    out.push_str(&format!("  {}\n", s.dim("│")));
    out.push_str(&format!(
        "  {} {}  {}  {}\n",
        s.dim("│"),
        s.green("[y] allow once"),
        s.cyan("[a] always allow this tool"),
        s.red("[n] deny"),
    ));
    out.push_str(&format!("  {} ", s.bold("›")));
    out
}

/// Flatten the arguments into label/value lines.
///
/// Values are shown whole, wrapped rather than truncated: an approval card is
/// the one place where hiding part of the input defeats the purpose. Only an
/// absurdly large value is cut, and it says so.
fn flatten(args: &Value) -> Vec<(String, String)> {
    const MAX: usize = 2000;
    match args {
        // An empty object must produce no lines. Without this arm it falls
        // through to the catch-all and renders a meaningless `args {}` row.
        Value::Object(m) if m.is_empty() => vec![],
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| {
                let rendered = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let rendered = if rendered.chars().count() > MAX {
                    let kept: String = rendered.chars().take(MAX).collect();
                    format!(
                        "{kept}\n… [{} more characters]",
                        rendered.chars().count() - MAX
                    )
                } else {
                    rendered
                };
                (k.clone(), rendered)
            })
            .collect(),
        Value::Null => vec![],
        other => vec![("args".into(), other.to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openbot_proto::approval::ApprovalId;
    use openbot_proto::{ToolCallId, ToolId};
    use serde_json::json;

    fn req(args: Value) -> ApprovalRequestParams {
        ApprovalRequestParams {
            approval_id: ApprovalId::new("a1"),
            call_id: ToolCallId::new("c1"),
            tool_id: ToolId::new("shell.exec"),
            args,
            reason: "runs a shell command".into(),
            timeout_secs: 120,
        }
    }

    #[test]
    fn ask_without_a_terminal_becomes_deny_not_allow() {
        assert_eq!(resolve(ApproveMode::Ask, false), ApproveMode::Deny);
        assert_eq!(resolve(ApproveMode::Ask, true), ApproveMode::Ask);
        // An explicit choice is honoured either way.
        assert_eq!(resolve(ApproveMode::Auto, false), ApproveMode::Auto);
        assert_eq!(resolve(ApproveMode::Deny, true), ApproveMode::Deny);
    }

    #[test]
    fn the_card_shows_every_argument_in_full() {
        let fields = flatten(&json!({ "command": "rm -rf /tmp/x", "timeout_secs": 30 }));
        let map: std::collections::HashMap<_, _> = fields.into_iter().collect();
        assert_eq!(map["command"], "rm -rf /tmp/x");
        assert_eq!(map["timeout_secs"], "30");
    }

    #[test]
    fn an_absurd_value_is_cut_but_says_so() {
        let big = "x".repeat(5000);
        let fields = flatten(&json!({ "contents": big }));
        assert!(fields[0].1.contains("more characters"));
    }

    #[test]
    fn empty_arguments_produce_no_field_lines() {
        assert!(flatten(&json!({})).is_empty());
        assert!(flatten(&Value::Null).is_empty());
    }

    #[test]
    fn the_card_names_the_tool_and_the_reason() {
        let card = render_card(Style::plain(), &req(json!({"command": "ls"})));
        assert!(card.contains("shell.exec"));
        assert!(card.contains("runs a shell command"));
        assert!(card.contains("command"));
        assert!(card.contains("ls"));
        // All three answers must be visible; a hidden option is not an option.
        assert!(card.contains("[y]") && card.contains("[a]") && card.contains("[n]"));
    }

    /// `--approve auto` must not supply a credential.
    ///
    /// Auto-approving is an operator accepting a risk out of band: the actions
    /// were going to happen and somebody decided not to be asked each time.
    /// There is no equivalent for a secret, because there is no value a
    /// handler could invent, so the default `supply` refuses and the modes
    /// that answer approvals without a person inherit that.
    #[tokio::test]
    async fn no_unattended_mode_hands_over_a_credential() {
        let req = SecretRequestParams {
            name: "linear-token".into(),
            why: "to file the issue".into(),
            timeout_secs: 5,
        };
        for (what, h) in [
            (
                "auto",
                Arc::new(openbot_agent::AllowAll) as Arc<dyn ApprovalHandler>,
            ),
            ("deny", Arc::new(openbot_agent::DenyAll)),
        ] {
            assert!(
                h.supply(&req).await.is_none(),
                "`--approve {what}` supplied a credential nobody was asked for"
            );
        }
    }

    /// Refusing must be as easy as answering, and a value must arrive whole.
    ///
    /// Three claims, one per row: a bare Enter is a refusal, an absent person
    /// is a refusal, and a credential is stored exactly as typed apart from the
    /// newline the terminal added. A token silently trimmed fails against a
    /// service later with nothing here to explain why.
    #[test]
    fn a_typed_credential_is_taken_whole_and_a_bare_enter_is_a_refusal() {
        for (typed, expect, why) in [
            (
                Some("sk-live-abc\n"),
                Some("sk-live-abc"),
                "the newline stays",
            ),
            (
                Some("sk-live-abc\r\n"),
                Some("sk-live-abc"),
                "so does a CRLF",
            ),
            (Some("\n"), None, "a bare Enter is not a credential"),
            (Some("   \n"), None, "nor is a space bar"),
            (Some(""), None, "nor is an empty read"),
            (None, None, "nobody was there to ask"),
            // The value as typed, not a tidied-up version of it.
            (
                Some("sk-live with spaces\n"),
                Some("sk-live with spaces"),
                "an inner space is part of the value",
            ),
            (
                Some(" leading-is-kept\n"),
                Some(" leading-is-kept"),
                "leading space is likelier pasted than accidental",
            ),
        ] {
            assert_eq!(
                typed_secret(typed).as_deref(),
                expect,
                "{why} (given {typed:?})"
            );
        }
    }

    /// The card has to say what it is for and what happens to the value.
    ///
    /// A person is being asked to paste a secret into a terminal. The two
    /// things that make that reasonable (it is for a named purpose, and the
    /// Bot will never see it) are the two things a prompt can omit while still
    /// looking complete.
    #[test]
    fn the_card_says_what_it_is_for_and_where_it_goes() {
        let card = render_secret_card(
            Style::plain(),
            &SecretRequestParams {
                name: "linear-token".into(),
                why: "to file the issue you asked for".into(),
                timeout_secs: 300,
            },
        );
        // Named once, on the line the cursor sits on.
        assert_eq!(
            card.matches("linear-token").count(),
            1,
            "the credential is named more than once: {card}"
        );
        let prompt = card.lines().last().unwrap_or_default();
        assert!(
            prompt.contains("linear-token"),
            "the prompt line does not say what to paste: {prompt:?}"
        );
        assert!(card.contains("to file the issue you asked for"), "{card}");
        assert!(
            card.contains("never shown the value"),
            "the prompt does not say the Bot cannot read it: {card}"
        );
        assert!(
            card.contains("visible on screen"),
            "the prompt implies a masking it does not do: {card}"
        );
        assert!(
            card.contains("Enter to refuse"),
            "there is no way out of the prompt: {card}"
        );
    }
}
