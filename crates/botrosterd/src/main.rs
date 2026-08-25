//! `botrosterd`, the BOTROSTER control plane.
//!
//! Holds everything the guest must not: identity, the bot registry, the
//! conversation store, the approval engine, the routine scheduler, the
//! credential broker, and the computer orchestrator. It also terminates the
//! Computer Hub WebSocket and routes between harnesses and tool servers.
//!
//! See `docs/SPEC.md` §3 for the component map and §10 for the build order.

use std::sync::Arc;

use botroster_proto::PROTOCOL_VERSION;
use clap::Parser;

use botrosterd::server::Server;

#[derive(Parser, Debug)]
#[command(name = "botrosterd", about = "botroster control plane")]
struct Args {
    /// Address for the hub WebSocket listener. Port 0 binds an ephemeral port.
    #[arg(long, env = "BOTROSTER_BIND", default_value = "127.0.0.1:8443")]
    bind: String,

    /// OIDC issuer used to authenticate connections at upgrade time.
    #[arg(long, env = "BOTROSTER_OIDC_ISSUER")]
    oidc_issuer: Option<String>,

    /// Where Bot profiles, conversations and inboxes live.
    ///
    /// The hub serves `bot.list` and `bot.send` from here, so a Bot can hand
    /// work to another mid-run and the handoff passes the approval gate like
    /// any other action.
    #[arg(long, env = "BOTROSTER_HOME", default_value = "./botroster-data")]
    home: std::path::PathBuf,

    /// What to do when an approval hook fails to answer.
    ///
    /// `closed` is the default. A fail-open hook contract, where a timeout,
    /// crash, or malformed response lets the call proceed, is unsafe for an
    /// unattended agent holding live sessions. See `docs/SPEC.md` §6.
    #[arg(long, env = "BOTROSTER_HOOK_FAILURE", default_value = "closed")]
    hook_failure: HookFailurePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum HookFailurePolicy {
    /// Deny the tool call and surface it for review. Default.
    Closed,
    /// Allow the call through. Opt-in, for genuinely advisory hooks only.
    Open,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "botrosterd=info".into()),
        )
        .init();

    if args.oidc_issuer.is_none() {
        tracing::warn!("no --oidc-issuer set; every connection gets the development principal");
    }
    if args.hook_failure == HookFailurePolicy::Open {
        tracing::warn!(
            "hook failures are set to FAIL OPEN — a crashed or timed-out approval hook \
             will let tool calls through. Do not run unattended like this."
        );
    }

    // Bind before discovery. The port is then claimed and early connections
    // queue in the accept backlog instead of being refused, so a slow
    // integration delays the first tool call rather than breaking the boot.
    let (listener, local) = Server::bind(&args.bind).await?;

    let booted =
        botrosterd::boot::hub_from_home(&args.home, botrosterd::policy::Policy::default()).await?;
    let hub = booted.hub;
    tracing::info!(home = %args.home.display(), "serving bot.list and bot.send");
    if !booted.connector_tools.is_empty() {
        tracing::info!(tools = ?booted.connector_tools, "connector tools available");
    }
    if booted.hook_count > 0 {
        tracing::info!(hooks = booted.hook_count, "PreToolUse hooks armed");
    }
    if booted.skill_count > 0 {
        tracing::info!(skills = booted.skill_count, "skills available to look up");
    }
    tracing::info!(addr = %local, protocol = PROTOCOL_VERSION, "hub listening");

    let server = Arc::new(Server::new(hub));
    tokio::select! {
        _ = Arc::clone(&server).serve(listener) => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
