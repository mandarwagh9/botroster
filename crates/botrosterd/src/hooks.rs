//! `PreToolUse` hooks: a command that gets a veto before a tool runs.
//!
//! The format is the Claude Code hook contract, which Grok Build adopted: a
//! JSON object on stdin, a decision on stdout. Existing hooks work unchanged.
//!
//! # Where this runs
//!
//! In the hub, next to the policy gate, not in the agent loop. A hook
//! evaluated by the client is a check the client can delete, and the harness
//! is not a trust boundary (`docs/SPEC.md` §6.0). The same reasoning that put
//! approvals here puts hooks here.
//!
//! # Fail closed
//!
//! Upstream is fail-open: a hook that times out, crashes, or returns garbage
//! lets the call proceed. That is defensible for a local CLI with someone
//! watching the terminal and wrong for an unattended agent holding a live
//! session, where a broken guard rail would go unnoticed. Here a hook that
//! cannot answer denies, with a per-hook `fail_open` for hooks that are
//! genuinely advisory.

use std::path::Path;
use std::time::Duration;

use botroster_proto::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What a hook said about a call.
#[derive(Debug, Clone, PartialEq)]
pub enum HookVerdict {
    /// No hook objected. This is not approval; the policy still decides.
    NoObjection,
    /// Refuse the call, with something a person can read.
    Deny(String),
}

/// The hub asks this before running a tool.
///
/// A trait so the hub does not depend on process spawning, and so tests can
/// state a verdict directly rather than shipping shell scripts.
#[async_trait::async_trait]
pub trait PreToolUse: Send + Sync {
    async fn check(&self, session: &SessionId, tool: &str, args: &Value) -> HookVerdict;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hook {
    /// Tool ids this applies to. `*` matches everything, `fs.*` a namespace.
    #[serde(default = "everything")]
    pub matches: String,
    /// The command to run, as a shell line.
    pub command: String,
    /// Let the call through when this hook cannot answer.
    ///
    /// Off by default; this is the divergence from upstream. Turn it on only
    /// for a hook that logs or notifies, never one that decides.
    #[serde(default)]
    pub fail_open: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn everything() -> String {
    "*".into()
}
fn default_timeout() -> u64 {
    10
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

impl Hooks {
    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let p = home.join("hooks.json");
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("{} is not readable: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    fn matching<'a>(&'a self, tool: &'a str) -> impl Iterator<Item = &'a Hook> + 'a {
        self.hooks.iter().filter(move |h| glob(&h.matches, tool))
    }
}

/// The same single-`*` glob the policy uses, for the same reason: a rule that
/// decides whether something runs must be obvious on sight.
fn glob(pattern: &str, s: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == s,
        Some((a, b)) => s.len() >= a.len() + b.len() && s.starts_with(a) && s.ends_with(b),
    }
}

#[async_trait::async_trait]
impl PreToolUse for Hooks {
    async fn check(&self, session: &SessionId, tool: &str, args: &Value) -> HookVerdict {
        for h in self.matching(tool) {
            // The payload Claude Code hooks already expect.
            let payload = json!({
                "hook_event_name": "PreToolUse",
                "session_id": session.as_str(),
                "tool_name": tool,
                "tool_input": args,
            })
            .to_string();

            match run_one(h, &payload).await {
                Ok(HookVerdict::NoObjection) => {}
                Ok(v) => return v,
                Err(why) => {
                    if h.fail_open {
                        tracing::warn!(
                            hook = %h.command, error = %why,
                            "hook failed and is marked fail_open; letting the call through"
                        );
                    } else {
                        tracing::warn!(hook = %h.command, error = %why, "hook failed; denying");
                        return HookVerdict::Deny(format!(
                            "a PreToolUse hook could not answer ({why}); denying because \
                             a guard that is not working is not a reason to proceed"
                        ));
                    }
                }
            }
        }
        HookVerdict::NoObjection
    }
}

/// Run one hook and read its decision.
async fn run_one(h: &Hook, payload: &str) -> Result<HookVerdict, String> {
    use tokio::io::AsyncWriteExt;

    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        // `raw_arg`, not `arg`: Windows re-quotes every argument, and cmd.exe
        // then reads those quotes as part of the command. A hook whose path
        // contains a space (`C:\Users\Some Name\audit.cmd`) has to be quoted
        // by the person writing it, and that quoting must survive.
        use std::os::windows::process::CommandExt;
        c.as_std_mut().raw_arg(format!("/C {}", h.command));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(&h.command);
        c
    };
    // Reap the child if the wait is abandoned: a hook that hangs must not
    // outlive the call it was gating.
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes()).await;
        drop(stdin); // a hook that reads to EOF would otherwise wait forever
    }

    let out = tokio::time::timeout(
        Duration::from_secs(h.timeout_secs),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("no answer in {}s", h.timeout_secs))?
    .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();

    // Exit code 2 is the documented "block, and tell the model why" signal.
    if out.status.code() == Some(2) {
        return Ok(HookVerdict::Deny(if stderr.is_empty() {
            "a PreToolUse hook blocked this".into()
        } else {
            stderr
        }));
    }

    // Anything other than "ran fine" or "blocked" is a hook that did not
    // answer. A command that does not exist exits 1 with nothing on stdout,
    // and reading that as consent is the fail-open hole this module exists to
    // close: a typo in the config would silently disarm the guard.
    if !out.status.success() {
        return Err(format!(
            "exit {}{}",
            out.status.code().unwrap_or(-1),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {}", cut(&stderr))
            }
        ));
    }

    if stdout.is_empty() {
        // The common case: a hook that logs, notifies, or lints and has no
        // opinion. Silence from a hook that ran cleanly is not an objection.
        return Ok(HookVerdict::NoObjection);
    }

    let v: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("its answer was not JSON ({e}): {}", cut(&stdout)))?;

    // Both the older `decision` field and the newer nested form, because hooks
    // in the wild use each.
    let decision = v
        .get("decision")
        .and_then(|d| d.as_str())
        .or_else(|| {
            v.pointer("/hookSpecificOutput/permissionDecision")
                .and_then(|d| d.as_str())
        })
        .unwrap_or("");
    let reason = v
        .get("reason")
        .and_then(|r| r.as_str())
        .or_else(|| {
            v.pointer("/hookSpecificOutput/permissionDecisionReason")
                .and_then(|r| r.as_str())
        })
        .unwrap_or("a PreToolUse hook denied this")
        .to_owned();

    match decision {
        "deny" | "block" => Ok(HookVerdict::Deny(reason)),
        // An "approve" does not grant anything: the policy still decides, and
        // a hook that could silently lift the approval gate would be a way to
        // turn the gate off from a config file.
        "approve" | "allow" | "ask" | "" => Ok(HookVerdict::NoObjection),
        other => Err(format!("unknown decision `{other}`")),
    }
}

fn cut(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a script that prints `out` and exits `code`, and return a command
    /// line that runs it.
    ///
    /// A file rather than an inline `echo`: quoting JSON through `cmd /C` on
    /// Windows mangles it, and a hook in the real world is a script anyway.
    fn script(dir: &Path, out: &str, code: i32) -> String {
        if cfg!(windows) {
            let p = dir.join("hook.cmd");
            let body = if out.is_empty() {
                String::new()
            } else {
                format!("echo {out}\r\n")
            };
            let text = format!("@echo off\r\n{body}exit /b {code}\r\n");
            std::fs::write(&p, text).unwrap();
            format!("\"{}\"", p.display())
        } else {
            let p = dir.join("hook.sh");
            let body = if out.is_empty() {
                String::new()
            } else {
                format!("cat <<'EOF'\n{out}\nEOF\n")
            };
            std::fs::write(&p, format!("#!/bin/sh\n{body}exit {code}\n")).unwrap();
            format!("sh {}", p.display())
        }
    }

    fn hook(command: &str) -> Hooks {
        Hooks {
            hooks: vec![Hook {
                matches: "*".into(),
                command: command.into(),
                fail_open: false,
                timeout_secs: 5,
            }],
        }
    }

    async fn check(h: &Hooks, tool: &str) -> HookVerdict {
        h.check(&SessionId::new("s1"), tool, &json!({ "path": "a.md" }))
            .await
    }

    #[test]
    fn globs_match_a_namespace_or_everything() {
        assert!(glob("*", "shell.exec"));
        assert!(glob("fs.*", "fs.write"));
        assert!(!glob("fs.*", "shell.exec"));
        assert!(glob("shell.exec", "shell.exec"));
    }

    #[tokio::test]
    async fn a_hook_that_says_nothing_is_not_an_objection() {
        // The common shape: something that logs and exits.
        let h = hook(if cfg!(windows) { "cd ." } else { "true" });
        assert_eq!(check(&h, "fs.write").await, HookVerdict::NoObjection);
    }

    #[tokio::test]
    async fn a_hook_can_deny_with_a_reason() {
        let d = tempfile::tempdir().unwrap();
        let cmd = script(
            d.path(),
            r#"{"decision":"deny","reason":"not on a Friday"}"#,
            0,
        );
        match check(&hook(&cmd), "fs.write").await {
            HookVerdict::Deny(r) => assert!(r.contains("Friday"), "{r}"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_code_two_blocks_and_carries_stderr() {
        let cmd = if cfg!(windows) {
            "echo nope 1>&2 && exit 2"
        } else {
            "echo nope >&2; exit 2"
        };
        match check(&hook(cmd), "shell.exec").await {
            HookVerdict::Deny(r) => assert!(r.contains("nope"), "{r}"),
            other => panic!("expected a block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hook_that_cannot_run_denies() {
        // The divergence from upstream, which lets this through.
        match check(&hook("this-command-does-not-exist-anywhere"), "fs.write").await {
            HookVerdict::Deny(r) => assert!(r.contains("not working"), "{r}"),
            other => panic!("a broken hook let the call through: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hook_that_answers_nonsense_denies() {
        let d = tempfile::tempdir().unwrap();
        let cmd = script(d.path(), "not json at all", 0);
        match check(&hook(&cmd), "fs.write").await {
            HookVerdict::Deny(r) => assert!(r.contains("not working"), "{r}"),
            other => panic!("a malformed answer was treated as consent: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hook_that_hangs_denies_rather_than_hanging_the_call() {
        let cmd = if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        };
        let mut h = hook(cmd);
        h.hooks[0].timeout_secs = 1;
        let started = std::time::Instant::now();
        let v = check(&h, "fs.write").await;
        assert!(matches!(v, HookVerdict::Deny(_)), "{v:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the call waited past the hook timeout"
        );
    }

    #[tokio::test]
    async fn fail_open_is_available_for_a_hook_that_only_advises() {
        let mut h = hook("this-command-does-not-exist-anywhere");
        h.hooks[0].fail_open = true;
        assert_eq!(check(&h, "fs.write").await, HookVerdict::NoObjection);
    }

    #[tokio::test]
    async fn an_approve_does_not_lift_the_approval_gate() {
        // A hook returning "approve" must not become a way to switch the gate
        // off from a config file; it only means "no objection from me".
        let d = tempfile::tempdir().unwrap();
        let cmd = script(d.path(), r#"{"decision":"approve"}"#, 0);
        assert_eq!(
            check(&hook(&cmd), "shell.exec").await,
            HookVerdict::NoObjection
        );
    }

    #[tokio::test]
    async fn a_hook_only_sees_the_tools_it_matches() {
        let d = tempfile::tempdir().unwrap();
        let mut h = hook(&script(d.path(), r#"{"decision":"deny","reason":"no"}"#, 0));
        h.hooks[0].matches = "shell.*".into();
        assert_eq!(check(&h, "fs.write").await, HookVerdict::NoObjection);
        assert!(matches!(
            check(&h, "shell.exec").await,
            HookVerdict::Deny(_)
        ));
    }

    #[test]
    fn a_missing_hooks_file_is_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(Hooks::load(d.path()).unwrap().is_empty());
    }

    #[test]
    fn hooks_load_in_the_documented_shape() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("hooks.json"),
            r#"{"hooks":[{"matches":"shell.*","command":"./audit.sh"}]}"#,
        )
        .unwrap();
        let h = Hooks::load(d.path()).unwrap();
        assert_eq!(h.hooks.len(), 1);
        // Leaving `fail_open` out must yield the safe default.
        assert!(!h.hooks[0].fail_open);
        assert_eq!(h.hooks[0].timeout_secs, 10);
    }
}
