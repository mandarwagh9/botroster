//! What the shell that launched the window can still decide for a Bot.
//!
//! The window spawns `botroster acp` for every Bot, and the agent-client-protocol
//! SDK offers no `env_remove` (only additive `env(name, value)`), so that
//! child inherits the window's whole environment. Every `BOTROSTER_*` variable the
//! CLI reads is therefore a channel from the shell that started the app into a
//! Bot's model connection, and the window closes exactly two of them: it sets
//! `BOTROSTER_HUB_URL` explicitly and passes `--home`.
//!
//! Both of those rest on one assumption, that a flag beats the environment.
//! That is clap's behaviour, not this codebase's. `config.rs` proves that
//! flags override the file, but it builds `ModelOverrides` by hand; that an
//! environment variable becomes such a flag is `main.rs`'s `env = "..."`
//! wiring, and these tests join the two.
//!
//! These read the shipped binary, because that is the only thing that can
//! answer either question.

mod common;

use std::collections::BTreeSet;
use std::process::Command;

fn botroster(ambient_home: Option<&std::path::Path>, args: &[&str]) -> String {
    let mut cmd = Command::new(common::up::botroster());
    cmd.args(args).env("NO_COLOR", "1");
    match ambient_home {
        Some(p) => cmd.env("BOTROSTER_HOME", p),
        None => cmd.env_remove("BOTROSTER_HOME"),
    };
    let out = cmd.output().expect("the shipped binary runs");
    assert!(
        out.status.success(),
        "`botroster {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An explicit `--home` beats an ambient `BOTROSTER_HOME`.
///
/// The window passes `--home` to every child it spawns, including the agent,
/// while the environment it inherited may name a different one. If the
/// environment won instead, a Bot would read and write another home (its
/// conversations, secrets and roster) while the window went on showing the
/// home it thought it had chosen. Nothing about that failure is visible from
/// inside the window.
///
/// The second half keeps this from being vacuous. "The flag won" and "the
/// variable is ignored" look identical from one assertion, and only the first
/// is the property relied on: with no flag the ambient home is used, so the
/// variable is a live channel and the flag is really closing it.
#[test]
fn an_explicit_home_beats_an_ambient_one() {
    let flagged = tempfile::tempdir().expect("a temp dir");
    let ambient = tempfile::tempdir().expect("a temp dir");

    for (home, name) in [(&flagged, "flag-bot"), (&ambient, "ambient-bot")] {
        botroster(
            None,
            &["bot", "new", name, "--home", home.path().to_str().unwrap()],
        );
    }

    let with_flag = botroster(
        Some(ambient.path()),
        &[
            "bot",
            "ls",
            "--json",
            "--home",
            flagged.path().to_str().unwrap(),
        ],
    );
    assert!(
        with_flag.contains("flag-bot") && !with_flag.contains("ambient-bot"),
        "an ambient BOTROSTER_HOME changed which home an explicit --home selected: {with_flag}"
    );

    let without_flag = botroster(Some(ambient.path()), &["bot", "ls", "--json"]);
    assert!(
        without_flag.contains("ambient-bot") && !without_flag.contains("flag-bot"),
        "BOTROSTER_HOME had no effect even without --home, so the check above is vacuous: {without_flag}"
    );
}

/// The two children the window does not scrub have no home to scrub.
///
/// Every other place this crate shells out to `botroster` calls
/// `env_remove("BOTROSTER_HOME")`; `hub::reach` and `viewer::open` do not,
/// because `botroster tools` and `botroster watch` declare no home argument, so an
/// ambient value has nothing to reach. Asserted rather than commented, so
/// that if one of them grows a `--home` the omission fails a test.
///
/// The `BOTROSTER_HUB_URL` check is what makes the absence meaningful: it proves
/// the help renders env annotations for these subcommands, so `BOTROSTER_HOME`
/// not being there is a fact about the command rather than about the output.
#[test]
fn the_children_the_window_does_not_scrub_read_no_home() {
    for sub in ["tools", "watch"] {
        let help = botroster(None, &[sub, "--help"]);
        assert!(
            help.contains("[env: BOTROSTER_HUB_URL"),
            "`botroster {sub} --help` shows no env annotations at all, so this test cannot tell \
             a command that reads no home from output that lists no variables"
        );
        assert!(
            !help.contains("BOTROSTER_HOME"),
            "`botroster {sub}` now reads BOTROSTER_HOME, and the window spawns it without scrubbing \
             or passing one; see `hub::reach` and `viewer::open`"
        );
    }
}

/// Every environment variable a Bot's agent can read, listed explicitly.
///
/// The window cannot scrub this child, so each name here is something the
/// shell that launched the app can still decide about a Bot. The two that
/// matter most are already shut: `BOTROSTER_HUB_URL` is set explicitly by the
/// window and `BOTROSTER_HOME` is overridden by `--home`, proven above. What is
/// left is model configuration, which is inherited intentionally: the window
/// offers no model UI, so the environment and the home's `config.toml` are
/// the only places it can come from.
///
/// A new name failing this test is not necessarily a bug. It means the set of
/// things an outside environment can change about a Bot has widened, and the
/// question is whether the window should be setting it instead.
#[test]
fn every_environment_variable_the_agent_can_read_is_recorded() {
    const ACCOUNTED_FOR: &[(&str, &str)] = &[
        (
            "BOTROSTER_HUB_URL",
            "shut: the window sets this on the child explicitly",
        ),
        (
            "BOTROSTER_HOME",
            "shut: the window passes --home, which beats it",
        ),
        ("BOTROSTER_MODEL", "inherited: model configuration"),
        ("BOTROSTER_DIALECT", "inherited: model configuration"),
        ("BOTROSTER_BASE_URL", "inherited: model configuration"),
        (
            "BOTROSTER_API_KEY_ENV",
            "inherited: names the key's variable",
        ),
        ("BOTROSTER_TOKEN_BUDGET", "inherited: model configuration"),
    ];

    let help = botroster(None, &["acp", "--help"]);
    let mentions = help.matches("[env: ").count();
    let names: Vec<&str> = help
        .match_indices("[env: ")
        .map(|(at, m)| {
            let rest = &help[at + m.len()..];
            &rest[..rest
                .find(['=', ']'])
                .expect("clap closes the env annotation")]
        })
        .collect();

    // One name per annotation, so a change to how clap renders help fails as
    // itself rather than as a variable that has gone missing. Counted before
    // de-duplicating: two args sharing a variable is fine and must not look
    // like an extraction that lost one.
    assert_eq!(
        names.len(),
        mentions,
        concat!(
            "`botroster acp --help` shows {} env annotations but {} names came out of them; ",
            "clap's help changed shape and this test no longer reads what it says it reads"
        ),
        mentions,
        names.len()
    );

    let declared: BTreeSet<&str> = names.into_iter().collect();
    let recorded: BTreeSet<&str> = ACCOUNTED_FOR.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        declared, recorded,
        "the environment a Bot's agent can read is not the one recorded here"
    );
}

/// Every `botroster` child the window points at a hub is given the token.
///
/// This test exists because the rule was applied at four call sites and there
/// were five. `hub::reach`, `viewer::open` and the agent were wired by hand;
/// `attach::put` was missed, and the failure was a live test refusing to attach
/// a file — one screen away from being a shipped defect that only appears once
/// a hub requires a token.
///
/// The lesson recorded three times in this project's history is that a rule
/// stated in one place and applied by hand everywhere else is applied at n-1
/// places. So the rule is checked rather than remembered: any command built in
/// this crate that names `--hub` must also name the token variable.
///
/// Read from source rather than by running the binaries, because the property
/// is about construction, not behaviour. A behavioural version could only see
/// the sites a test happens to drive, which is exactly how the fifth one was
/// missed. `botrosterd/src/hub.rs` does the same for a different rule and
/// `messages.rs` for another; a source sweep is how this repository checks the
/// shape of code it cannot reach at runtime.
#[test]
fn every_child_pointed_at_a_hub_is_given_the_token_for_it() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;

    for entry in std::fs::read_dir(&src).expect("the crate has a src directory") {
        let path = entry.expect("a readable entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        let file = path.file_name().expect("a file name").to_string_lossy();

        // One chunk per command under construction: everything from the
        // constructor to the next one. Crude on purpose — a parser would be a
        // second thing to maintain, and the failure mode of the crude version
        // is a false alarm that a person reads, not a silent pass.
        let mut chunks: Vec<&str> = Vec::new();
        for marker in ["Command::new(", "AcpAgentConfig::new("] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(marker) {
                rest = &rest[at + marker.len()..];
                let end = rest.find(marker).unwrap_or(rest.len());
                chunks.push(&rest[..end]);
            }
        }

        for chunk in chunks {
            // Two ways this crate points a child at a hub: the flag, and the
            // variable the agent is given instead because the ACP SDK builds
            // its own argument list. `env_remove` of the same name is the
            // opposite and must not count. `up` is excluded because it *starts*
            // a hub and mints the token the others then present.
            let points_at_a_hub =
                chunk.contains(r#".arg("--hub")"#) || chunk.contains(r#".env("BOTROSTER_HUB_URL""#);
            if !points_at_a_hub || chunk.contains(r#".arg("up")"#) {
                continue;
            }
            checked += 1;
            assert!(
                chunk.contains("HUB_TOKEN_ENV"),
                "{file} builds a `botroster` child with --hub and never gives it \
                 HUB_TOKEN_ENV. A hub that requires a token will refuse it, and the person \
                 sees the window fail at whatever that child was for. See `hub::token_at`."
            );
        }
    }

    // Anti-vacuity. A pattern that matched nothing would pass this test for
    // ever, including on the day somebody renames the argument.
    assert!(
        checked >= 4,
        "only {checked} hub-pointed children were found in this crate's source; the scan \
         has stopped matching how commands are built and is no longer checking anything"
    );
}
