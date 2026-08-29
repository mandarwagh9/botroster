//! The hub's token, through the shipped binary.
//!
//! `botrosterd/tests/admission.rs` proves the hub refuses the wrong token. This
//! file proves the other half, which is the one that decides whether the change
//! is shippable: that a person running ordinary commands presents the right one
//! without knowing the token exists.
//!
//! The interesting case is `--home`. It is declared on each subcommand with
//! `env = "BOTROSTER_HOME"`, so the flag and the variable are one knob to a
//! person and two to `botroster_proto::hub_token`: passing `--home` on the
//! command line sets no variable, and before `use_home` the lookup read the
//! *default* home — presenting the wrong home's token to a hub started on the
//! named one, and being refused with a message about a token the person did in
//! fact have.

use std::process::Command;

const BOTROSTER: &str = env!("CARGO_BIN_EXE_botroster");

mod common;

use common::up::Up;

/// Run a command carrying `--home` and nothing else that could name a home or a
/// token, so only the flag can supply either.
fn with_home_flag(up: &Up, home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(BOTROSTER)
        .args(args)
        .arg("--home")
        .arg(home)
        .env("BOTROSTER_HUB_URL", &up.hub)
        .env("NO_COLOR", "1")
        // Both scrubbed on purpose. `common::up::run` sets `BOTROSTER_HOME`,
        // which `hub_token` consults directly — so a test that used it would
        // pass whether or not the flag worked, which is the thing being
        // measured.
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_TOKEN")
        .output()
        .expect("could not run botroster")
}

/// A command told which home to use presents that home's token.
#[test]
fn a_home_named_by_the_flag_alone_is_where_the_token_is_read_from() {
    let Some(up) = Up::start() else {
        return;
    };

    let out = with_home_flag(
        &up,
        &up.home,
        &["run", "--demo", "--approve", "auto", "prove it"],
    );
    assert!(
        out.status.success(),
        "`botroster run --home <the hub's home>` was refused, so a person passing --home cannot \
         reach their own computer.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The anti-vacuity half, and the one that proves the hub is really checking.
///
/// Without it, a hub that admitted everyone would pass the test above. Pointed
/// at a home that holds no token, the same command must be refused — and the
/// refusal has to name the file, because the overwhelmingly likely cause is not
/// an attacker but a second terminal addressing the wrong home.
#[test]
fn a_home_that_holds_no_token_is_refused_and_told_why() {
    let Some(up) = Up::start() else {
        return;
    };
    let elsewhere = tempfile::tempdir().expect("a home with nothing in it");

    let out = with_home_flag(
        &up,
        elsewhere.path(),
        &["run", "--demo", "--approve", "auto", "prove it"],
    );
    assert!(
        !out.status.success(),
        "a command addressing a home with no token reached the hub anyway, so the hub is not \
         checking:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains(botroster_proto::HUB_TOKEN_FILE),
        "the refusal does not name the file that holds the token, so a person cannot act on \
         it: {said}"
    );
}

/// The variable still works, and beats the flag.
///
/// This is how a client reaches a hub whose home it does not share — the
/// desktop window's children are given it this way, and it is the only route
/// open to somebody pointing Connect at another machine. A change that made the
/// flag win would close that route silently.
#[test]
fn the_variable_reaches_a_hub_whose_home_the_caller_does_not_share() {
    let Some(up) = Up::start() else {
        return;
    };
    let elsewhere = tempfile::tempdir().expect("a home with nothing in it");
    let (name, value) = up
        .token()
        .expect("`botroster up` writes a token into its home");

    let out = Command::new(BOTROSTER)
        .args(["run", "--demo", "--approve", "auto", "prove it"])
        .arg("--home")
        .arg(elsewhere.path())
        .env("BOTROSTER_HUB_URL", &up.hub)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env(name, value)
        .output()
        .expect("could not run botroster");

    assert!(
        out.status.success(),
        "the token in the environment did not reach a hub whose home this command does not \
         share, so a window pointed at another machine has no way in.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A hub this command started is the one it talks to, whatever the environment
/// says about some other hub.
///
/// Measured before it was fixed: with `BOTROSTER_HUB_TOKEN` exported — which is
/// how a window reaches a hub on another machine, so people will have it set —
/// and nothing listening, `run` started a computer, generated its token,
/// presented the stale ambient one, and was refused by the hub it had just
/// created. The message blamed the home, which was the one thing the command
/// had right.
///
/// No `Up` here on purpose: the case only exists when this process is the one
/// that starts the hub.
#[test]
fn a_hub_this_command_started_outranks_a_stale_token_in_the_environment() {
    let home = tempfile::tempdir().expect("a home of its own");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port();

    let out = Command::new(BOTROSTER)
        .args(["run", "--demo", "--approve", "auto", "prove it"])
        .arg("--home")
        .arg(home.path())
        .env(
            "BOTROSTER_HUB_URL",
            format!("ws://127.0.0.1:{port}/v1/tools"),
        )
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        // The stale one. Nothing this command starts will ever have this token.
        .env(botroster_proto::HUB_TOKEN_ENV, "a token for some other hub")
        .output()
        .expect("could not run botroster");

    assert!(
        out.status.success(),
        "a stale BOTROSTER_HUB_TOKEN made this command refuse itself entry to the hub it had \
         just started.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
