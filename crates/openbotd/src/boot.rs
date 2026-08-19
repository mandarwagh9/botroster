//! Assembling a hub from a control-plane home.
//!
//! Lives here rather than in `main` because two commands start a hub, `openbotd`
//! and `openbot up`, and they must wire it identically.

use std::path::Path;
use std::sync::Arc;

use crate::connector::{ConnectorTools, Connectors};
use crate::hub::{Hub, InternalTools};
use crate::internal::Composite;
use crate::secrets::SecretStore;

/// What was wired up, for the caller to report.
pub struct Booted {
    pub hub: Arc<Hub>,
    /// Connector tools discovered at startup, by name.
    pub connector_tools: Vec<String>,
    /// How many `PreToolUse` hooks are armed.
    pub hook_count: usize,
    /// How many skills the model can look up.
    pub skill_count: usize,
}

/// Build a hub serving `bot.*` and any configured connectors.
///
/// A malformed connector definition fails the boot rather than being skipped,
/// so a misconfiguration surfaces at startup instead of as a missing tool
/// later. A connector that is merely unreachable is logged and its tools are
/// not offered until it answers.
pub async fn hub_from_home(home: &Path, policy: crate::policy::Policy) -> anyhow::Result<Booted> {
    let bots = Arc::new(openbot_bots::BotStore::open(home)?);
    let mut sources: Vec<Arc<dyn InternalTools>> =
        vec![Arc::new(crate::bot_tools::BotTools::new(bots))];

    // Skills are procedures the model looks up, not code that runs. The
    // provider is always registered and reads the directory on each catalog
    // call, so a skill written after boot is offered without a restart.
    let skills = crate::skills::Skills::load(home);
    let skill_count = skills.len();
    sources.push(Arc::new(skills));

    // One store, shared between connectors and the hub. `SecretStore` keeps
    // only a path and re-reads on every access, so a second instance would
    // behave identically; sharing one avoids the question of which wins.
    let secrets = Arc::new(SecretStore::open(home)?);

    let mut connector_tools = Vec::new();
    let connectors = Connectors::load(home)?;
    if !connectors.connectors.is_empty() {
        let tools = ConnectorTools::discover(connectors.connectors, Arc::clone(&secrets)).await;
        connector_tools = tools.tool_names();
        sources.push(Arc::new(tools));
    }

    // The credential store lets a Bot request a credential it does not have
    // instead of asking for it in conversation, which would put the value in
    // the model's context and in the log on disk. Always attached, not only
    // when a connector exists: a new connector's first need is a token.
    let mut hub = Hub::with_policy(policy)
        .with_internal_tools(Arc::new(Composite::new(sources)))
        .with_secrets(secrets);

    // `PreToolUse` hooks, if any are configured. A malformed hooks file fails
    // the boot: starting anyway would run unguarded while the operator
    // believes otherwise.
    let hooks = crate::hooks::Hooks::load(home)?;
    let hook_count = hooks.hooks.len();
    if !hooks.is_empty() {
        hub = hub.with_hooks(Arc::new(hooks));
    }

    Ok(Booted {
        hub: Arc::new(hub),
        connector_tools,
        hook_count,
        skill_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bare_home_still_serves_the_bot_tools() {
        let d = tempfile::tempdir().unwrap();
        let b = hub_from_home(d.path(), crate::policy::Policy::default())
            .await
            .unwrap();
        assert!(b.connector_tools.is_empty());
        // The hub is usable with no configuration at all.
        assert_eq!(b.hub.inflight_calls().await, 0);
    }

    #[tokio::test]
    async fn a_skill_written_after_boot_is_offered_without_a_restart() {
        // The skills provider must be registered even when the directory is
        // empty at boot; otherwise a skill written later is invisible until
        // restart.
        let d = tempfile::tempdir().unwrap();
        let b = hub_from_home(d.path(), crate::policy::Policy::default())
            .await
            .unwrap();

        let skill_tools = |b: &Booted| {
            b.hub
                .internal_catalog()
                .iter()
                .filter(|t| t.tool_id.as_str().starts_with("skill."))
                .count()
        };
        // Nothing to look up, so nothing is offered: tools that can only
        // answer "nothing" waste context.
        assert_eq!(
            skill_tools(&b),
            0,
            "skill tools offered on an install with no skills"
        );

        let dir = d.path().join("skills").join("refund-a-customer");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---
name: refund-a-customer
description: how to issue a refund
---

Ask first.
",
        )
        .unwrap();

        assert_eq!(
            skill_tools(&b),
            2,
            "a skill written while the hub runs is not offered"
        );
    }

    /// A real boot offers `secret.request`.
    ///
    /// The hub advertises the tool only when a store is attached, and the
    /// unit tests of the feature build their own `Hub` with `with_secrets`.
    /// This test covers the path a shipped binary takes, `hub_from_home`, on
    /// a bare home: the store is attached even when no connector exists.
    #[tokio::test]
    async fn a_bare_boot_can_ask_a_person_for_a_credential() {
        let d = tempfile::tempdir().unwrap();
        let b = hub_from_home(d.path(), crate::policy::Policy::default())
            .await
            .unwrap();
        let offered: Vec<_> = b
            .hub
            .internal_catalog()
            .iter()
            .map(|t| t.tool_id.as_str().to_owned())
            .collect();
        assert!(
            offered.iter().any(|t| t == "secret.request"),
            "a Bot booted from a bare home cannot request a credential: {offered:?}"
        );
    }

    #[tokio::test]
    async fn a_broken_connector_definition_fails_the_boot() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("connectors.json"),
            r#"{"connectors":[{"id":"bad__id","url":"https://x.invalid/mcp",
                "authorization":"Bearer ${t}"}]}"#,
        )
        .unwrap();
        // Failing at startup beats a tool that silently does not exist later.
        assert!(hub_from_home(d.path(), crate::policy::Policy::default())
            .await
            .is_err());
    }
}
