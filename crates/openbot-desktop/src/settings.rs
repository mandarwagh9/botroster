//! What is wired up: connected apps, and work that runs on a schedule.
//!
//! Both are read-only here. A connector is installed through an OAuth flow in
//! a browser and routines are created from a conversation; neither is a form
//! in a settings panel, so the panel shows what exists rather than pretending
//! to be the place it is made.
//!
//! A routine is the run nobody is watching: a person who cannot see that one
//! exists, or that it has been paused, has no way to notice it stopped
//! working. The same is true of a connector that has quietly lost its
//! credential.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A connected app, as `openbot connector ls --json` prints it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Connector {
    pub id: String,
    pub url: String,
    /// The credential names it needs, never a value. Nothing in this product
    /// reads one back.
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// Recurring work, as `openbot routine ls --json` prints it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Routine {
    /// Which Bot runs it, by id: stable across a rename, and what any command
    /// addressing it needs.
    pub bot: String,
    /// The same Bot's display name. Shown instead of the id, which stops
    /// matching the name after a rename; falls back to the id when the Bot is
    /// gone.
    #[serde(default)]
    pub bot_name: String,
    pub id: String,
    /// When it runs, in words: "every day at 9:00", or what event starts it.
    pub trigger: String,
    /// The next time it is due, or `None` for an event routine, which has no
    /// next time. Absent rather than invented.
    #[serde(default)]
    pub next: Option<String>,
    /// A paused routine keeps its definition and stops running. It looks
    /// identical to a working one unless this is shown.
    pub enabled: bool,
}

/// Stop a routine firing, or start it again.
///
/// A routine is the run nobody is watching, so this is the control that
/// matters when one starts failing every night or costing more than it is
/// worth. The alternative is deleting it and losing its definition.
///
/// The routine keeps its definition and history either way. Pausing is not
/// deleting, which is why the window can offer it without a confirmation.
///
/// # Errors
/// If the binary cannot be run, or there is no such routine on that Bot.
pub async fn set_paused(
    openbot: &Path,
    home: &Path,
    bot: &str,
    routine: &str,
    paused: bool,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("routine")
        .arg(if paused { "pause" } else { "resume" })
        .arg(bot)
        .arg(routine)
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_HUB_URL");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .trim_start_matches("Error:")
                .trim()
        ));
    }
    Ok(())
}

/// Every connected app.
///
/// # Errors
/// If the binary cannot be run, fails, or answers something else.
pub async fn connectors(openbot: &Path, home: &Path) -> anyhow::Result<Vec<Connector>> {
    read(openbot, home, "connector").await
}

/// Every routine, across every Bot.
///
/// # Errors
/// As [`connectors`].
pub async fn routines(openbot: &Path, home: &Path) -> anyhow::Result<Vec<Routine>> {
    read(openbot, home, "routine").await
}

async fn read<T: serde::de::DeserializeOwned>(
    openbot: &Path,
    home: &Path,
    group: &str,
) -> anyhow::Result<Vec<T>> {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg(group)
        .arg("ls")
        .arg("--json")
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_HUB_URL");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "`openbot {group} ls` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("`openbot {group} ls --json` answered something else: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_routine_shape_matches_what_the_binary_prints() {
        let json = r#"[{"bot":"account-health","bot_name":"Account Health",
                        "id":"morning","trigger":"every day at 9:00",
                        "next":"2026-08-17T09:00:00+00:00","enabled":true}]"#;
        let rows: Vec<Routine> = serde_json::from_str(json).expect("parses");
        assert_eq!(rows[0].bot, "account-health");
        // Both id and name, because after a rename they differ. A hand-written
        // fixture cannot detect the binary changing shape (`#[serde(default)]`
        // would fill a missing `bot_name` silently);
        // `openbot/tests/roster_live.rs` asks the real binary and is what
        // holds this true.
        assert_eq!(rows[0].bot_name, "Account Health");
        assert_eq!(rows[0].trigger, "every day at 9:00");
        assert!(rows[0].enabled);
        assert!(rows[0].next.is_some());
    }

    /// An event routine has no next time, and the field is absent rather than
    /// zero or "never".
    #[test]
    fn an_event_routine_has_no_next_time() {
        let json = r#"[{"bot":"b","id":"on-push","trigger":"on an event",
                        "next":null,"enabled":false}]"#;
        let rows: Vec<Routine> = serde_json::from_str(json).expect("parses");
        assert!(rows[0].next.is_none());
        assert!(!rows[0].enabled, "a paused routine must say it is paused");
    }

    #[test]
    fn a_connector_lists_the_credentials_it_needs_and_no_values() {
        let json = r#"[{"id":"linear","url":"https://mcp.linear.app/sse",
                        "secrets":["linear-token"]}]"#;
        let rows: Vec<Connector> = serde_json::from_str(json).expect("parses");
        assert_eq!(rows[0].secrets, vec!["linear-token".to_owned()]);
    }
}
