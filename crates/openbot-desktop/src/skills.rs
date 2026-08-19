//! Saved procedures a Bot can look up, for the composer's `/`.
//!
//! Typing `/` references a saved skill by name. A person picks one from a
//! list rather than remembering what they called it, so the client's job is
//! to offer the names that actually work.
//!
//! That is why the files that failed to load travel with the catalog. A skill
//! that does not parse is not in `openbot skill ls`'s working set and a Bot
//! ignores it silently: the file is on disk and the procedure is never
//! followed. A menu built from the working set alone would hide that, so the
//! catalog carries both and the caller decides: offer the ones that load, and
//! say that the others exist.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A skill a Bot can be pointed at.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Skill {
    /// How a Bot refers to it, and what a person types after `/`.
    pub name: String,
    /// The sentence a Bot reads before deciding to open it. Shown beside the
    /// name, since a list of bare slugs is hard to use.
    pub description: String,
}

/// A file under `skills/` that did not load, and why.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub path: String,
    pub why: String,
}

/// Everything under `skills/`: what loaded, and what did not.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Catalog {
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub problems: Vec<Problem>,
}

/// Read the skills for a home.
///
/// # Errors
/// If the binary cannot be run, fails, or answers something else. A home with
/// no skills is not an error; it is an empty catalog, which is what a first
/// run looks like.
pub async fn catalog(openbot: &Path, home: &Path) -> anyhow::Result<Catalog> {
    let mut cmd = tokio::process::Command::new(openbot);
    cmd.arg("skill")
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
            "`openbot skill ls` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("`openbot skill ls --json` answered something else: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalog_matches_what_the_binary_prints() {
        // Copied from a real run of `openbot skill ls --json`, not written from
        // the struct, so the fixture reflects the binary's actual output.
        let json = r#"{"skills":[{"name":"refund-a-customer",
                                  "description":"How to issue a refund"}],
                       "problems":[]}"#;
        let cat: Catalog = serde_json::from_str(json).expect("parses");
        assert_eq!(cat.skills[0].name, "refund-a-customer");
        assert_eq!(cat.skills[0].description, "How to issue a refund");
        assert!(cat.problems.is_empty());
    }

    /// A half-written skill is a procedure a Bot quietly ignores. The failure
    /// must survive the trip to the window rather than be hidden there.
    #[test]
    fn a_skill_that_failed_to_load_is_carried_not_dropped() {
        let json = r#"{"skills":[],
                       "problems":[{"path":"/h/skills/half/SKILL.md",
                                    "why":"no frontmatter: a SKILL.md starts with `---`"}]}"#;
        let cat: Catalog = serde_json::from_str(json).expect("parses");
        assert!(cat.skills.is_empty(), "nothing loaded");
        assert_eq!(cat.problems.len(), 1);
        assert!(
            cat.problems[0].why.contains("frontmatter"),
            "the reason must reach the person, not just the fact of failure"
        );
    }

    /// A first run has no `skills/` at all. That is an empty catalog, not a
    /// failure.
    #[test]
    fn a_home_with_no_skills_is_empty_rather_than_broken() {
        let cat: Catalog = serde_json::from_str(r#"{"skills":[],"problems":[]}"#).expect("parses");
        assert_eq!(cat, Catalog::default());
    }
}
