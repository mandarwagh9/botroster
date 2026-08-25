//! Permission rules, for the settings surface.
//!
//! Settings > General > Auto-review: rules that always stop a matching action,
//! rules that let one proceed, and "Require Approval wins" when both match.
//! The engine enforces exactly that ordering (deny beats require-approval
//! beats allow), so a permissive rule can never widen a restrictive one. This
//! module and `botroster permission` are the write side of that store.
//!
//! Rules are read when the hub boots. Adding one here does not change a
//! running hub, and the window must say so rather than let a person believe a
//! rule is live when it is not; a security control that appears to be on is
//! worse than one that is visibly off.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// What a rule does to a matching call.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Run it. Nobody is asked.
    Allow,
    /// Stop and ask a person.
    RequireApproval,
    /// Refuse outright. No person is asked, because the answer is already no.
    Deny,
}

/// Narrowing on one argument, e.g. `path` matching `/etc/*`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct When {
    pub key: String,
    pub glob: String,
}

/// One rule, as `botroster permission ls --json` prints it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub action: Action,
    /// Glob over the tool id: `fs.read`, `fs.*`, `*`.
    pub tool: String,
    #[serde(default)]
    pub when: Option<When>,
    /// What a person is shown when this rule stops or refuses a call.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Every rule this home configures, in the order they are written.
///
/// # Errors
/// If the binary cannot be run, or a rule cannot be parsed. An unparseable
/// rule is a refusal rather than a skip, because a silently dropped rule
/// leaves a person believing something is forbidden when it is not.
pub async fn list(botroster: &Path, home: &Path) -> anyhow::Result<Vec<Rule>> {
    let out = base(botroster, home)
        .arg("ls")
        .arg("--json")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim()).map_err(|e| {
        anyhow::anyhow!("`botroster permission ls --json` answered something else: {e}")
    })
}

/// Add a rule.
///
/// `reason` is required for anything that stops or refuses a call, and the
/// binary enforces that too: it is what the person reads in the approval
/// prompt and what the log records afterwards.
///
/// # Errors
/// If the binary cannot be run or refuses the rule.
pub async fn add(botroster: &Path, home: &Path, rule: &Rule) -> anyhow::Result<()> {
    let mut cmd = base(botroster, home);
    cmd.arg("add")
        .arg("--action")
        .arg(match rule.action {
            Action::Allow => "allow",
            Action::RequireApproval => "ask",
            Action::Deny => "deny",
        })
        .arg("--tool")
        .arg(&rule.tool);
    if let Some(when) = &rule.when {
        cmd.arg("--when-key")
            .arg(&when.key)
            .arg("--when-glob")
            .arg(&when.glob);
    }
    if let Some(reason) = &rule.reason {
        cmd.arg("--reason").arg(reason);
    }
    run(cmd, botroster).await
}

/// Remove the rule at this position in [`list`], counting from one.
///
/// # Errors
/// If the binary cannot be run or there is no such rule.
pub async fn remove(botroster: &Path, home: &Path, number: usize) -> anyhow::Result<()> {
    let mut cmd = base(botroster, home);
    cmd.arg("rm").arg(number.to_string());
    run(cmd, botroster).await
}

async fn run(mut cmd: tokio::process::Command, botroster: &Path) -> anyhow::Result<()> {
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    if !out.status.success() {
        // The binary's own message already explains what is wrong with the
        // rule.
        return Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn base(botroster: &Path, home: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("permission")
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape is a contract with `botroster permission ls --json`, which
    /// serialises botrosterd's own `Rule`. Either end moving alone is the bug.
    #[test]
    fn the_rule_shape_matches_what_the_binary_prints() {
        let json = r#"[{"action":"deny","tool":"shell.exec","reason":"read-only account"},
                       {"action":"allow","tool":"fs.*"},
                       {"action":"require_approval","tool":"fs.write","reason":"writes a file",
                        "when":{"key":"path","glob":"/etc/*"}}]"#;
        let rules: Vec<Rule> = serde_json::from_str(json).expect("the rules parse");
        assert_eq!(rules[0].action, Action::Deny);
        assert_eq!(rules[0].reason.as_deref(), Some("read-only account"));
        assert_eq!(rules[1].action, Action::Allow);
        assert!(rules[1].reason.is_none(), "an allow interrupts nobody");
        assert_eq!(rules[2].action, Action::RequireApproval);
        let when = rules[2].when.as_ref().expect("the narrowing");
        assert_eq!(when.key, "path");
        assert_eq!(when.glob, "/etc/*");
    }
}
