//! What a shell command is allowed to see.
//!
//! `shell.exec` is the most powerful tool the guest offers, and it used to hand
//! the child this process's entire environment: `tokio::process::Command`
//! inherits by default and nothing said otherwise.
//!
//! `openbot up` runs the hub, the credential store and the guest in one
//! process, so on the documented install — export `XAI_API_KEY`, then run — a
//! model-chosen command could print the model key with `env`.
//!
//! `crates/openbot-guest/tests/isolation.rs` panics with "this is the reason a
//! prompt injection cannot exfiltrate a credential". That invariant is real and
//! it is a property of `Cargo.toml`: no crate edge means no `use`. It says
//! nothing about a process that inherits the secrets anyway.

use std::sync::Arc;

use openbot_guest::{Context, Workspace};
use serde_json::json;

/// Serialises the tests that write to the process environment.
///
/// `set_var` is process-global, and `cargo test` runs the tests in a binary on
/// separate threads at once. Two of these set variables and a third reads what
/// the child received, so without this they race — and the failure would be
/// intermittent and would look like the fix rather than the test. The same
/// mistake, made in `browser.rs`, is in `.claude/product-review/MEMORY.md`.
/// Async-aware, because the guard is held across the `await` on the command:
/// a `std::sync::Mutex` held over one blocks the whole runtime worker, which
/// clippy refuses and is right to.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take the lock. `tokio`'s mutex has no poisoning, so a panicking test simply
/// releases it.
async fn env_guard() -> tokio::sync::MutexGuard<'static, ()> {
    ENV.lock().await
}

fn ctx_for(dir: &std::path::Path) -> Arc<Context> {
    Arc::new(Context::new(
        Workspace::new(dir, true).unwrap(),
        dir.join(".browser"),
    ))
}

async fn call(
    ctx: &Arc<Context>,
    tool: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    openbot_guest::tools::invoke(ctx, tool, args, &mut |_| {})
        .await
        .map_err(|e| e.to_string())
}

/// Print one variable, in whichever shell the guest will use.
fn echo_var(name: &str) -> String {
    if cfg!(windows) {
        format!("echo %{name}%")
    } else {
        format!("echo ${name}")
    }
}

/// A shell command cannot read the credentials the guest is meant not to have.
///
/// `crates/openbot-guest/tests/isolation.rs` panics with "the guest can now
/// reach the credential store... this is the reason a prompt injection cannot
/// exfiltrate a credential". That invariant is real and it is a property of
/// `Cargo.toml`: no crate edge means no `use`. It said nothing about a process
/// that inherited the secrets anyway — and `openbot up` runs the hub, the
/// credential store and the guest in a single process, so on the documented
/// install (export the key, run) `env` printed the model key to a model-chosen
/// command.
///
/// The variable is set on this process and the command is asked for it by name,
/// so a pass here means the child genuinely did not receive it rather than that
/// the test failed to look.
#[tokio::test]
async fn a_shell_command_does_not_inherit_this_process_s_credentials() {
    let _lock = env_guard().await;
    let ws = tempfile::tempdir().unwrap();
    let ctx = ctx_for(ws.path());

    // Set on the parent, the way an install that exports its key would.
    std::env::set_var("XAI_API_KEY", "sk-not-for-the-model");
    let out = call(
        &ctx,
        "shell.exec",
        &json!({ "command": echo_var("XAI_API_KEY") }),
    )
    .await
    .expect("shell.exec ran");
    std::env::remove_var("XAI_API_KEY");

    let stdout = out["stdout"].as_str().unwrap_or_default().to_owned();
    assert!(
        !stdout.contains("sk-not-for-the-model"),
        "the model key reached a model-chosen shell command: {stdout:?}"
    );
}

/// And a command that needs the ordinary environment still works.
///
/// The anti-vacuity half. An allow-list that passed nothing would satisfy the
/// test above and would break every shell command in the product: `PATH` is
/// what lets a command find the binary it names. It is inherited rather than
/// fixed on purpose — the parent was started from the person's own shell, so
/// its `PATH` already carries whatever their profile set up.
#[tokio::test]
async fn a_shell_command_can_still_find_the_programs_it_names() {
    let _lock = env_guard().await;
    let ws = tempfile::tempdir().unwrap();
    let ctx = ctx_for(ws.path());

    // `cd` is a shell builtin and would pass even with no PATH at all, so this
    // names a real program that has to be found on disk.
    let cmd = if cfg!(windows) {
        "where cmd"
    } else {
        "ls -d ."
    };
    let out = call(&ctx, "shell.exec", &json!({ "command": cmd }))
        .await
        .expect("shell.exec ran");
    assert_eq!(
        out["exit_code"].as_i64(),
        Some(0),
        "a command that needs PATH failed, so the allow-list is too tight to be usable: {out:#}"
    );
}

/// A variable named in `OPENBOT_SHELL_ENV` is passed through.
///
/// The escape hatch has to work, or the answer to "my script needs
/// `GITHUB_TOKEN`" becomes "turn the whole thing off". Naming one variable is a
/// decision someone made; inheriting everything was not.
#[tokio::test]
async fn a_variable_named_in_the_passthrough_is_allowed_through() {
    let _lock = env_guard().await;
    let ws = tempfile::tempdir().unwrap();
    let ctx = ctx_for(ws.path());

    std::env::set_var("OPENBOT_TEST_PASSTHROUGH", "wanted-value");
    std::env::set_var("OPENBOT_SHELL_ENV", "OPENBOT_TEST_PASSTHROUGH");
    let out = call(
        &ctx,
        "shell.exec",
        &json!({ "command": echo_var("OPENBOT_TEST_PASSTHROUGH") }),
    )
    .await
    .expect("shell.exec ran");
    std::env::remove_var("OPENBOT_SHELL_ENV");
    std::env::remove_var("OPENBOT_TEST_PASSTHROUGH");

    let stdout = out["stdout"].as_str().unwrap_or_default().to_owned();
    assert!(
        stdout.contains("wanted-value"),
        "a variable the operator named explicitly did not reach the command: {stdout:?}"
    );
}
