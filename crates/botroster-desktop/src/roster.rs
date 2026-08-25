//! The roster: which Bots exist, for the sidebar to show.
//!
//! This intentionally does not go over the ACP connection. ACP has no notion
//! of "list the agents you could talk to", and inventing one would be private
//! protocol for something the shipped binary already answers. Nor is it a
//! direct read of the Bot store: the agent process owns that store, and a
//! second reader of its files couples to a storage format rather than to a
//! contract.
//!
//! So this asks `botroster bot ls --json`, which exists for exactly this and is
//! pinned by `botroster-cli/tests/cli.rs`. One subprocess per refresh is
//! acceptable for a sidebar.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One Bot, as a sidebar shows it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// What the client addresses. Stable across renames of the display name.
    pub id: String,
    /// What a person calls it: "Talent Scout".
    pub name: String,
    /// The job, in a few words. Empty is normal and the UI must cope.
    #[serde(default)]
    pub title: String,
    /// Standing brief. Long; the sidebar shows it on hover or not at all.
    #[serde(default)]
    pub description: String,
    /// Hidden Bots keep their work and leave the list ("Hide from sidebar",
    /// with "Show hidden chats" to bring them back).
    #[serde(default)]
    pub hidden: bool,
    /// Whether there is a conversation to come back to.
    #[serde(default)]
    pub messages: u64,
}

/// Every Bot in a home, in the order the binary lists them.
///
/// A hidden Bot is out of the main list, and asking for it is a separate act
/// ("Show hidden chats"), not a filter the sidebar applies to a list it
/// already has. Asking the binary keeps the two lists distinct rather than
/// making the client reimplement the rule.
///
/// # Errors
/// If the binary cannot be run, exits non-zero, or answers something that is
/// not the roster. A sidebar that silently shows nothing when the roster fails
/// is indistinguishable from a person with no Bots.
pub async fn list(botroster: &Path, home: &Path, hidden: bool) -> anyhow::Result<Vec<Entry>> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("bot")
        .arg("ls")
        .arg("--json")
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        // Never inherit an operator's shell into a client's roster.
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL");
    if hidden {
        cmd.arg("--all");
    }
    #[cfg(windows)]
    {
        // Otherwise every refresh flashes a console window over the app.
        // `tokio::process::Command` has its own `creation_flags` on Windows;
        // std's `CommandExt` does not apply to it.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "`botroster bot ls` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim()).map_err(|e| {
        anyhow::anyhow!("`botroster bot ls --json` answered something else ({e}): {text}")
    })
}

/// Change a Bot's name, title or description.
///
/// `None` means "leave it" rather than "clear it", so a form that sends only
/// the field that was touched cannot blank the one beside it; clearing is
/// `Some("")`.
///
/// A rename keeps the Bot's id, which is what its conversation, inbox, group
/// memberships and routines are all stored under (see `BotStore::rename`).
/// The window shows names; the home is keyed by ids, and the two stop
/// matching after the first rename.
///
/// # Errors
/// If the binary cannot be run, or refuses: an empty name, a Bot that does
/// not exist, or nothing to change.
pub async fn describe(
    botroster: &Path,
    home: &Path,
    bot: &str,
    rename: Option<&str>,
    title: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("bot")
        .arg("set")
        .arg(bot)
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL");
    for (flag, value) in [
        ("--rename", rename),
        ("--title", title),
        ("--description", description),
    ] {
        // Only what was given. Passing `--title ""` for an untouched field
        // would silently erase it.
        if let Some(v) = value {
            cmd.arg(flag).arg(v);
        }
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
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

/// Copy a Bot's brief as the start of another.
///
/// The copy does not inherit the conversation (`BotStore::duplicate` matches
/// that): a Bot copied to cover a second region should not start answering
/// with facts about the first one.
///
/// # Errors
/// If the binary cannot be run, or refuses: no such Bot, a name already
/// taken, or an empty name.
pub async fn duplicate(
    botroster: &Path,
    home: &Path,
    bot: &str,
    new_name: &str,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("bot")
        .arg("dup")
        .arg(bot)
        .arg(new_name)
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
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
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

/// Delete a Bot and everything under it.
///
/// Irreversible, and it removes more than the Bot. The conversation, the
/// inbox and any routines live in the Bot's directory and go with it; the
/// binary also removes it from every group that holds it, because a group
/// naming a Bot that is gone answers every post with `no bot ...` forever.
/// Callers should confirm with the person first; `BotStore::delete` computes
/// the list of what goes.
///
/// # Errors
/// If the binary cannot be run, or there is no such Bot.
pub async fn delete(botroster: &Path, home: &Path, bot: &str) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("bot")
        .arg("rm")
        .arg(bot)
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
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
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

/// Take a Bot out of the list, or put it back.
///
/// Hiding is not archiving: the Bot keeps its conversation and keeps running
/// anything it has scheduled, which `botroster bot hide` warns about explicitly.
/// SPEC §8 calls that a footgun, so a window offering this should give the
/// same warning; see [`live_routines`].
///
/// # Errors
/// If the binary cannot be run, or there is no such Bot.
pub async fn set_hidden(
    botroster: &Path,
    home: &Path,
    bot: &str,
    hidden: bool,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.arg("bot")
        .arg(if hidden { "hide" } else { "unhide" })
        .arg(bot)
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
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
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

/// A Bot in a group, as both things a client needs it to be.
///
/// The id is not the name. A group stores ids, a window shows names, and
/// `botroster bot set --rename` intentionally keeps the id, so after one rename
/// the two differ permanently. Carrying only the id would put slugs in the
/// sidebar beside Bots listed by name; carrying only the name would leave
/// nothing to address it by.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Member {
    pub id: String,
    /// The current name, or the id if the Bot is gone. A group that lists a
    /// member which no longer exists shows the id rather than an empty space.
    pub name: String,
}

/// A group: several Bots on one thread, with the handoffs between them
/// visible in it. `botroster group log` reads that thread back.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// The members, coordinator first; the coordinator answers anything
    /// nobody was mentioned in.
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub messages: u64,
}

/// Every group in a home.
///
/// # Errors
/// As [`list`].
pub async fn groups(botroster: &Path, home: &Path) -> anyhow::Result<Vec<Group>> {
    let out = run(botroster, home, &["group", "ls", "--json"]).await?;
    serde_json::from_str(out.trim())
        .map_err(|e| anyhow::anyhow!("`botroster group ls --json` answered something else: {e}"))
}

/// One group's thread, oldest first.
///
/// # Errors
/// As [`list`], plus if there is no such group.
pub async fn group_log(
    botroster: &Path,
    home: &Path,
    name: &str,
) -> anyhow::Result<serde_json::Value> {
    let out = run(botroster, home, &["group", "log", name, "--json"]).await?;
    serde_json::from_str(out.trim())
        .map_err(|e| anyhow::anyhow!("`botroster group log --json` answered something else: {e}"))
}

/// One place a phrase was said.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Hit {
    /// `bot` or `group`.
    pub kind: String,
    /// Whose conversation it was.
    pub name: String,
    /// Which message, counting from zero.
    pub at: usize,
    pub role: String,
    /// The line around the match, so a result reads on its own.
    pub text: String,
}

/// Find a phrase in what the Bots and groups have said.
///
/// # Errors
/// As [`list`], plus if the phrase is empty, which the binary refuses rather
/// than answering with everything ever said.
pub async fn search(botroster: &Path, home: &Path, query: &str) -> anyhow::Result<Vec<Hit>> {
    let out = run(botroster, home, &["search", query, "--json"]).await?;
    serde_json::from_str(out.trim())
        .map_err(|e| anyhow::anyhow!("`botroster search --json` answered something else: {e}"))
}

async fn run(botroster: &Path, home: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new(botroster);
    cmd.args(args)
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
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", botroster.display()))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape is a contract with `botroster bot ls --json`, and the fields the
    /// sidebar needs must survive a round trip. Pinned here as well as in the
    /// CLI's own test because these are the two ends of the same wire.
    #[test]
    fn the_roster_shape_matches_what_the_binary_prints() {
        let json = r#"[{"id":"talent-scout","name":"Talent Scout","title":"Hiring",
                        "description":"Finds candidates","hidden":false,"seq":0,"messages":12}]"#;
        let rows: Vec<Entry> = serde_json::from_str(json).expect("the roster parses");
        assert_eq!(rows[0].id, "talent-scout");
        assert_eq!(rows[0].name, "Talent Scout");
        assert_eq!(rows[0].title, "Hiring");
        assert_eq!(rows[0].messages, 12);
        assert!(!rows[0].hidden);
    }

    /// `bot new` requires only a name, so a Bot with no title and no
    /// description is ordinary and must parse.
    #[test]
    fn a_bot_with_no_title_still_parses() {
        let json = r#"[{"id":"x","name":"X","hidden":false,"messages":0}]"#;
        let rows: Vec<Entry> = serde_json::from_str(json).expect("the roster parses");
        assert_eq!(rows[0].title, "");
        assert_eq!(rows[0].description, "");
    }

    #[test]
    fn a_group_lists_its_members_coordinator_first() {
        let json = r#"[{"id":"website-launch","name":"Website Launch",
                        "members":[{"id":"researcher","name":"Researcher"},
                                   {"id":"writer","name":"Writer"},
                                   {"id":"reviewer","name":"Reviewer"}],
                        "messages":12}]"#;
        let rows: Vec<Group> = serde_json::from_str(json).expect("parses");
        assert_eq!(rows[0].name, "Website Launch");
        assert_eq!(
            rows[0].members[0].id, "researcher",
            "the coordinator leads the list; it answers anything nobody was mentioned in"
        );
        assert_eq!(rows[0].messages, 12);
    }

    /// A member carries both id and name because after a rename they differ.
    /// The id is what the group stores and what an `@mention` must be; the
    /// name is what a sidebar shows.
    #[test]
    fn a_renamed_member_keeps_its_id_and_shows_its_new_name() {
        let json = r#"[{"id":"launch","name":"Launch",
                        "members":[{"id":"talent-scout","name":"Recruiting"}],
                        "messages":0}]"#;
        let rows: Vec<Group> = serde_json::from_str(json).expect("parses");
        assert_eq!(rows[0].members[0].id, "talent-scout");
        assert_eq!(rows[0].members[0].name, "Recruiting");
    }

    #[test]
    fn an_unknown_field_is_not_a_parse_failure() {
        let json = r#"[{"id":"x","name":"X","hidden":false,"messages":0,"avatar":"a.png"}]"#;
        serde_json::from_str::<Vec<Entry>>(json).expect("unknown fields are ignored");
    }
}
