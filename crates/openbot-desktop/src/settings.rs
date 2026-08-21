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

/// Write the model settings a connect needs, using the runtime's own
/// `config set`.
///
/// Shelling out rather than writing `config.toml` from here: the file's shape,
/// its defaults and its merge order are the runtime's, and a second writer
/// would be a second definition of them, free to drift the first time either
/// side gained a field.
///
/// The API key is deliberately not among the arguments. `config.toml` names an
/// environment variable to read the key from and never holds the key itself —
/// a key in a config file ends up in a backup, a screen share or a repository —
/// so the window passes it to the agent process instead. See
/// [`crate::engine::Config::api_key`].
///
/// `None` leaves a field alone, so a person who fills in only the model keeps
/// whatever dialect and base URL were already there.
///
/// # Errors
/// If the binary cannot be run, or `config set` fails.
pub async fn save_model(
    openbot: &Path,
    home: &Path,
    model: &str,
    dialect: Option<&str>,
    base_url: Option<&str>,
    api_key_env: Option<&str>,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("config")
        .arg("set")
        .arg("--home")
        .arg(home)
        .arg("--model")
        .arg(model);
    for (flag, value) in [("--dialect", dialect), ("--base-url", base_url)] {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            cmd.arg(flag).arg(value);
        }
    }
    // `api_key_env` does not get the empty-means-absent treatment the other two
    // get, because for this one field empty is a *meaning*: it is how a model
    // on localhost says it wants no credential at all. Folding `Some("")` into
    // `None` here would make that choice unsendable from the window — the
    // field would silently keep whatever key variable was configured before,
    // and a local endpoint would be asked for a key that does not exist. Only
    // `None`, which is "the caller did not mention this field", leaves it
    // alone.
    if let Some(value) = api_key_env {
        cmd.arg("--api-key-env").arg(value.trim());
    }
    cmd.env("NO_COLOR", "1")
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

/// Remember an API key on this computer, under the name `config.toml` records.
///
/// Until this existed the key a person typed into the window lived in the
/// spawned agent's environment and nowhere else, which is correct and meant
/// retyping it at every launch. The runtime reads the environment first and
/// falls back to this store, so an exported variable still wins and nothing
/// about the existing arrangement changes for somebody using one.
///
/// The value goes over **stdin**, never as an argument. Command-line arguments
/// are world-readable in `/proc/<pid>/cmdline` for as long as the process lives
/// and land in shell history besides — which is why `openbot secret set` has no
/// `--value` flag to pass it to. Shelling out rather than writing the file from
/// here for the same reason [`save_model`] does: the store's format, its
/// permissions and its validation are the runtime's, and a second writer would
/// be a second definition of them.
///
/// # Errors
/// If the binary cannot be run, or the runtime refuses the value. It refuses an
/// empty one and one containing control characters, because neither can be sent
/// as an HTTP header — a paste that picked up a stray character fails here,
/// where it can be explained, rather than as an unexplained 401 later.
pub async fn save_key(openbot: &Path, home: &Path, name: &str, value: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("secret")
        .arg("set")
        .arg("--home")
        .arg(home)
        .arg(name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_HUB_URL");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", openbot.display()))?;
    // Taken and dropped before waiting. The child reads to end of file, so a
    // pipe left open here would have both sides waiting for the other.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin on the runtime process"))?;
        stdin.write_all(value.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let out = child.wait_with_output().await?;
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
