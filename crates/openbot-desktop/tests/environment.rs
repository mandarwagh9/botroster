//! What the shell that launched the window can still decide for a Bot.
//!
//! The window spawns `openbot acp` for every Bot, and the agent-client-protocol
//! SDK offers no `env_remove` (only additive `env(name, value)`), so that
//! child inherits the window's whole environment. Every `OPENBOT_*` variable the
//! CLI reads is therefore a channel from the shell that started the app into a
//! Bot's model connection, and the window closes exactly two of them: it sets
//! `OPENBOT_HUB_URL` explicitly and passes `--home`.
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

fn openbot(ambient_home: Option<&std::path::Path>, args: &[&str]) -> String {
    let mut cmd = Command::new(common::up::openbot());
    cmd.args(args).env("NO_COLOR", "1");
    match ambient_home {
        Some(p) => cmd.env("OPENBOT_HOME", p),
        None => cmd.env_remove("OPENBOT_HOME"),
    };
    let out = cmd.output().expect("the shipped binary runs");
    assert!(
        out.status.success(),
        "`openbot {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An explicit `--home` beats an ambient `OPENBOT_HOME`.
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
        openbot(
            None,
            &["bot", "new", name, "--home", home.path().to_str().unwrap()],
        );
    }

    let with_flag = openbot(
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
        "an ambient OPENBOT_HOME changed which home an explicit --home selected: {with_flag}"
    );

    let without_flag = openbot(Some(ambient.path()), &["bot", "ls", "--json"]);
    assert!(
        without_flag.contains("ambient-bot") && !without_flag.contains("flag-bot"),
        "OPENBOT_HOME had no effect even without --home, so the check above is vacuous: {without_flag}"
    );
}

/// The two children the window does not scrub have no home to scrub.
///
/// Every other place this crate shells out to `openbot` calls
/// `env_remove("OPENBOT_HOME")`; `hub::reach` and `viewer::open` do not,
/// because `openbot tools` and `openbot watch` declare no home argument, so an
/// ambient value has nothing to reach. Asserted rather than commented, so
/// that if one of them grows a `--home` the omission fails a test.
///
/// The `OPENBOT_HUB_URL` check is what makes the absence meaningful: it proves
/// the help renders env annotations for these subcommands, so `OPENBOT_HOME`
/// not being there is a fact about the command rather than about the output.
#[test]
fn the_children_the_window_does_not_scrub_read_no_home() {
    for sub in ["tools", "watch"] {
        let help = openbot(None, &[sub, "--help"]);
        assert!(
            help.contains("[env: OPENBOT_HUB_URL"),
            "`openbot {sub} --help` shows no env annotations at all, so this test cannot tell \
             a command that reads no home from output that lists no variables"
        );
        assert!(
            !help.contains("OPENBOT_HOME"),
            "`openbot {sub}` now reads OPENBOT_HOME, and the window spawns it without scrubbing \
             or passing one; see `hub::reach` and `viewer::open`"
        );
    }
}

/// Every environment variable a Bot's agent can read, listed explicitly.
///
/// The window cannot scrub this child, so each name here is something the
/// shell that launched the app can still decide about a Bot. The two that
/// matter most are already shut: `OPENBOT_HUB_URL` is set explicitly by the
/// window and `OPENBOT_HOME` is overridden by `--home`, proven above. What is
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
            "OPENBOT_HUB_URL",
            "shut: the window sets this on the child explicitly",
        ),
        (
            "OPENBOT_HOME",
            "shut: the window passes --home, which beats it",
        ),
        ("OPENBOT_MODEL", "inherited: model configuration"),
        ("OPENBOT_DIALECT", "inherited: model configuration"),
        ("OPENBOT_BASE_URL", "inherited: model configuration"),
        ("OPENBOT_API_KEY_ENV", "inherited: names the key's variable"),
        ("OPENBOT_TOKEN_BUDGET", "inherited: model configuration"),
    ];

    let help = openbot(None, &["acp", "--help"]);
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
            "`openbot acp --help` shows {} env annotations but {} names came out of them; ",
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
