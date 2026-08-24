//! Every command in the README has to be a command this binary accepts.
//!
//! Prose has nothing checking it. Documentation drifts silently, and the
//! person it fails is the one following it for the first time, who has no
//! way to tell whether they typed it wrong or the project did.
//!
//! So this parses the fenced shell blocks out of the README, and for every
//! `openbot ...` line checks that the subcommand exists and that every long
//! flag it uses appears in that subcommand's own `--help`. It does not run
//! them (most would need a hub, and some write files), but a flag that no
//! longer exists is exactly the drift that happens, and it is caught here.

use std::collections::BTreeSet;
use std::process::Command;

const OPENBOT: &str = env!("CARGO_BIN_EXE_openbot");

fn readme() -> String {
    // CARGO_MANIFEST_DIR is crates/openbot-cli.
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("README.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Lines inside ```sh / ```console fences that invoke `openbot`.
fn openbot_invocations(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            // Only shell blocks; a ```text block is illustrative output.
            inside = t.starts_with("```sh") || t.starts_with("```console") || t == "```bash";
            continue;
        }
        if !inside {
            continue;
        }
        let mut cmd = t.trim_start_matches("$ ").trim();
        // The quickstart runs the binary through cargo.
        if let Some(rest) = cmd.strip_prefix("cargo run -p openbot-cli -- ") {
            out.push(format!("openbot {rest}"));
            continue;
        }
        if let Some(rest) = cmd.strip_prefix("cargo run -p openbotd") {
            let _ = rest;
            continue;
        }
        // `cat x | openbot secret set y`: the interesting half is on the right.
        if let Some((_, right)) = cmd.rsplit_once('|') {
            cmd = right.trim();
        }
        if cmd.starts_with("openbot ") {
            out.push(cmd.to_owned());
        } else if cmd.starts_with("--") || cmd.starts_with("-") {
            // A continued line: `openbot connector add ... \` then its flags. The
            // shell joins these; reading only the first line makes a complete
            // command look like it is missing its arguments.
            if let Some(prev) = out.last_mut() {
                prev.push(' ');
                prev.push_str(cmd);
            }
        }
    }
    out
}

/// Split a command line, honouring double quotes so `--title "a b"` is one arg.
///
/// A trailing `# comment` is dropped: the README annotates its examples, and
/// passing `#` and the words after it as arguments makes a valid command look
/// broken. Only outside quotes; a `#` can legitimately appear in a value.
fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        if c == '#' && !quoted {
            break;
        }
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            // A line continuation is not an argument.
            '\\' if !quoted => {}
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The subcommand path: leading non-flag words, stopping at the first flag or
/// free-text argument that clap would treat as a value.
fn subcommand_path(args: &[String], known: &BTreeSet<String>) -> Vec<String> {
    let mut path = Vec::new();
    for a in args {
        if a.starts_with('-') {
            break;
        }
        let candidate = {
            let mut p = path.clone();
            p.push(a.clone());
            p.join(" ")
        };
        if known.contains(&candidate) {
            path.push(a.clone());
        } else {
            break;
        }
    }
    path
}

/// Subcommands and sub-subcommands this binary actually has, from its own help.
fn known_subcommands() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for top in help_commands(&[]) {
        for sub in help_commands(std::slice::from_ref(&top)) {
            out.insert(format!("{top} {sub}"));
        }
        out.insert(top);
    }
    out
}

fn help(args: &[String]) -> String {
    let out = Command::new(OPENBOT)
        .args(args)
        .arg("--help")
        .env("NO_COLOR", "1")
        .output()
        .expect("openbot --help");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Command names listed in a `--help` output's Commands section.
fn help_commands(args: &[String]) -> Vec<String> {
    let text = help(args);
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("Commands:") {
            inside = true;
            continue;
        }
        if inside {
            if line.trim().is_empty() || !line.starts_with("  ") {
                break;
            }
            if let Some(name) = line.split_whitespace().next() {
                if name != "help" {
                    out.push(name.to_owned());
                }
            }
        }
    }
    out
}

#[test]
fn every_readme_command_is_one_the_binary_accepts() {
    let md = readme();
    let known = known_subcommands();
    assert!(
        known.contains("bot new"),
        "could not read the binary's own subcommands; got {known:?}"
    );

    let lines = openbot_invocations(&md);
    assert!(
        lines.len() > 10,
        "only found {} openbot commands in the README; the parser is probably broken",
        lines.len()
    );

    let mut problems = Vec::new();
    for line in &lines {
        let args = split(line);
        let rest = &args[1..]; // drop "openbot"
        let path = subcommand_path(rest, &known);
        // `openbot` on its own is a command now: it reports what is configured
        // and, when nothing is, offers a model already running on the machine.
        // Before that it printed the subcommand list, so a bare invocation in
        // the README really was a mistake and this check really did want to
        // catch it.
        if rest.iter().all(|a| a.starts_with('-')) {
            continue;
        }
        if path.is_empty() {
            problems.push(format!("`{line}`: no such subcommand"));
            continue;
        }

        let text = help(&path);
        if text.is_empty() {
            problems.push(format!("`{line}`: `{}` has no help", path.join(" ")));
            continue;
        }

        // If the path stopped at a command that has subcommands, the next
        // word has to be one of them. Without this a renamed subcommand slips
        // through as an innocent-looking positional argument: `openbot bot
        // lsx` parsed as "the bot command, with an argument".
        let subs = help_commands(&path);
        if !subs.is_empty() {
            if let Some(next) = rest[path.len()..].iter().find(|a| !a.starts_with('-')) {
                if !subs.contains(next) {
                    problems.push(format!(
                        "`{line}`: `{}` has no `{next}` (it has: {})",
                        path.join(" "),
                        subs.join(", ")
                    ));
                    continue;
                }
            }
        }
        for a in rest {
            let Some(flag) = a.strip_prefix("--") else {
                continue;
            };
            // `--` alone, and `--flag=value` forms.
            let name = flag.split('=').next().unwrap_or(flag);
            if name.is_empty() {
                continue;
            }
            if !text.contains(&format!("--{name}")) {
                problems.push(format!("`{line}`: `{}` has no `--{name}`", path.join(" ")));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the README documents commands this binary does not have:\n  {}",
        problems.join("\n  ")
    );
}

/// Every relative link in the README has to resolve inside the repository.
///
/// A link such as `[../SPEC.md](../SPEC.md)` points outside the repository
/// root, so it is fine on the machine it was written on and broken for
/// everyone who clones: the failure mode of a link nothing checks.
#[test]
fn every_relative_link_in_the_readme_resolves() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let md = readme();

    let mut missing = Vec::new();
    let mut rest = md.as_str();
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let Some(end) = after.find(')') else { break };
        let target = &after[..end];
        rest = &after[end..];

        // Only relative file links: anchors and URLs are somebody else's
        // problem, and checking them would need a network.
        if target.starts_with("http") || target.starts_with('#') || target.is_empty() {
            continue;
        }
        let path = target.split('#').next().unwrap_or(target);
        let joined = root.join(path);
        if !joined.exists() {
            missing.push(format!("{target} (looked for {})", joined.display()));
        }
        // A link that escapes the repository is broken for anyone who clones
        // it, however real the file is on this machine.
        if path.starts_with("..") {
            missing.push(format!("{target} points outside the repository"));
        }
    }

    assert!(
        missing.is_empty(),
        "the README links to things a clone will not have:\n  {}",
        missing.join("\n  ")
    );
}

/// The README's examples work in the order a reader meets them.
///
/// An example that assumes a Bot created off-page (`openbot bot send Writer
/// ...` with no `Writer`) fails for someone following along, who then
/// concludes the project is broken.
///
/// This replays every command that only touches local state, in page order,
/// against one fresh home. Commands needing a hub or a model are skipped: they
/// are covered in `cli_live.rs`, and a README test that needs an API key is a
/// README test nobody runs.
#[test]
fn the_examples_work_in_the_order_they_are_written() {
    let home = tempfile::tempdir().unwrap();
    let md = readme();
    let known = known_subcommands();

    // Local-state commands only. `run`, `group post`, `watch`, `call`, `tools`
    // and `servers` all need something running.
    const NEEDS_MORE: &[&str] = &[
        "run",
        "watch",
        "call",
        "tools",
        "servers",
        "up",
        "event",
        "group post",
        "computer",
        // `connector add` verifies against the real endpoint before saving,
        // which is the right behaviour and makes it untestable offline.
        // `connector test` asks the same endpoint, so it is offline-bound too.
        "connector add",
        "connector test",
    ];

    let mut ran = 0;
    for line in openbot_invocations(&md) {
        let args = split(&line);
        let rest = &args[1..];
        let path = subcommand_path(rest, &known).join(" ");
        if path.is_empty() || NEEDS_MORE.iter().any(|n| path.starts_with(n)) {
            continue;
        }
        // `secret set` reads stdin; covered in cli.rs.
        if path == "secret set" {
            continue;
        }

        let (cmd, tail) = rest.split_first().unwrap();
        let out = std::process::Command::new(OPENBOT)
            .arg(cmd)
            .arg("--home")
            .arg(home.path())
            .args(tail)
            .env("NO_COLOR", "1")
            .env_remove("OPENBOT_HOME")
            .output()
            .expect("run openbot");

        assert!(
            out.status.success(),
            "`{line}` fails for someone following the README from the top:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        ran += 1;
    }

    assert!(
        ran >= 8,
        "only replayed {ran} commands; the filter is probably eating the page"
    );
}
