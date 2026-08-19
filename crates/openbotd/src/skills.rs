//! Skills: reusable procedures a Bot can look up when it needs them.
//!
//! The format is the one Claude Code and Grok Build use, so an existing skill
//! folder works unchanged: `<home>/skills/<name>/SKILL.md`, with YAML
//! frontmatter carrying at least a `name` and a `description`.
//!
//! ```text
//! ---
//! name: refund-a-customer
//! description: How to issue a refund, including the approvals needed.
//! ---
//!
//! 1. Find the charge in Stripe …
//! ```
//!
//! # Progressive disclosure
//!
//! Only the descriptions go to the model, as the catalogue of `skill.list`.
//! The body is fetched by `skill.read` when the model decides it needs it.
//! Pasting every skill into the system prompt would defeat the purpose: the
//! context window is the scarce resource, and a Bot with thirty procedures
//! would spend most of it on twenty-nine it is not currently doing.
//!
//! Skills are instructions, not code. Nothing here executes anything; a skill
//! body is text that reaches the model, and the model still has to use the
//! ordinary, gated tools to act on it.

use std::path::{Path, PathBuf};

use openbot_proto::frames::ToolDescription;
use serde_json::{json, Value};

use crate::hub::InternalTools;

/// How much of a skill body to hand over at once.
///
/// A skill is meant to be a procedure, not a manual. Truncating loudly beats
/// silently blowing a context window on a file somebody pasted a PDF into.
const MAX_BODY_BYTES: usize = 32_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Skills found under `<home>/skills`.
pub struct Skills {
    /// Kept so the directory can be read again. A skill is content, not code:
    /// one written while the computer is running must be visible to the next
    /// task without a restart.
    home: PathBuf,
    skills: Vec<Skill>,
    /// Folders that look like skills but could not be loaded, and why.
    ///
    /// Kept rather than only logged, because a warning at hub boot is seen by
    /// nobody: the person who needs it is the skill's author, hours later,
    /// wondering why their Bot ignores it. `openbot skill ls` reads this.
    problems: Vec<(PathBuf, String)>,
}

impl Skills {
    /// Load every `skills/*/SKILL.md` under this home.
    ///
    /// A folder without a `SKILL.md`, or one whose frontmatter has no
    /// description, is skipped with a warning rather than failing the boot: a
    /// half-written skill should not stop the control plane starting, and the
    /// warning is what tells its author why it is not showing up.
    pub fn load(home: &Path) -> Self {
        let dir = home.join("skills");
        let mut skills = Vec::new();
        let mut problems = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Self {
                home: home.to_path_buf(),
                skills,
                problems,
            };
        };

        for e in entries.flatten() {
            let path = e.path().join("SKILL.md");
            if !path.exists() {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => match parse(&text, &e.file_name().to_string_lossy()) {
                    Ok(s) => skills.push(s),
                    Err(why) => {
                        tracing::warn!(path = %path.display(), reason = %why, "skipping a skill");
                        problems.push((path.clone(), why));
                    }
                },
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "cannot read a skill");
                    problems.push((path.clone(), err.to_string()));
                }
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        problems.sort();
        Self {
            home: home.to_path_buf(),
            skills,
            problems,
        }
    }

    /// Whether any skill exists, without reading any of them.
    ///
    /// `catalog()` runs on every session bind and only needs to know whether
    /// to offer the two tools. Re-reading every `SKILL.md` to answer that would
    /// put the whole skills directory on the path of every task.
    fn any_exist(home: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(home.join("skills")) else {
            return false;
        };
        entries
            .flatten()
            .any(|e| e.path().join("SKILL.md").exists())
    }

    /// Skills that could not be loaded, and why. Empty in the happy case.
    pub fn problems(&self) -> &[(PathBuf, String)] {
        &self.problems
    }

    /// Everything that loaded, for a listing.
    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn names(&self) -> Vec<String> {
        self.skills.iter().map(|s| s.name.clone()).collect()
    }

    fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }
}

/// Split frontmatter from body and read the two fields that matter.
///
/// Not a YAML parser: this reads `key: value` lines and nothing else, because
/// a skill's frontmatter is a handful of scalars and does not justify a YAML
/// dependency. Unknown keys are ignored rather than rejected, so a skill
/// written for another runtime still loads.
fn parse(text: &str, folder: &str) -> Result<Skill, String> {
    let rest = text
        .strip_prefix("---")
        .ok_or("no frontmatter: a SKILL.md starts with `---`")?;
    let end = rest
        .find("\n---")
        .ok_or("the frontmatter is not closed with `---`")?;
    let (front, body) = rest.split_at(end);

    let mut name = String::new();
    let mut description = String::new();
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_owned();
        match k.trim() {
            "name" => name = v,
            "description" => description = v,
            _ => {}
        }
    }

    // The folder name is a reasonable fallback for `name`; a missing
    // description is not recoverable, because it is the only thing the model
    // sees when deciding whether this skill is relevant.
    if name.is_empty() {
        name = folder.to_owned();
    }
    if description.is_empty() {
        return Err("no `description` in the frontmatter, so a model would \
                    never know when to use it"
            .into());
    }

    let body = body.trim_start_matches("\n---").trim().to_owned();
    Ok(Skill {
        name,
        description,
        body,
    })
}

#[async_trait::async_trait]
impl InternalTools for Skills {
    fn catalog(&self) -> Vec<ToolDescription> {
        // Offered only when there is something to offer: two tools that always
        // return nothing waste context. Asked of the directory rather than of
        // the boot-time snapshot, so a skill written after boot does not need
        // a restart to exist.
        if !Self::any_exist(&self.home) {
            return Vec::new();
        }
        vec![
            ToolDescription::new(
                "skill.list",
                "List the procedures available to you, with what each one is for. \
                 Read one with skill.read before doing work it covers.",
                json!({ "type": "object", "properties": {}, "required": [] }),
            ),
            ToolDescription::new(
                "skill.read",
                "Read a procedure in full, by name, as listed by skill.list.",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "The skill's name." }
                    },
                    "required": ["name"]
                }),
            ),
        ]
    }

    async fn invoke(
        &self,
        _as_bot: Option<&str>,
        tool: &str,
        args: &Value,
    ) -> Result<Value, String> {
        // Re-read the directory: a skill written since boot is the common
        // case, and the cost is a handful of small files on a path the model
        // reaches only when it has decided a procedure might apply.
        let live = Self::load(&self.home);
        match tool {
            "skill.list" => Ok(json!({
                "skills": live
                    .skills
                    .iter()
                    .map(|s| json!({ "name": s.name, "description": s.description }))
                    .collect::<Vec<_>>()
            })),
            "skill.read" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("`name` must be a string")?;
                let s = live.get(name).ok_or_else(|| {
                    // Name the ones that do exist: the usual cause is a near
                    // miss, and a bare "not found" makes the model guess again.
                    format!(
                        "no skill called `{name}`; there is {}",
                        live.names().join(", ")
                    )
                })?;
                let (body, truncated) = if s.body.len() > MAX_BODY_BYTES {
                    let mut end = MAX_BODY_BYTES;
                    while end > 0 && !s.body.is_char_boundary(end) {
                        end -= 1;
                    }
                    (s.body[..end].to_owned(), true)
                } else {
                    (s.body.clone(), false)
                };
                Ok(json!({
                    "name": s.name,
                    "description": s.description,
                    "body": body,
                    "truncated": truncated,
                }))
            }
            other => Err(format!("unknown skill tool `{other}`")),
        }
    }
}

/// Where a skill folder goes, for error messages and docs.
pub fn dir(home: &Path) -> PathBuf {
    home.join("skills")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn listing_reads_the_directory_again_rather_than_a_snapshot() {
        // A skill added after load must show up: skills are typically written
        // while the computer is running, in response to something the Bot
        // just got wrong.
        let d = tempfile::tempdir().unwrap();
        let skills = Skills::load(d.path());
        assert!(skills.is_empty());

        write_skill(
            d.path(),
            "refund-a-customer",
            "---
name: refund-a-customer
description: how to issue a refund
---

Ask first.
",
        );

        let listed = skills
            .invoke(None, "skill.list", &json!({}))
            .await
            .expect("skill.list");
        let names: Vec<String> = listed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(names, ["refund-a-customer"], "the listing is stale");

        // And its body is readable, not just its name.
        let read = skills
            .invoke(None, "skill.read", &json!({ "name": "refund-a-customer" }))
            .await
            .expect("skill.read");
        assert!(
            read.to_string().contains("Ask first"),
            "the body came from the snapshot: {read}"
        );
    }

    fn write_skill(home: &Path, folder: &str, text: &str) {
        let d = dir(home).join(folder);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), text).unwrap();
    }

    const REFUND: &str = "---\nname: refund-a-customer\ndescription: How to issue a refund.\n---\n\n1. Find the charge.\n2. Ask before refunding over £100.\n";

    #[test]
    fn a_home_with_no_skills_offers_no_tools() {
        let d = tempfile::tempdir().unwrap();
        let s = Skills::load(d.path());
        assert!(s.is_empty());
        // Two tools that can only ever answer "nothing" waste context.
        assert!(s.catalog().is_empty());
    }

    #[tokio::test]
    async fn a_skill_is_listed_by_description_and_read_by_name() {
        let d = tempfile::tempdir().unwrap();
        write_skill(d.path(), "refund", REFUND);
        let s = Skills::load(d.path());

        assert_eq!(s.len(), 1);
        assert_eq!(s.catalog().len(), 2);

        // The list carries descriptions only.
        let listed = s.invoke(None, "skill.list", &json!({})).await.unwrap();
        assert_eq!(listed["skills"][0]["name"], "refund-a-customer");
        assert!(listed["skills"][0]["description"]
            .as_str()
            .unwrap()
            .contains("refund"));
        assert!(
            !listed.to_string().contains("Find the charge"),
            "the whole body went to the model in the listing: {listed}"
        );

        let read = s
            .invoke(None, "skill.read", &json!({ "name": "refund-a-customer" }))
            .await
            .unwrap();
        assert!(read["body"].as_str().unwrap().contains("Find the charge"));
        assert_eq!(read["truncated"], false);
    }

    #[tokio::test]
    async fn a_missing_skill_names_the_ones_that_exist() {
        let d = tempfile::tempdir().unwrap();
        write_skill(d.path(), "refund", REFUND);
        let s = Skills::load(d.path());
        let e = s
            .invoke(None, "skill.read", &json!({ "name": "refund-customer" }))
            .await
            .unwrap_err();
        assert!(e.contains("refund-a-customer"), "unhelpful error: {e}");
    }

    #[test]
    fn a_skill_without_a_description_is_skipped_rather_than_breaking_the_boot() {
        let d = tempfile::tempdir().unwrap();
        write_skill(d.path(), "good", REFUND);
        write_skill(d.path(), "bad", "---\nname: half-written\n---\n\nbody\n");
        // A half-written skill should not stop the control plane starting; its
        // author finds out from the warning, not from an outage.
        let s = Skills::load(d.path());
        assert_eq!(s.names(), vec!["refund-a-customer"]);
    }

    #[test]
    fn a_folder_without_a_skill_file_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir(d.path()).join("notes")).unwrap();
        std::fs::write(dir(d.path()).join("notes/README.md"), "hello").unwrap();
        assert!(Skills::load(d.path()).is_empty());
    }

    #[test]
    fn the_folder_name_stands_in_for_a_missing_name_field() {
        let d = tempfile::tempdir().unwrap();
        write_skill(
            d.path(),
            "deploy-the-site",
            "---\ndescription: How to deploy.\n---\n\nsteps\n",
        );
        assert_eq!(Skills::load(d.path()).names(), vec!["deploy-the-site"]);
    }

    #[test]
    fn frontmatter_keys_it_does_not_know_are_ignored() {
        // A skill written for another runtime still loads here.
        let d = tempfile::tempdir().unwrap();
        write_skill(
            d.path(),
            "x",
            "---\nname: x\ndescription: d\nallowed-tools: [Bash]\nversion: 3\n---\n\nbody\n",
        );
        assert_eq!(Skills::load(d.path()).len(), 1);
    }

    #[tokio::test]
    async fn an_enormous_body_is_cut_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let huge = "x".repeat(MAX_BODY_BYTES * 2);
        write_skill(
            d.path(),
            "big",
            &format!("---\nname: big\ndescription: d\n---\n\n{huge}"),
        );
        let s = Skills::load(d.path());
        let read = s
            .invoke(None, "skill.read", &json!({ "name": "big" }))
            .await
            .unwrap();
        assert_eq!(read["truncated"], true);
        assert!(read["body"].as_str().unwrap().len() <= MAX_BODY_BYTES);
    }
}
