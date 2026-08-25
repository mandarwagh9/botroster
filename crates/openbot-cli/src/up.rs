//! `openbot up`: the hub and a computer, in one command.
//!
//! Runs the control plane and one guest in a single process, then prints the
//! one command to type next.
//!
//! # This is local mode, and it is not isolation
//!
//! The guest here is a task in this process, on this machine, as the current
//! user. That is the right shape for development and the wrong shape for
//! anything else: in a real deployment the guest runs inside its own computer,
//! elsewhere, and the hub reaches it over a socket exactly as it does here.
//! Nothing about the protocol changes; what changes is what is on the other
//! end of it. See the isolation warning in the README for what the local guest
//! does and does not contain.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openbotd::server::Server;

/// Where the two halves keep their state.
///
/// These are intentionally different directories. The control plane's home
/// holds `secrets.json`; the guest's workspace is inside the workspace, where
/// `fs.read` is allow-listed and prompts for nothing. Pointing one at the
/// other hands the model every stored credential. The guest refuses to start
/// on such a directory, but a command whose defaults collide would only reveal
/// that on the day someone stored their first secret.
pub struct Paths {
    pub home: PathBuf,
    /// `None` means use the durable volume, which is the default: the agent's
    /// files are then the ones `snapshot`, `restore` and `prune` operate on.
    ///
    /// An explicit path is a directory of the operator's choosing, outside the
    /// volume, with no snapshots.
    pub workspace: Option<PathBuf>,
}

impl Paths {
    /// Where the guest's files actually live, and whether that is durable.
    pub fn resolve(&self, server_id: &str) -> anyhow::Result<Workspace> {
        let Some(explicit) = &self.workspace else {
            let store = openbot_store::Store::open(&self.home)?;
            let volume = store.volume(server_id)?;
            return Ok(Workspace::Durable(volume));
        };
        Ok(Workspace::Plain(explicit.clone()))
    }

    /// What to tell someone whose files are in the pre-volume default
    /// directory.
    ///
    /// Earlier versions defaulted to a plain `./workspace` directory. With the
    /// default on the durable volume, a computer that was full under the old
    /// default looks empty, and saying nothing would read as data loss.
    /// Nothing is moved automatically: those are the person's files, and a
    /// command that silently relocates a workspace is worse than one that
    /// explains.
    pub fn legacy_note(&self) -> Option<String> {
        if self.workspace.is_some() {
            return None;
        }
        let old = Path::new(crate::DEFAULT_WORKSPACE);
        let has_files = std::fs::read_dir(old)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        has_files.then(|| {
            format!(
                "{} still has files in it. The computer now lives on the durable volume in \
                 --home, so those are not what the agent sees.\n  \
                 Copy them across, or pass --workspace {} to keep working there (no snapshots).",
                old.display(),
                old.display()
            )
        })
    }

    pub fn check(&self) -> anyhow::Result<()> {
        let Some(workspace) = &self.workspace else {
            return Ok(());
        };
        // Both paths in the same space, or neither. Canonicalise when both
        // exist and fall back to a lexical clean-up otherwise: a canonical
        // path on Windows carries a `\?\` prefix a lexical one does not, so
        // comparing one of each answers no to questions whose answer is yes.
        // The home usually does not exist yet (openbotd creates it), which is
        // exactly when this matters.
        let (a, b) = match (self.home.canonicalize(), workspace.canonicalize()) {
            (Ok(a), Ok(b)) => (a, b),
            _ => (lexical(&self.home), lexical(workspace)),
        };
        if a == b {
            anyhow::bail!(
                "--home and --workspace are the same directory ({}).\n  \
                 The first holds secrets.json; the second is inside the workspace, where \
                 fs.read needs no approval. They cannot be the same place.",
                self.home.display()
            );
        }
        // The workspace must not contain the home either, and this is the
        // likely case: `--home` defaults to `./openbot-data`, so
        // `openbot up --workspace .` (a reasonable way to say "work in this
        // directory") puts the token store one level inside the workspace. The
        // guest's own guard looks for `secrets.json` at its root, finds
        // nothing, and serves it; `fs.read` is allow-listed with no approval
        // prompt.
        //
        // The other nesting stays allowed and is safe: a workspace inside the
        // home cannot reach back out, because `..` is refused.
        if a.starts_with(&b) {
            anyhow::bail!(
                "--workspace ({}) contains --home ({}).\n  \
                 The home holds secrets.json, and everything under the workspace is \
                 readable by `fs.read` with no approval prompt — so the workspace guard would \
                 contain every stored credential. Point one of them somewhere else.",
                workspace.display(),
                self.home.display()
            );
        }
        Ok(())
    }
}

/// Where the guest's files live for this run.
pub enum Workspace {
    /// The live tree of a durable volume: snapshots, restore and prune all
    /// operate on exactly these files.
    Durable(openbot_store::Volume),
    /// A directory someone named. Nothing snapshots it.
    Plain(PathBuf),
}

impl Workspace {
    pub fn dir(&self) -> PathBuf {
        match self {
            Self::Durable(v) => v.workspace(),
            Self::Plain(p) => p.clone(),
        }
    }

    /// Where the browser keeps cookies and `Login Data`.
    ///
    /// Beside the workspace, never inside it: `fs.read` is allow-listed and
    /// prompts for nothing, so a profile within reach of it is a credential
    /// store the model can read. A durable volume already has a place for this
    /// and says so; a plain directory gets a sibling.
    pub fn browser_profile(&self) -> PathBuf {
        match self {
            Self::Durable(v) => v.browser_profile(),
            // Computed from the absolute root, not the argument. Taking the
            // parent of what somebody typed puts the profile inside the
            // workspace for `--workspace .`: its parent is `""`, so the
            // profile becomes the relative `.openbot-browser`, which the browser
            // resolves against the process's own directory, the workspace.
            // Cookies and `Login Data` would then sit where `fs.read` reads
            // without asking. A lexical check does not catch this, because
            // the canonical root carries a `\?\` prefix on Windows and the
            // relative profile does not.
            //
            // One computation, in the crate both callers depend on, so the
            // two callers cannot drift.
            Self::Plain(p) => openbot_guest::profile_beside(p),
        }
    }

    /// Claim the volume so nothing rolls it back underneath this guest.
    ///
    /// `restore` swaps the live directory, which goes wrong differently on
    /// every platform while a process is writing into it. Attaching here is
    /// what lets `restore` refuse while a guest is running.
    pub fn hold(&self) -> anyhow::Result<Option<openbot_store::Attached>> {
        match self {
            Self::Durable(v) => Ok(Some(v.attach()?)),
            // A plain workspace is locked too. Otherwise two `openbot up`
            // instances on one `--workspace` directory would both start and
            // both serve agents writing the same files. Opting out of
            // snapshots is not opting out of that.
            Self::Plain(p) => Ok(Some(openbot_store::hold_dir(p).map_err(|e| match e {
                // Worded here rather than reused: the store's own message says
                // "volume", and this is a directory somebody chose.
                openbot_store::StoreError::Attached => anyhow::anyhow!(
                    "another openbot is already working in {}.
  Stop it first, or pass a different --workspace.",
                    p.display()
                ),
                other => anyhow::anyhow!("could not claim {}: {other}", p.display()),
            })?)),
        }
    }
}

/// The path with `.` removed, and nothing asked of the filesystem.
///
/// Used for both sides of a comparison or neither: `check` canonicalises when
/// both paths exist and falls back to this when either does not, because a
/// canonical path on Windows carries a `\?\` prefix a lexical one does not,
/// and comparing one of each answers no to questions whose answer is yes.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The label every scheduled snapshot carries.
///
/// Retention keys off it, so the schedule can bound its own history without
/// ever reaching a snapshot somebody took by hand.
pub const SCHEDULED: &str = "scheduled";

pub struct Up {
    pub bind: String,
    pub paths: Paths,
    pub server_id: String,
    /// How often to snapshot the computer, or `None` to leave it to the person.
    pub snapshot_every: Option<std::time::Duration>,
    /// How many scheduled snapshots to keep.
    pub snapshot_keep: usize,
    /// How often to run whatever routines are due, or `None` to leave it to an
    /// external scheduler.
    pub routines_every: Option<std::time::Duration>,
}

/// Run the routines that are due, on a timer, for as long as this hub is up.
///
/// Without this, routines did not run. Not "ran unreliably" — the cron parsed,
/// `due` computed the right answer, `tick` executed correctly, and nothing
/// anywhere called `tick`. `up.rs` did not contain the word "routine". Someone
/// could create a 9am digest in the window, close the laptop, and never learn
/// that the feature had no clock; the desktop client starts the runtime by
/// running `openbot up` (`openbot-desktop/src/hub.rs`), so this was every
/// desktop user.
///
/// # Why this spawns the CLI instead of calling the code
///
/// `routine tick` says of itself: "Composable by design: point cron, systemd or
/// a container scheduler at this rather than baking a daemon into the CLI."
/// That decision is a good one and this does not overturn it — `up` becomes one
/// of those schedulers, which is the usage the sentence describes. `tick` is
/// still a plain command anyone can point cron at, and this timer is off in one
/// flag for people who do.
///
/// Running it as a child process is not a shortcut around extracting the code.
/// A routine run drives a model and a shell for minutes at a time and can hang
/// on either. In-process, a wedged routine holds a task inside the supervisor
/// that is also serving the hub; as a child it is isolable, killable, and its
/// panic is an exit code rather than an aborted runtime. That separation is
/// worth more than the process it costs once a minute.
///
/// Ticks never overlap, because the loop awaits the child before arming the
/// next interval. A routine that takes ten minutes delays the next check rather
/// than starting a second copy of itself, which is the safer of the two — the
/// only thing worse than a digest that does not arrive is two of them.
///
/// Failures are logged and the timer keeps going, matching
/// [`snapshot_on_a_timer`]: a model that was overloaded at 09:00 is a reason to
/// miss one firing, not to stop being a scheduler for the rest of the day.
/// The command line the timer runs.
///
/// Separated from the spawn so it can be asserted without starting a hub or a
/// model. `--approve deny` is stated rather than left to the default: `tick`
/// resolves `ask` to deny today because an unattended run cannot prompt, and
/// this is the caller that most needs that to stay true, so it says so itself.
fn tick_args(home: &Path, hub_url: &str) -> Vec<String> {
    // `--home` goes *after* `routine`, not before it. Unlike `--hub` it is not
    // a global argument, so the root-position form clap rejects outright:
    //
    //     $ openbot --home H routine tick
    //     error: unexpected argument '--home' found
    //
    // The first version of this built exactly that, and the unit test below
    // passed anyway, because asserting that a joined argument list contains the
    // substring "--home H" says nothing about whether clap will accept it in
    // that position. It was caught by running the built binary, which is what
    // CONTRIBUTING.md means by checking anything with a face in the shipped
    // binary rather than only in a library test. The test now runs the real
    // parser.
    vec![
        "routine".to_owned(),
        "--home".to_owned(),
        home.display().to_string(),
        "--hub".to_owned(),
        hub_url.to_owned(),
        "tick".to_owned(),
        "--approve".to_owned(),
        "deny".to_owned(),
    ]
}

fn routines_on_a_timer(
    exe: std::path::PathBuf,
    home: std::path::PathBuf,
    hub_url: String,
    every: std::time::Duration,
    token: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        // The first tick fires immediately. Unlike the snapshot timer, that is
        // wanted here: a hub coming back after downtime should find out what it
        // missed now rather than one interval from now, and `tick` reports a
        // missed count rather than replaying each firing.
        loop {
            tick.tick().await;
            let mut cmd = tokio::process::Command::new(&exe);
            cmd.args(tick_args(&home, &hub_url));
            // Told, not left to find it. This child would otherwise look in
            // the default home, which is the right place only when `--home`
            // did not name another one.
            cmd.env(openbot_proto::HUB_TOKEN_ENV, &token);
            cmd.kill_on_drop(true);
            match cmd.output().await {
                Ok(out) if out.status.success() => {
                    let said = String::from_utf8_lossy(&out.stdout);
                    let said = said.trim();
                    // "nothing due" is the common case and would otherwise fill
                    // the log with one line a minute forever.
                    if !said.is_empty() && said != "nothing due" {
                        tracing::info!(ran = %said, "routines");
                    }
                }
                Ok(out) => tracing::warn!(
                    code = ?out.status.code(),
                    stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                    "routine tick failed; will try again next interval"
                ),
                Err(e) => tracing::warn!(error = %e, "could not run routine tick"),
            }
        }
    })
}

/// Snapshot the volume on a timer, trimming only what this timer took.
///
/// A computer's work compounds, and an agent with a shell can wreck a week of
/// it; without a schedule the only recourse is a snapshot nobody remembered
/// to take. Content addressing makes the default affordable: a snapshot of an
/// unchanged workspace stores no file bodies, only a small manifest, so an
/// idle computer costs almost nothing to protect.
///
/// Failures are logged and the timer keeps going. A volume busy with somebody
/// else's restore is a reason to miss one tick, not to stop protecting the
/// computer for the rest of the day.
fn snapshot_on_a_timer(
    volume: openbot_store::Volume,
    every: std::time::Duration,
    keep: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        // The first tick fires immediately; the computer is empty then and a
        // snapshot of nothing is noise in the listing.
        tick.tick().await;
        loop {
            tick.tick().await;
            let volume = &volume;
            let taken = tokio::task::block_in_place(|| {
                let snap = volume.snapshot(SCHEDULED)?;
                let trimmed = volume.prune_labelled(SCHEDULED, keep)?;
                Ok::<_, openbot_store::StoreError>((snap, trimmed))
            });
            match taken {
                Ok((snap, trimmed)) => tracing::info!(
                    id = %snap.id, files = snap.files, trimmed,
                    "scheduled snapshot"
                ),
                Err(e) => tracing::warn!(error = %e, "scheduled snapshot skipped"),
            }
        }
    })
}

/// A hub to talk to, started here if there was not one already.
///
/// The two-terminal problem, removed rather than documented. `openbot up` in one
/// window and `openbot run` in another is an implementation detail — the hub and
/// the guest are separate processes in a real deployment — and it was the first
/// thing every new person hit. It was not even signposted: `run` failed with a
/// bare winsock errno whose remedy named `openbotd` and `openbot-guest`, two
/// binaries the documented install does not put on anyone's PATH, and did not
/// mention `openbot up`, which is the binary they had just typed.
///
/// An already-running hub is used as-is. That matters more than it looks: a
/// person who ran `up` deliberately has a computer with a browser profile, a
/// durable volume and possibly a takeover in progress, and a second stack
/// started underneath them would fight for the volume lock and win or lose at
/// random. Reachable means reachable — the probe completes a real handshake
/// rather than testing whether a port accepts, because a port that accepts and
/// then refuses the protocol is not a hub.
///
/// Otherwise one is started on an ephemeral port, for the length of the command.
/// Ephemeral rather than the default 8443 so it cannot collide with an `up` the
/// person starts a moment later, and so two of these can run at once.
///
/// The caller must hold the returned [`Running`] until the work is done and
/// then stop it: process exit runs no destructors, so the browser teardown has
/// to be explicit.
/// Say what could not be reached, and the one command that fixes it.
///
/// Every hub-dependent command used to fail with the bare transport error, and
/// on Windows that is `connect: IO error: No connection could be made because
/// the target machine actively refused it. (os error 10061)` — a line naming
/// neither the hub, nor its address, nor anything to do about it. Eight
/// commands printed exactly that and nothing else, byte-identical, and the
/// same string reached editors through the ACP bridge as `-32603 Internal
/// error`.
///
/// The crate already knew better in two places: `status` names the URL and
/// renders the failure as a row, and `watch` maps its *second* failure to
/// "is openbot-guest running?" twenty lines below a first failure it does not
/// map at all. One rule, written twice, applied nowhere else.
///
/// The underlying error is kept rather than replaced. A hub that is running
/// and refusing — a wrong address, a rejected upgrade — is a different problem
/// from one that is not there, and a remedy that assumed the common case would
/// send somebody to start a second hub beside the one already failing. So the
/// remedy is offered conditionally, in words that stay true either way.
pub fn unreachable(hub_url: &str, e: &impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "could not reach the computer at {hub_url}\n  {e}\n  \
         if nothing is running there, `openbot up` starts one"
    )
}

pub async fn hub_or_start(
    hub_url: &str,
    paths: Paths,
    server_id: &str,
    style: crate::render::Style,
) -> anyhow::Result<(String, Option<Running>)> {
    // Short, because this runs before every command that needs a hub and the
    // common answer on a fresh machine is "nothing is there". Long enough that
    // a loaded machine does not get a second stack started underneath it.
    const PROBE: std::time::Duration = std::time::Duration::from_millis(1500);
    if let Ok(Ok(_)) = tokio::time::timeout(PROBE, openbot_agent::HubClient::connect(hub_url)).await
    {
        return Ok((hub_url.to_owned(), None));
    }

    // Said out loud, because it takes a moment and silence during it reads as a
    // hang. Not said when a hub was already there, because then nothing
    // happened worth reporting.
    eprintln!("{}", style.dim("  starting a computer for this run…"));

    let running = Up {
        bind: "127.0.0.1:0".to_owned(),
        paths,
        server_id: server_id.to_owned(),
        // No scheduled snapshots and no routine timer: this stack lives for one
        // command. A snapshot every 30 minutes never fires, and firing the
        // routines of a person who asked for one task would be a surprise.
        snapshot_every: None,
        snapshot_keep: 0,
        routines_every: None,
    }
    .start()
    .await?;
    Ok((running.hub_url.clone(), Some(running)))
}

impl Up {
    /// Start the hub and one guest, and return once the guest has registered.
    ///
    /// Waiting for registration rather than printing optimistically: "ready"
    /// has to mean a tool call will work, or the first thing anyone types
    /// fails.
    pub async fn start(self) -> anyhow::Result<Running> {
        self.paths.check()?;

        // The policy this home configures, not a hardcoded default; this is
        // how `[permission]` in config.toml reaches the engine.
        let booted = openbotd::boot::hub_from_home(
            &self.paths.home,
            crate::config::policy(&self.paths.home)?,
        )
        .await?;
        // A secret for this home, written before anything can connect.
        //
        // Generated fresh on every start rather than kept: a token that
        // outlives the hub that issued it is a credential lying on disk for
        // whatever reads it next, and there is nothing to gain — every client
        // reads the file, so rotating it costs them nothing.
        //
        // Written before `serve` is spawned, so there is no window in which
        // the listener is up and the file a client would read is stale.
        // See `Hello::token` for what this defends and what it does not.
        let token = uuid::Uuid::new_v4().to_string();
        openbot_proto::write_hub_token(&self.paths.home, &token)?;

        let (listener, addr) = Server::bind(&self.bind).await?;
        let server = Arc::new(Server::new(Arc::clone(&booted.hub)));
        tokio::spawn(Arc::clone(&server).serve(listener));

        let hub_url = format!("ws://{addr}/v1/tools");

        let legacy = self.paths.legacy_note();
        let workspace = self.paths.resolve(&self.server_id)?;

        // Its own handle on the same volume: cheap, and it keeps the timer
        // from borrowing the one the guest is set up from.
        let snapshots = match (&workspace, self.snapshot_every) {
            (Workspace::Durable(_), Some(every)) => {
                let store = openbot_store::Store::open(&self.paths.home)?;
                Some(snapshot_on_a_timer(
                    store.volume(&self.server_id)?,
                    every,
                    self.snapshot_keep,
                ))
            }
            // A directory somebody named is not snapshotted, and there is
            // nowhere to put one.
            _ => None,
        };

        // Started after the hub is listening, because the child connects back
        // to it. `current_exe` rather than a name on PATH: the desktop client
        // ships this binary as a Tauri sidecar, where it is not on PATH at all
        // and is not called `openbot`.
        let routines = match (self.routines_every, std::env::current_exe()) {
            (Some(every), Ok(exe)) => Some(routines_on_a_timer(
                exe,
                self.paths.home.clone(),
                hub_url.clone(),
                every,
                token.clone(),
            )),
            (Some(_), Err(e)) => {
                // Worth a warning rather than a failed boot: everything else
                // this process does still works, and a hub that refuses to
                // start because it could not find itself is the worse outcome.
                tracing::warn!(error = %e, "cannot locate this binary; routines will not run on a timer");
                None
            }
            (None, _) => None,
        };
        // Held for the life of the process: while a guest is writing here,
        // nothing may roll the directory out from under it.
        let attached = workspace.hold()?;
        let dir = workspace.dir();
        std::fs::create_dir_all(&dir)?;

        let ctx = Arc::new(openbot_guest::Context::new(
            openbot_guest::Workspace::new(&dir, true)?,
            workspace.browser_profile(),
        ));
        let guest = Arc::clone(&ctx);
        let tools = openbot_guest::tools::catalog().len();
        let cfg = openbot_guest::GuestConfig {
            hub_url: hub_url.clone(),
            server_id: self.server_id.clone(),
            description: "openbot workspace (local mode)".into(),
            // Handed over rather than looked up. This guest runs in *this*
            // process, against whatever home `--home` named, and the lookup
            // inside `Hello` would read the default home — which is the right
            // answer only when they happen to be the same.
            token: Some(token.clone()),
        };
        tokio::spawn(async move {
            if let Err(e) = openbot_guest::run_supervised(cfg, ctx).await {
                tracing::error!(error = %e, "the guest stopped");
            }
        });

        wait_until_ready(&hub_url, &self.server_id).await?;

        Ok(Running {
            hub_url,
            addr,
            tools,
            connector_tools: booted.connector_tools,
            durable: matches!(workspace, Workspace::Durable(_)),
            workspace: dir,
            home: self.paths.home,
            guest,
            legacy,
            snapshot_every: snapshots.as_ref().and(self.snapshot_every),
            snapshots,
            routines_every: routines.as_ref().and(self.routines_every),
            routines,
            _attached: attached,
        })
    }
}

pub struct Running {
    pub hub_url: String,
    #[allow(dead_code)] // printed via hub_url; kept for callers that need the port
    pub addr: std::net::SocketAddr,
    /// How many tools the guest actually advertises, read from the catalogue
    /// rather than written down, so it cannot go stale.
    pub tools: usize,
    pub connector_tools: Vec<String>,
    pub workspace: PathBuf,
    /// Whether those files are on a durable volume, which decides whether
    /// `openbot computer snapshot` means anything for this run.
    pub durable: bool,
    pub home: PathBuf,
    /// Set when an old-style `./workspace` still holds files.
    pub legacy: Option<String>,
    /// How often the computer is being snapshotted, if it is.
    pub snapshot_every: Option<std::time::Duration>,
    snapshots: Option<tokio::task::JoinHandle<()>>,
    /// How often routines are being checked, if they are. `None` means an
    /// external scheduler owns it, which the banner has to say — a person who
    /// turned the timer off and a person whose routines silently never run see
    /// exactly the same thing otherwise, which is the bug this shipped with.
    pub routines_every: Option<std::time::Duration>,
    routines: Option<tokio::task::JoinHandle<()>>,
    /// Held so the browser can be torn down on Ctrl-C. Process exit runs no
    /// destructors, so `kill_on_drop` does not cover the way people actually
    /// stop this; it has to be done explicitly.
    guest: Arc<openbot_guest::Context>,
    /// Dropped last, releasing the volume for the next run.
    _attached: Option<openbot_store::Attached>,
}

impl Running {
    /// Tear the computer down. Idempotent; safe to call with no browser ever
    /// having been launched.
    pub async fn stop(&self) {
        // Stop taking snapshots before tearing the computer down, so the last
        // one is not of a workspace mid-shutdown.
        if let Some(t) = &self.snapshots {
            t.abort();
        }
        // Aborting only stops the timer; a routine already running is a child
        // process, and `kill_on_drop` on a future being dropped mid-await ends
        // it. A half-finished routine is recorded as a failed run and stays
        // due, which is the same outcome as a machine losing power.
        if let Some(t) = &self.routines {
            t.abort();
        }
        self.guest.shutdown().await;
    }
}

/// Poll until the guest has registered, or give up with a real explanation.
async fn wait_until_ready(hub_url: &str, server_id: &str) -> anyhow::Result<()> {
    let (hub, _p) = openbot_agent::HubClient::connect(hub_url)
        .await
        .map_err(|e| unreachable(hub_url, &e))?;
    for _ in 0..200 {
        if let Ok(list) = hub.list_servers().await {
            if list.iter().any(|s| s.server_id.as_str() == server_id) {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("the guest never registered with the hub")
}

/// The banner's routines line, for both states.
///
/// Extracted so a test can read it without starting a hub. Always rendered and
/// never omitted: a routine that has no clock and a scheduler somebody turned
/// off look identical from the outside, and that ambiguity is exactly what let
/// the missing clock ship. One line resolves it.
fn routines_line(every: Option<std::time::Duration>, s: crate::render::Style) -> String {
    match every {
        Some(every) => format!(
            "  {}every {}m{}\n",
            s.dim("routines     "),
            every.as_secs() / 60,
            s.dim(" \u{b7} openbot routine ls")
        ),
        None => format!(
            "  {}{}\n",
            s.dim("routines     "),
            s.yellow("not checked here \u{2014} point cron at `openbot routine tick`")
        ),
    }
}

/// What to print once it is up. Kept here so the wording is testable.
pub fn banner(r: &Running, s: crate::render::Style) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n  {}  {}  {} tools  {}  {}\n",
        s.bold("openbot is up"),
        s.dim("·"),
        // Counted from the catalogue, never written down: a number in prose is
        // a number that goes stale the next time a tool is added.
        r.tools,
        s.dim("·"),
        r.hub_url
    ));
    out.push_str(&format!(
        "  {} {}{}\n  {} {}\n",
        s.dim("computer     "),
        r.workspace.display(),
        // Whether the work is snapshottable is not something to leave someone
        // to infer from a path. It decides whether `openbot computer restore`
        // can save them later.
        if r.durable {
            String::new()
        } else {
            format!(
                "  {}",
                s.yellow("not a durable volume — nothing snapshots this")
            )
        },
        s.dim("control plane"),
        r.home.display()
    ));
    if !r.connector_tools.is_empty() {
        out.push_str(&format!(
            "  {} {}\n",
            s.dim("connectors   "),
            r.connector_tools.join(", ")
        ));
    }
    if let Some(every) = r.snapshot_every {
        // Shown because it is the difference between a rollback that works
        // and one that finds nothing to roll back to.
        out.push_str(&format!(
            "  {} every {} min{}\n",
            s.dim("snapshots    "),
            every.as_secs() / 60,
            s.dim(" · openbot computer snapshots")
        ));
    }
    // Always a line, both ways. Routines used to have no clock at all, and the
    // symptom was silence: a person who set a 9am digest saw exactly what a
    // person whose scheduler was broken saw, which is nothing. Saying which of
    // the two is true costs one line and is the difference between a feature
    // that is off and a feature that is missing.
    out.push_str(&routines_line(r.routines_every, s));
    if let Some(note) = &r.legacy {
        out.push_str(&format!("\n  {} {note}\n", s.yellow("note:")));
    }
    out.push_str("\n  Next, in another terminal:\n");
    out.push_str(&format!(
        "    {}   {}\n",
        s.bold("openbot run --demo --approve auto \"prove it\""),
        s.dim("no API key needed")
    ));
    out.push_str(&format!(
        "    {}                                {}\n\n",
        s.bold("openbot watch"),
        s.dim("see the computer")
    ));
    out.push_str(&format!(
        "  {}\n",
        s.dim(
            "Ctrl-C to stop. The guest runs here, on this machine, as you — \
             see the isolation note in the README."
        )
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routine_timer_runs_tick_against_this_home_and_this_hub() {
        // The bug was not that these arguments were wrong. It was that nothing
        // was ever run, so there were no arguments: `up.rs` did not contain the
        // word "routine". The two that matter are `--home` and `--hub` — a tick
        // aimed at the default home while the hub runs on another one reports
        // "nothing due" forever and looks exactly like a working scheduler.
        use clap::Parser as _;

        let args = tick_args(Path::new("/tmp/h"), "ws://127.0.0.1:9/v1/tools");

        // Parsed by the real parser, not matched as strings. The first version
        // of this asserted that the joined list contained "--home /tmp/h", which
        // it did, in a position clap rejects: `--home` is not a global argument,
        // so the root-position form fails with "unexpected argument". The test
        // passed and the timer would have errored once a minute forever.
        let parsed = crate::Cli::try_parse_from(std::iter::once("openbot".to_owned()).chain(args))
            .expect("the timer must build a command line this binary accepts");

        // `cmd` is an `Option` now that bare `openbot` is a screen rather than a
        // usage error, so a command line the timer builds must still parse into
        // `Some(...)` — a version that parsed to `None` would be one that ran
        // the welcome screen once a minute.
        // Rendered before the move, because `Command` is not `Clone` and the
        // failure message is worth more than the borrow.
        let shown = format!("{:?}", parsed.cmd);
        let Some(cmd) = parsed.cmd else {
            panic!("the timer built a command line with no subcommand at all: {shown}");
        };
        let crate::Command::Routine {
            home,
            cmd: crate::RoutineCmd::Tick { approve, .. },
        } = cmd
        else {
            panic!("expected `routine tick`, got {shown}");
        };
        assert_eq!(
            home,
            Path::new("/tmp/h"),
            "must tick the home this hub is serving, not the default"
        );
        assert_eq!(
            parsed.hub, "ws://127.0.0.1:9/v1/tools",
            "must reach the hub this process is running, not the default"
        );
        assert!(
            matches!(approve, crate::ApproveMode::Deny),
            "an unattended run cannot answer a prompt, so it must not ask"
        );
    }

    #[test]
    fn the_banner_says_which_of_the_two_silences_you_are_looking_at() {
        // A routine that never fires and a scheduler deliberately turned off
        // produce the same observable: nothing happens. That ambiguity is what
        // let the missing clock go unnoticed, so the banner has to resolve it
        // in both directions or it only half exists.
        let s = crate::render::Style::plain();
        let on = super::routines_line(Some(std::time::Duration::from_secs(60)), s);
        let off = super::routines_line(None, s);
        assert!(on.contains("every 1m"), "{on}");
        assert!(
            off.contains("routine tick"),
            "if this hub is not the scheduler, say what is: {off}"
        );
        assert_ne!(on, off, "the two states must not read identically");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_timer_snapshots_and_trims_only_its_own() {
        // Fast because the interval is a parameter: the CLI takes minutes, the
        // timer takes a Duration, and a test has no business waiting one.
        let d = tempfile::tempdir().unwrap();
        let store = openbot_store::Store::open(d.path()).unwrap();
        let v = store.volume("openbot-workspace").unwrap();
        std::fs::write(v.workspace().join("work.md"), "the thing that matters").unwrap();

        // A snapshot taken by hand, before letting an agent loose.
        let by_hand = v.snapshot("before the risky migration").unwrap();

        let task = snapshot_on_a_timer(
            store.volume("openbot-workspace").unwrap(),
            std::time::Duration::from_millis(60),
            2,
        );

        // Long enough for several ticks, so trimming has to have happened.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        task.abort();

        let left = v.snapshots().unwrap();
        let scheduled = left.iter().filter(|s| s.label == SCHEDULED).count();
        assert!(scheduled > 0, "the timer never took one: {left:?}");
        assert!(
            scheduled <= 2,
            "the timer kept {scheduled} of its own, past the {} it was given",
            2
        );
        assert!(
            left.iter().any(|s| s.id == by_hand.id),
            "the timer trimmed away a snapshot a person took: {left:?}"
        );
    }

    #[test]
    fn the_default_workspace_is_inside_the_volume_not_beside_it() {
        // The default resolves to the volume's live tree; a sibling directory
        // with nothing connecting it to the store would leave the snapshot
        // layer operating on files nobody writes.
        let d = tempfile::tempdir().unwrap();
        let p = Paths {
            home: d.path().to_path_buf(),
            workspace: None,
        };
        assert!(p.check().is_ok());
        let w = p.resolve("openbot-workspace").unwrap();
        assert!(matches!(w, Workspace::Durable(_)));
        assert!(
            w.dir().starts_with(d.path()),
            "the computer is not on the volume: {}",
            w.dir().display()
        );
        // The profile holds cookies and Login Data, so it must not sit where
        // `fs.read` can reach it.
        assert!(
            !w.browser_profile().starts_with(w.dir()),
            "the browser profile is inside the workspace"
        );
    }

    #[test]
    fn an_explicit_collision_is_refused_before_anything_starts() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths {
            home: d.path().to_path_buf(),
            workspace: Some(d.path().to_path_buf()),
        };
        let e = p.check().unwrap_err().to_string();
        assert!(e.contains("secrets.json"), "unhelpful refusal: {e}");
    }

    #[test]
    fn a_collision_written_two_different_ways_is_still_a_collision() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("data")).unwrap();
        let p = Paths {
            home: d.path().join("data"),
            workspace: Some(d.path().join("./data")),
        };
        assert!(p.check().is_err(), "`./data` slipped past as different");
    }

    /// The browser profile must not be inside the workspace.
    ///
    /// It holds cookies and `Login Data` (the credentials a person signed in
    /// with), and `fs.read` is allow-listed with no approval prompt, so a
    /// profile within reach of it is a credential store the model can read on
    /// request.
    ///
    /// `--workspace .` is the case to watch: `Path::new(".").parent()` is
    /// `Some("")`, so a naive computation makes the profile the relative
    /// `.openbot-browser`, which the browser resolves against the process's own
    /// directory, the workspace. Both sides are made absolute here because
    /// comparing a canonical root against a relative path answers "not
    /// inside" to a path that is.
    #[test]
    fn the_browser_profile_is_never_inside_the_workspace() {
        let d = tempfile::tempdir().unwrap();
        let inner = d.path().join("work");
        std::fs::create_dir_all(&inner).unwrap();

        for w in [inner.clone(), d.path().to_path_buf(), PathBuf::from(".")] {
            let ws = Workspace::Plain(w.clone());
            let root = std::path::absolute(&w).expect("an absolute root");
            let profile = std::path::absolute(ws.browser_profile()).expect("an absolute profile");
            assert!(
                !profile.starts_with(&root),
                "a workspace of {w:?} puts the browser profile at {profile:?}, inside {root:?}: `fs.read` hands over its cookies"
            );
        }
    }

    /// The durable volume keeps it beside `current/` for the same reason, and
    /// that is the path every real run uses.
    #[test]
    fn the_durable_profile_sits_outside_the_live_tree() {
        let d = tempfile::tempdir().unwrap();
        let store = openbot_store::Store::open(d.path()).unwrap();
        let volume = store.volume("openbot-workspace").unwrap();
        let ws = Workspace::Durable(volume);
        assert!(
            !ws.browser_profile().starts_with(ws.dir()),
            "{:?} is inside {:?}",
            ws.browser_profile(),
            ws.dir()
        );
    }

    /// The guest keeps its own copy of the default home's directory name, to
    /// catch the overlap it can see without being told where `--home` points.
    /// The dependency runs one way so the crates cannot share the constant;
    /// this test keeps the two copies from drifting.
    #[test]
    fn the_guests_copy_of_the_default_home_name_still_matches_ours() {
        let ours = crate::DEFAULT_HOME.as_str();
        let name = Path::new(ours)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        assert!(
            openbot_guest::DEFAULT_HOME_DIRS.contains(&name.as_str()),
            "`{ours}` ends with `{name}`, which the guest does not know, so its guard looks in directories nothing uses: {:?}",
            openbot_guest::DEFAULT_HOME_DIRS
        );
    }

    /// A workspace that contains the home is the dangerous nesting.
    ///
    /// `--home` defaults to `./openbot-data`, so `openbot up --workspace .` reads
    /// as "work in this directory" and puts the token store inside the
    /// workspace. The guest cannot catch it (its guard checks its own root for
    /// `secrets.json`, which is one level up from where it lands), and
    /// `fs.read` is allow-listed with no approval prompt. Only this layer
    /// knows both paths.
    #[test]
    fn a_workspace_that_contains_the_home_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths {
            home: d.path().join("openbot-data"),
            workspace: Some(d.path().to_path_buf()),
        };
        let why = p
            .check()
            .expect_err("the token store would be inside the workspace");
        let said = why.to_string();
        assert!(said.contains("contains"), "{said}");
        assert!(
            said.contains("secrets.json"),
            "the refusal should say what is at stake: {said}"
        );
    }

    #[test]
    fn nested_is_allowed_but_identical_is_not() {
        let d = tempfile::tempdir().unwrap();
        // A workspace inside the home is a different question: the guest's
        // own guard covers it, and nesting is a reasonable layout.
        let p = Paths {
            home: d.path().to_path_buf(),
            workspace: Some(d.path().join("workspace")),
        };
        assert!(p.check().is_ok());
    }
}
