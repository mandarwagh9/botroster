//! `botroster-guest`: the guest tool server.
//!
//! Runs inside the computer. Connects out to the hub, advertises its tool set,
//! and executes what the hub routes to it.

use std::sync::Arc;

use botroster_guest::{tools, Context, GuestConfig, Workspace};
use botroster_proto::{ConnectionKind, PROTOCOL_VERSION};
use clap::Parser;

/// How long to linger after a failed hub connection before exiting, so a
/// supervising host can observe the failed state instead of seeing a bare
/// restart loop.
const HUB_CONNECT_FAILED_DWELL: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Parser, Debug)]
#[command(name = "botroster-guest", about = "botroster guest tool server")]
struct Args {
    /// Print the capability manifest as JSON and exit 0.
    ///
    /// A build predating a capability rejects the corresponding flag and exits
    /// non-zero, giving the supervisor a definitive feature probe rather than a
    /// version-string guess.
    #[arg(long)]
    capabilities: bool,

    #[arg(
        long,
        env = "BOTROSTER_HUB_URL",
        default_value = "ws://127.0.0.1:8443/v1/tools"
    )]
    hub_url: String,

    /// Stable identity for `servers.list` and `session_bind_server`.
    #[arg(long, default_value = "botroster-workspace")]
    server_id: String,

    /// Workspace root. Everything durable lives under here.
    ///
    /// Ignored when `--store` is given: a managed volume owns its own path.
    #[arg(long, env = "BOTROSTER_WORKSPACE", default_value = "./workspace")]
    workspace: std::path::PathBuf,

    /// Back the workspace with a durable store at this root.
    ///
    /// The guest becomes replaceable: its data lives in the volume, survives
    /// the process being rebuilt, and can be rolled back independently.
    #[arg(long, env = "BOTROSTER_STORE")]
    store: Option<std::path::PathBuf>,

    /// Volume to attach within `--store`. Defaults to the server id, so one
    /// guest identity maps to one computer.
    #[arg(long, env = "BOTROSTER_VOLUME")]
    volume: Option<String>,

    /// Confine path resolution to the workspace root: reject `..`,
    /// absolute-outside-root, and symlink escapes.
    ///
    /// On by default: the guest backs a remote workspace, which is a tenant
    /// boundary. Turn it off only for local development.
    #[arg(long, env = "BOTROSTER_CONFINE_FS", default_value_t = true, action = clap::ArgAction::Set)]
    confine_fs_to_workspace_root: bool,

    /// Permit plaintext `ws://` to a non-loopback host.
    ///
    /// Only for a mesh-secured transport; otherwise the bearer crosses the
    /// network in the clear.
    #[arg(long)]
    allow_insecure_ws: bool,
}

fn capability_manifest(args: &Args) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "binary_version": env!("CARGO_PKG_VERSION"),
        "server_id": args.server_id,
        "connection_kind": ConnectionKind::ToolServer,
        "methods": ["session.bind", "session.unbind", "tool_call_request", "ping"],
        "tools": tools::catalog().iter().map(|t| t.tool_id.as_str()).collect::<Vec<_>>(),
    })
}

/// Refuse plaintext transport to a non-loopback host unless explicitly allowed.
fn check_url(hub_url: &str, allow_insecure: bool) -> anyhow::Result<()> {
    let insecure = hub_url.starts_with("ws://");
    let loopback =
        hub_url.contains("localhost") || hub_url.contains("127.0.0.1") || hub_url.contains("[::1]");
    if insecure && !loopback && !allow_insecure {
        anyhow::bail!(
            "refusing plaintext ws:// to a non-loopback host ({hub_url}); \
             pass --allow-insecure-ws only on a mesh-secured transport"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "botroster_guest=info".into()),
        )
        .init();

    if args.capabilities {
        println!(
            "{}",
            serde_json::to_string_pretty(&capability_manifest(&args))?
        );
        return Ok(());
    }

    check_url(&args.hub_url, args.allow_insecure_ws)?;

    // A managed volume owns the path; otherwise use the bare directory.
    // The attach guard is held for the whole run: while it exists a restore
    // is refused rather than swapping the workspace out from under a live
    // guest, which fails differently on each platform.
    let mut _attached = None;
    let (root, profile, volume_note) = match &args.store {
        Some(store_root) => {
            let store = botroster_store::Store::open(store_root)?;
            let id = args
                .volume
                .clone()
                .unwrap_or_else(|| args.server_id.clone());
            let vol = store.volume(&id)?;
            _attached = Some(vol.attach().map_err(|e| {
                anyhow::anyhow!(
                    "cannot attach volume `{id}`: {e}\n\
                     If no guest is running, clear it with `botroster computer force-detach`."
                )
            })?);
            let snaps = vol.snapshots()?.len();
            (
                vol.workspace(),
                vol.browser_profile(),
                format!(" (volume `{id}`, {snaps} snapshots)"),
            )
        }
        None => {
            // Beside the workspace, never inside it; see `profile_beside`.
            let ws = args.workspace.clone();
            let profile = botroster_guest::profile_beside(&ws);
            (ws, profile, String::new())
        }
    };
    // The profile is durable so signed-in sessions survive a rebuild, and it
    // lives outside the workspace so `fs.read` (allow-listed, no approval
    // prompt) cannot read its cookies and Login Data.
    let ws = Arc::new(Context::new(
        Workspace::new(&root, args.confine_fs_to_workspace_root)?,
        &profile,
    ));
    tracing::info!(
        hub = %args.hub_url,
        workspace = %format!("{}{}", ws.ws.root().display(), volume_note),
        confined = args.confine_fs_to_workspace_root,
        tools = tools::catalog().len(),
        "guest starting"
    );
    if !args.confine_fs_to_workspace_root {
        tracing::warn!(
            "filesystem confinement is OFF — the guest can read and write outside the workspace"
        );
    }

    let cfg = GuestConfig {
        hub_url: args.hub_url.clone(),
        server_id: args.server_id.clone(),
        description: "botroster guest workspace".into(),
        // A guest started on its own — the split-deployment case — is told the
        // hub's token the same way it is told the hub's address: through the
        // environment. `None` here falls back to that inside `Hello`.
        token: None,
    };

    // `run` only returns when the hub connection ends, so a stop signal must
    // race it; otherwise the process exits with its browser still running.
    let result = tokio::select! {
        r = botroster_guest::run_supervised(cfg, Arc::clone(&ws)) => r,
        _ = botroster_guest::stop_signal() => {
            tracing::info!("stopping");
            Ok(())
        }
    };
    // Always tear the browser down: a headless Chrome outliving its guest is
    // an orphaned process holding a profile lock the next guest needs.
    ws.shutdown().await;

    if let Err(e) = result {
        tracing::error!(error = %e, "failed to connect workspace to hub");
        // Dwell so the host can observe the failed state before exit.
        tokio::time::sleep(HUB_CONNECT_FAILED_DWELL).await;
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_to_a_remote_host_is_refused() {
        assert!(check_url("ws://evil.example.com/v1/tools", false).is_err());
    }

    #[test]
    fn plaintext_to_loopback_is_allowed() {
        assert!(check_url("ws://127.0.0.1:8443/v1/tools", false).is_ok());
        assert!(check_url("ws://localhost:8443/v1/tools", false).is_ok());
    }

    #[test]
    fn tls_to_a_remote_host_is_allowed() {
        assert!(check_url("wss://hub.example.com/v1/tools", false).is_ok());
    }

    #[test]
    fn the_escape_hatch_works_but_must_be_explicit() {
        assert!(check_url("ws://evil.example.com/v1/tools", true).is_ok());
    }
}
