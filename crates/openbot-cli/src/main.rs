//! `openbot`, the command-line client.
//!
//! Connects to a hub, binds the guest's workspace, and runs a task through the
//! agent loop, rendering the session as it happens.

#![forbid(unsafe_code)]

mod acp;
mod approve;
mod config;
mod discover;
mod html;
mod render;
mod status;
mod up;
mod watch;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use openbot_agent::agent::{AgentConfig, AgentEvent};
use openbot_agent::providers::Scripted;
use openbot_agent::{Agent, HubClient, Model};
use tokio::sync::mpsc;

use crate::approve::ApproveMode;
use crate::render::Renderer;

/// Where Bots, secrets and connectors live.
/// Where Bots live unless told otherwise.
///
/// Resolved once, from [`openbot_proto::default_home`], so this binary and the
/// desktop window cannot disagree about where a person's Bots are. They did:
/// this was `./openbot-data`, relative to whatever directory the shell was in,
/// while the window used `~/.openbot`.
pub static DEFAULT_HOME: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| openbot_proto::default_home().display().to_string());
/// The computer's files. Never the same directory as the home; see `up::Paths`.
pub const DEFAULT_WORKSPACE: &str = "./workspace";

#[derive(Parser, Debug)]
#[command(
    name = "openbot",
    version,
    about = "Give a task to a Bot with its own computer"
)]
struct Cli {
    /// Hub WebSocket endpoint.
    #[arg(
        long,
        env = "OPENBOT_HUB_URL",
        default_value = "ws://127.0.0.1:8443/v1/tools",
        global = true
    )]
    hub: String,

    /// Tool server to bind for this invocation.
    #[arg(long, default_value = "openbot-workspace", global = true)]
    server: String,

    /// Model settings for this invocation, overriding config.toml.
    #[command(flatten)]
    model_opts: config::ModelOverrides,

    /// Optional, so that bare `openbot` can be the way in rather than a wall
    /// of forty subcommands. See `welcome`.
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the hub and a computer, in one command.
    Up {
        /// Address for the hub.
        #[arg(long, env = "OPENBOT_BIND", default_value = "127.0.0.1:8443")]
        bind: String,

        /// Where Bots, secrets and connectors live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        /// The computer's files. Defaults to the durable volume inside --home,
        /// which is what `openbot computer snapshot` and `restore` operate on.
        ///
        /// Naming a directory here opts out of that: the guest works there and
        /// nothing snapshots it.
        #[arg(long, env = "OPENBOT_WORKSPACE")]
        workspace: Option<std::path::PathBuf>,

        /// Minutes between automatic snapshots of the computer. 0 turns it off.
        ///
        /// On by default: the agent has a shell, and a rollback is only useful
        /// if a snapshot exists to roll back to. Content addressing keeps it
        /// cheap; an unchanged workspace stores no file bodies, only a small
        /// manifest.
        #[arg(long, default_value_t = 30)]
        snapshot_every: u64,

        /// How many automatic snapshots to keep. Never touches the ones you
        /// take yourself.
        #[arg(long, default_value_t = 48)]
        snapshot_keep: usize,

        /// How often to run routines that are due, in minutes. 0 turns it off.
        ///
        /// Turn it off if you point cron or systemd at `openbot routine tick`
        /// yourself; two schedulers on one home means a routine can fire twice.
        /// One minute by default because a routine's schedule is only ever as
        /// precise as the check interval, and a person who asks for 09:00 means
        /// 09:00 rather than some time in the next half hour.
        #[arg(long, default_value_t = 1)]
        routines_every: u64,
    },

    /// Give the Bot a task.
    Run {
        /// What you want done.
        task: Vec<String>,

        /// Hard cap on model turns.
        #[arg(long, default_value_t = 24)]
        max_steps: u32,

        /// Show full tool results, not just failures.
        #[arg(short, long)]
        verbose: bool,

        /// Also write a self-contained HTML transcript here.
        ///
        /// Opens from a file:// URL with no network and no build step. Useful
        /// for sharing what a Bot actually did, and for reading a long run
        /// that scrolled past.
        #[arg(long, value_name = "PATH")]
        html: Option<std::path::PathBuf>,

        /// How to answer when the hub asks for approval.
        ///
        /// `ask` needs a terminal; without one it becomes `deny`, since an
        /// unattended request cannot be approved.
        #[arg(long, value_enum, default_value_t = ApproveMode::Ask)]
        approve: ApproveMode,

        /// Include browser steps in --demo, against this URL.
        ///
        /// Off by default so the demo stays offline-safe; pass a URL to check
        /// the browser path on a deployment.
        #[arg(long, value_name = "URL")]
        demo_url: Option<String>,

        /// Run as this Bot: use its standing brief, and continue its
        /// conversation rather than starting a new one.
        #[arg(long)]
        bot: Option<String>,

        /// Where bot profiles and conversations live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        /// Most recent messages to carry into the run.
        #[arg(long, default_value_t = 40)]
        history: usize,

        /// Run without a model: replay a fixed demo script against the real
        /// hub and guest. Useful for checking a deployment end to end without
        /// spending tokens or needing a key.
        #[arg(long)]
        demo: bool,

        /// Run without a model and ask, once, for a credential.
        ///
        /// The credential card is the only prompt in this binary that cannot
        /// otherwise be reached without a model deciding it wants a token.
        /// `--demo` covers the tool path; this covers the other question a Bot
        /// can put to a person.
        #[arg(long)]
        demo_secret: bool,
    },

    /// Serve this openbot as an Agent Client Protocol agent, over stdio.
    ///
    /// For editors and other ACP clients: they spawn `openbot acp`, speak
    /// JSON-RPC on stdin and stdout, and drive a Bot the same way `openbot run`
    /// does, with the same hub, policy and approval gate. Nothing is printed to
    /// stdout except protocol, because stdout is the protocol.
    ///
    /// A session is bound to a Bot named after the client's working directory,
    /// so a project keeps the same Bot, and everything it has learned, across
    /// sessions. `--bot` overrides that.
    Acp {
        /// Use this Bot for every session instead of naming one after the
        /// client's working directory.
        #[arg(long)]
        bot: Option<String>,

        /// Where bot profiles and conversations live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        /// Serve without a model: every prompt gets one scripted reply.
        ///
        /// Enough to check a client's wiring end to end (handshake, session,
        /// a streamed message, a stop reason) without a key or any tokens. It
        /// calls no tools, so it does not exercise the client's tool rendering.
        #[arg(long)]
        demo: bool,

        /// Serve without a model, but play the tool script: write a note, read
        /// it back, list the workspace, run a shell command.
        ///
        /// The script's tools go through the hub's policy like a real model's
        /// would, so the requests that need approval reach the client exactly
        /// as they will in production: the same `session/request_permission`
        /// message, the same options. Still no key and no tokens.
        #[arg(long)]
        demo_tools: bool,

        /// Serve without a model and ask, once, for a credential.
        ///
        /// The one path a client cannot otherwise exercise without a real
        /// model deciding to want a token. `--demo-tools` covers approvals;
        /// this covers the other question a Bot can put to a person, which
        /// reaches the client as a `session/request_permission` marked in
        /// `_meta` exactly as it will in production.
        #[arg(long)]
        demo_secret: bool,
    },

    /// Copy a local file into the running computer's workspace.
    ///
    /// This is how a person attaches a file to a conversation. The guest is
    /// jailed to its workspace (`Workspace::resolve` refuses anything that
    /// escapes the root), so a file elsewhere on the host is not readable by a
    /// Bot however a prompt describes it. It has to be copied in.
    ///
    /// The destination is asked of the running guest, not derived from the
    /// store's layout: `openbot up` may be serving a durable volume or any
    /// directory given to `--workspace`, and a copy aimed at the wrong one
    /// would report success while leaving the Bot unable to open the file.
    ///
    /// This is not a tool, and the prompt carries the path rather than the
    /// bytes. The Bot reads the file with `fs.read` if it needs it, under
    /// whatever policy the operator set. Contents in the prompt would instead
    /// land in the transcript, replay into every following turn through the
    /// history window, and reach the model without passing the gate that
    /// decides whether this Bot may read files at all.
    Attach {
        /// The file to copy in.
        file: std::path::PathBuf,
        /// Machine-readable: the workspace-relative path it landed at.
        #[arg(long)]
        json: bool,
    },

    /// List the tools the bound server is serving.
    Tools,

    /// Call one tool directly, through the hub.
    ///
    /// The same path a model takes (policy, approval gate, credential broker)
    /// with the arguments supplied by hand. For checking an integration works
    /// before handing it to a Bot, and for seeing exactly what a tool returns.
    Call {
        /// Tool id, e.g. `fs.read` or `linear__create_issue`.
        tool: String,

        /// Arguments, as one JSON object.
        #[arg(default_value = "{}")]
        args: String,

        /// What to do when the hub asks for approval.
        #[arg(long, value_enum, default_value = "ask")]
        approve: ApproveMode,

        /// Skip binding a tool server. Hub-served tools (`bot.*` and
        /// connectors) need no guest, so this works with nothing else running.
        #[arg(long)]
        no_server: bool,
    },

    /// Credentials the hub uses on a Bot's behalf.
    ///
    /// Values are held by the control plane and attached to outbound requests
    /// at the moment of the call. The guest never receives one, so a Bot that
    /// reads a malicious web page cannot exfiltrate a token it was never given.
    /// What may run without asking, what must be approved, and what is refused.
    ///
    /// deny beats require-approval beats allow, so a permissive rule can
    /// never widen a restrictive one: adding an allow can only ever reduce
    /// prompts, never reduce safety.
    Permission {
        /// Where the config lives. Must match the hub's `--home`.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: PermissionCmd,
    },

    Secret {
        /// Where secrets live. Must match the hub's `--home`.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: SecretCmd,
    },

    /// Procedures a Bot can look up when it needs them.
    Skill {
        /// Where skills live. Must match the hub's `--home`.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: SkillCmd,
    },

    /// Remote MCP servers the hub calls on a Bot's behalf.
    Connector {
        /// Where connector definitions live. Must match the hub's `--home`.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: ConnectorCmd,
    },

    /// Watch the Bot's computer, and take control when you need to.
    ///
    /// Opens a local page showing the guest's browser live. Watching changes
    /// nothing; taking control locks the Bot out until you give it back, which
    /// is what makes a 2FA prompt or a payment safe to finish by hand.
    Watch {
        /// Port for the local viewer. 0 picks a free one.
        #[arg(long, default_value_t = 7777)]
        port: u16,
    },

    /// Find a phrase in what your Bots and groups have said.
    ///
    /// Reads the conversations on disk. There is no index: a home holds tens
    /// of conversations, not millions, and an index is a second copy of the
    /// data that can go stale. If that assumption stops holding, this is the
    /// place that changes.
    Search {
        /// What to look for. Matched without regard to case.
        query: Vec<String>,
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str())]
        home: std::path::PathBuf,
        /// Include hidden Bots.
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Is anything wrong? One screen: hub, computer, model, routines.
    Status {
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,
    },

    /// List tool servers registered with the hub.
    Servers,

    /// Create and manage Bots.
    Bot {
        /// Where bot profiles and conversations live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: BotCmd,
    },

    /// Put several Bots on one thread.
    Group {
        /// Where bot profiles, groups and conversations live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: GroupCmd,
    },

    /// Deliver an event, running any routine that matches it.
    Event {
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: EventCmd,
    },

    /// Read or change the settings in config.toml.
    Config {
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: ConfigCmd,
    },

    /// Recurring work a Bot does on a schedule.
    Routine {
        /// Where bot profiles, routines and conversations live.
        #[arg(long, env = "OPENBOT_HOME", default_value = DEFAULT_HOME.as_str(), global = true)]
        home: std::path::PathBuf,

        #[command(subcommand)]
        cmd: RoutineCmd,
    },

    /// Manage a computer's durable state.
    ///
    /// Operates on the store directly on disk, like `docker volume`. It is an
    /// operator tool for the host the store lives on: it does not go through
    /// the hub and is therefore not subject to approval policy. A multi-user
    /// deployment must not expose it to members; see SPEC 6.0.
    Computer {
        /// Store root.
        #[arg(long, env = "OPENBOT_STORE", default_value = DEFAULT_HOME.as_str(), global = true)]
        store: std::path::PathBuf,

        /// Volume to operate on.
        #[arg(
            long,
            env = "OPENBOT_VOLUME",
            default_value = "openbot-workspace",
            global = true
        )]
        volume: String,

        #[command(subcommand)]
        cmd: ComputerCmd,
    },
}

#[derive(Subcommand, Debug)]
enum BotCmd {
    /// Create a Bot.
    New {
        name: String,
        /// What it owns, in a few words.
        #[arg(long, default_value = "")]
        title: String,
        /// Standing rules and context, replayed into every task.
        #[arg(long, default_value = "")]
        description: String,
    },
    /// List Bots.
    Ls {
        /// Include hidden Bots.
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON instead of a table.
        ///
        /// For scripts and for OPENBOT's own clients: a sidebar built by
        /// parsing a column layout breaks the first time a name is long
        /// enough to need a wider column.
        #[arg(long)]
        json: bool,
    },
    /// Show a Bot's brief and conversation size.
    Show {
        name: String,
    },
    /// Copy a Bot's brief under a new name. The conversation is not copied.
    /// Change a Bot's name, title or description.
    ///
    /// Only the flags passed change; a flag left off is left alone rather
    /// than cleared, so editing a title cannot wipe a description. Pass an
    /// empty string to clear a field.
    ///
    /// Renaming keeps the Bot. Its id, its conversation, its place in any
    /// group and its routines all stay where they are: the id is the
    /// identity, the name is the label.
    Set {
        /// The Bot, by name or id.
        name: String,

        /// A new display name.
        #[arg(long)]
        rename: Option<String>,

        /// What it does, in a few words. Shown under its name.
        #[arg(long)]
        title: Option<String>,

        /// The longer description a person reads.
        #[arg(long)]
        description: Option<String>,
    },

    Dup {
        name: String,
        new_name: String,
    },
    /// Take a Bot out of the list without deleting its work.
    Hide {
        name: String,
    },
    /// Put a hidden Bot back.
    Unhide {
        name: String,
    },
    /// Forget a Bot's conversation, keeping the Bot.
    Forget {
        name: String,
    },
    /// Delete a Bot and its conversation.
    Rm {
        name: String,
    },
    /// Hand work to a Bot. It picks it up on its next run.
    Send {
        /// Recipient.
        to: String,
        /// What you are handing over.
        message: Vec<String>,
        /// Send as another Bot rather than as yourself.
        #[arg(long)]
        from: Option<String>,
    },
    /// Show what is waiting for a Bot.
    Inbox {
        name: String,
    },

    /// Read back what a Bot has been doing.
    ///
    /// The conversation survives the process; this is how to look at it.
    /// Useful after a routine has been running unattended: it shows the tasks
    /// the Bot was given and what it said back.
    Log {
        name: String,

        /// How many messages to show, most recent last.
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Include tool results, not just the calls.
        ///
        /// Off by default: results are the bulk of a transcript and rarely
        /// what someone scrolling back is looking for.
        #[arg(long)]
        full: bool,
    },
}

#[derive(Subcommand, Debug)]
enum GroupCmd {
    /// Create a group. The first member is the coordinator: it answers
    /// anything nobody was mentioned in.
    New {
        name: String,
        /// Comma-separated Bot names, coordinator first.
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
    },
    /// List groups.
    Ls {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show a group's members and thread size.
    Show { name: String },
    /// Post to a group. One Bot answers: whoever is @mentioned, or the
    /// coordinator.
    Post {
        name: String,
        message: Vec<String>,
        /// Hard cap on model turns for the answering Bot.
        #[arg(long, default_value_t = 24)]
        max_steps: u32,
        /// Show the whole run rather than just the reply.
        #[arg(short, long)]
        verbose: bool,
        /// Answer without a model, for checking a deployment.
        #[arg(long)]
        demo: bool,
        /// How to answer approval requests.
        #[arg(long, value_enum, default_value_t = ApproveMode::Ask)]
        approve: ApproveMode,
        /// Most recent thread messages to carry into the turn.
        #[arg(long, default_value_t = 40)]
        history: usize,
    },
    /// Read the thread back, including the handoffs between members.
    ///
    /// The reason to put Bots in a group is that the handoff is visible in one
    /// conversation; this is where it is shown.
    Log {
        name: String,
        /// How many messages, most recent last.
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// Include tool results, which are the bulk of a transcript.
        #[arg(long)]
        full: bool,
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Delete a group. Its members are left alone.
    Rm { name: String },
}

#[derive(Subcommand, Debug)]
enum EventCmd {
    /// Deliver an event. Point a webhook at this, or use it by hand.
    Post {
        /// Where it came from: `github`, `slack`, anything you name.
        source: String,

        /// The payload, as JSON. Use `-` to read stdin.
        #[arg(long, default_value = "{}")]
        payload: String,

        /// The sender's delivery id. Supply it and a retry is ignored:
        /// every webhook provider retries, and doing consequential work
        /// twice is an incident.
        #[arg(long)]
        id: Option<String>,

        /// Show which routines would run, without running them.
        #[arg(long)]
        dry_run: bool,

        #[arg(long, value_enum, default_value_t = ApproveMode::Ask)]
        approve: ApproveMode,

        #[arg(long)]
        demo: bool,
    },
}

#[derive(Subcommand, Debug)]
enum SecretCmd {
    /// Store a credential, read from standard input.
    ///
    /// There is no `--value` flag. Command-line arguments are world readable
    /// in `/proc/<pid>/cmdline` on Linux for as long as the process lives, and
    /// land in shell history besides. Piping is the safe shape:
    ///
    ///   `cat token.txt | openbot secret set linear-token`
    Set {
        /// What the connector will refer to, e.g. `linear-token`.
        name: String,
    },

    /// List the names, with a fingerprint so you can tell two apart.
    ///
    /// There is no command that prints a value: the only reason to want one is
    /// to put it somewhere less safe than here.
    Ls {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Forget a credential.
    Rm { name: String },
}

#[derive(Subcommand, Debug)]
enum PermissionCmd {
    /// List the rules, in the order they are written.
    Ls {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Add a rule.
    Add {
        /// `allow`, `ask`, or `deny`.
        #[arg(long)]
        action: String,
        /// Glob over the tool id: `fs.read`, `fs.*`, `*`.
        #[arg(long)]
        tool: String,
        /// Narrow to one argument, e.g. `--when-key path --when-glob "/etc/*"`.
        #[arg(long, requires = "when_glob")]
        when_key: Option<String>,
        #[arg(long, requires = "when_key")]
        when_glob: Option<String>,
        /// What a person is shown when this rule stops or refuses a call.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Remove a rule by its number in `ls`.
    Rm { number: usize },
}

#[derive(Subcommand, Debug)]
enum SkillCmd {
    /// List what a Bot can look up, and anything that failed to load.
    Ls {
        /// Machine-readable, for a client offering skills by name.
        #[arg(long)]
        json: bool,
    },

    /// Start a new skill from a template.
    New {
        /// Short name, used as the folder and how a Bot refers to it.
        name: String,

        /// What it is for. This is the only part a Bot sees before deciding to
        /// read it, so write it for that reader.
        #[arg(long)]
        description: String,
    },

    /// Print a skill in full, exactly as a Bot would read it.
    Show { name: String },

    /// Delete a skill.
    Rm { name: String },
}

#[derive(Subcommand, Debug)]
enum ConnectorCmd {
    /// Add a remote MCP server and verify it answers.
    Add {
        /// Short name. Becomes the tool prefix: `linear` gives
        /// `linear__create_issue`.
        id: String,

        /// The MCP endpoint.
        url: String,

        /// Header value, referencing a stored secret by name.
        ///
        /// e.g. `--authorization "Bearer ${linear-token}"`. A literal token is
        /// refused: this file is meant to be readable and shareable.
        #[arg(long, default_value = "")]
        authorization: String,

        /// Save without checking the connector answers.
        ///
        /// The check catches a bad credential at the moment it is entered
        /// rather than at the first tool call.
        #[arg(long)]
        no_verify: bool,
    },

    /// List configured connectors and the secrets they use.
    Ls {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Ask a connector what tools it offers, using its stored credential.
    Test { id: String },

    /// Remove a connector. The secret it referenced is left alone.
    Rm { id: String },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved settings and where they came from.
    Show,
    /// Set the model used when no flag overrides it.
    Set {
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        dialect: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key_env: Option<String>,
        #[arg(long)]
        max_tokens: Option<u32>,
    },
}

#[derive(Subcommand, Debug)]
enum RoutineCmd {
    /// Create a routine that fires when something happens.
    On {
        /// Which Bot owns it.
        bot: String,
        name: String,
        /// Event source to listen to: `github`, `slack`, …
        #[arg(long)]
        source: String,
        /// A condition, as `path=value` (exact) or `path~value` (contains).
        /// Repeat for several; all must hold.
        ///
        /// At least one is required. A source on its own fires on every event
        /// from that source, which floods the thread and burns usage.
        #[arg(long = "when", required = true)]
        when: Vec<String>,
        #[arg(long)]
        instructions: String,
    },
    /// Create a routine.
    New {
        /// Which Bot owns it. A routine always has exactly one owner.
        bot: String,
        name: String,
        /// Five-field cron: minute hour day-of-month month weekday.
        #[arg(long)]
        cron: String,
        /// What to do, every run. It is replayed verbatim, so it has to
        /// carry its own context.
        #[arg(long)]
        instructions: String,
        /// IANA timezone. Empty means UTC.
        #[arg(long, default_value = "")]
        timezone: String,
    },
    /// List routines and when they next fire.
    Ls {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show a routine and its recent runs.
    Show { bot: String, id: String },
    /// Stop a routine firing, keeping its history.
    Pause { bot: String, id: String },
    /// Start it again.
    Resume { bot: String, id: String },
    /// Delete a routine.
    Rm { bot: String, id: String },
    /// Run one routine now, whatever its schedule says.
    ///
    /// A rehearsal: it does the work and records what happened, and it leaves
    /// the schedule exactly where it was. Testing a routine at 08:55 must not
    /// cancel the nine o'clock firing it was testing.
    ///
    /// Works on a paused routine too, which is most of the point — a routine
    /// you are still getting right is one you have not armed yet.
    Run {
        bot: String,
        id: String,
        /// Answer approvals automatically.
        ///
        /// `ask` needs a terminal; without one it becomes `deny`, since an
        /// unattended request cannot be approved.
        #[arg(long, value_enum, default_value_t = ApproveMode::Ask)]
        approve: ApproveMode,
        /// Use the scripted demo instead of a model.
        #[arg(long)]
        demo: bool,
    },
    /// Run everything that is due, once, and record the outcome.
    ///
    /// Composable by design: point cron, systemd or a container scheduler at
    /// this rather than baking a daemon into the CLI.
    Tick {
        /// Show what would run without running it.
        #[arg(long)]
        dry_run: bool,
        /// Answer approval requests this way. Unattended runs cannot prompt,
        /// so `ask` resolves to deny.
        #[arg(long, value_enum, default_value_t = ApproveMode::Ask)]
        approve: ApproveMode,
        /// Run without a model, for checking a deployment.
        #[arg(long)]
        demo: bool,

        /// Pause routines when nobody has looked at this account for this many
        /// days.
        ///
        /// An agent that keeps working, and spending, while nobody is watching
        /// is a bug (SPEC §8). "Looked" means an openbot command run at a
        /// terminal; a cron tick does not count. 0 turns it off.
        #[arg(long, default_value_t = 14)]
        idle_days: i64,
    },
}

#[derive(Subcommand, Debug)]
enum ComputerCmd {
    /// Show what the computer is holding.
    Status {
        /// Machine-readable, for a client that needs the workspace path.
        ///
        /// Every other listing here has one (`bot ls`, `secret ls`,
        /// `connector ls`, `routine ls`, `policy ls`), and the desktop client
        /// has to know where the workspace is to put an attachment in it. The
        /// alternative is the client reproducing `<home>/volumes/<id>/current`,
        /// a layout only `openbot-store` should know and one that would go
        /// quietly wrong if it changed.
        #[arg(long)]
        json: bool,
    },
    /// List snapshots, oldest first.
    Snapshots,
    /// Take a snapshot now.
    Snapshot {
        #[arg(default_value = "manual")]
        label: String,
    },
    /// Roll the workspace back to a snapshot.
    ///
    /// Always takes a safety snapshot first, so this is reversible.
    Restore { id: String },
    /// Delete all but the newest N snapshots and reclaim their storage.
    ///
    /// The one irreversible operation here: pruned snapshots are gone.
    Prune {
        /// How many of the newest snapshots to keep. Required, with no
        /// default: this is the only command in openbot that destroys history,
        /// and a default would let `openbot computer prune`, typed to see what
        /// it does, silently delete older snapshots. SPEC §5 asks for an
        /// explicit keep-count.
        keep: usize,
    },
    /// Clear an attach lock left by a guest that died.
    ///
    /// Only when no guest is running. Clearing it while one is puts two
    /// writers in one workspace.
    ForceDetach,
}

/// Run on a thread with a stack big enough for an unoptimised build.
///
/// Windows gives the main thread 1 MiB, and clap's generated parser is deeply
/// recursive when nothing is inlined, so a debug build of a CLI this size sits
/// close to that limit while a release build is nowhere near it. Past the
/// limit, `openbot --help` dies with "thread 'main' has overflowed its stack"
/// and every command with it, and the whole test suite drives the debug
/// binary. Release builds are unaffected, which makes the failure look like a
/// bad change rather than a stack limit.
///
/// A spawned thread names its own stack size and does so on every platform,
/// which a linker argument would not.
fn main() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("openbot".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(run)?
        .join()
        .map_err(|_| anyhow::anyhow!("openbot panicked"))?
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            // `openbotd::skills` warns about a skill it skipped, which the daemon
            // needs and the CLI reports itself; printing both would bury the
            // line that matters.
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,openbotd::skills=error".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let hub_url = cli.hub.clone();
    let server = cli.server.clone();
    let model_opts = cli.model_opts.clone();

    let Some(cmd) = cli.cmd else {
        return welcome().await;
    };

    match cmd {
        Command::Acp {
            bot,
            home,
            demo,
            demo_tools,
            demo_secret,
        } => {
            // Nothing may be written to stdout from here on: it carries the
            // protocol. Diagnostics go to stderr, which is where a client
            // shows them.
            acp::serve::serve(acp::serve::Config {
                hub: cli.hub.clone(),
                server: cli.server.clone(),
                home,
                model_opts: cli.model_opts.clone(),
                demo,
                demo_tools,
                demo_secret,
                bot,
            })
            .await?;
        }
        Command::Bot { home, cmd } => {
            watching(&home);
            let bots = openbot_bots::BotStore::open(&home)?;
            match cmd {
                BotCmd::New {
                    name,
                    title,
                    description,
                } => {
                    let b = bots.create(&name, &title, &description)?;
                    println!("created {} ({})", b.name, b.id);
                    if b.description.is_empty() {
                        println!(
                            "  tip: --description holds standing rules, replayed into every task"
                        );
                    }
                }
                BotCmd::Ls { all, json } => {
                    let list = bots.list(all)?;
                    if json {
                        // Straight to stdout, never through `render::outln!`:
                        // that decorates for a terminal, and a caller asking
                        // for JSON is not one. One array, so a reader can
                        // parse the whole answer rather than guess when it
                        // has all of it.
                        let rows: Vec<serde_json::Value> = list
                            .iter()
                            .map(|b| {
                                serde_json::json!({
                                    "id": b.id.as_str(),
                                    "name": b.name,
                                    "title": b.title,
                                    "description": b.description,
                                    "hidden": b.hidden,
                                    "seq": b.seq,
                                    "messages": bots.message_count(&b.id).unwrap_or(0),
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string(&rows)?);
                        return Ok(());
                    }
                    if list.is_empty() {
                        println!("no bots yet — `openbot bot new \"Account Health\"`");
                    }
                    for b in list {
                        let n = bots.message_count(&b.id).unwrap_or(0);
                        render::outln!(
                            "{:<20} {:<24} {:>4} messages{}",
                            b.id.as_str(),
                            if b.title.is_empty() { "—" } else { &b.title },
                            n,
                            if b.hidden { "  (hidden)" } else { "" }
                        );
                    }
                }
                BotCmd::Show { name } => {
                    let b = bots.resolve(&name)?;
                    println!("{}  ({})", b.name, b.id);
                    if !b.title.is_empty() {
                        println!("{}", b.title);
                    }
                    println!("{} messages", bots.message_count(&b.id)?);
                    if !b.description.is_empty() {
                        println!("\nstanding brief:\n{}", b.description);
                    }
                }
                BotCmd::Log { name, limit, full } => {
                    let b = bots.resolve(&name)?;
                    let total = bots.message_count(&b.id)?;
                    let msgs = bots.history(&b.id, Some(limit))?;
                    if msgs.is_empty() {
                        println!(
                            "{} has not been given anything yet — `openbot run --bot {} \"…\"`",
                            b.id, b.id
                        );
                    } else {
                        let s = render::Style::detect();
                        println!(
                            "{}  {}",
                            s.bold(&b.name),
                            s.dim(&format!("showing {} of {total} messages", msgs.len()))
                        );
                        render::conversation(&msgs, s, full);
                        println!();
                    }
                }
                BotCmd::Dup { name, new_name } => {
                    let src = bots.resolve(&name)?;
                    let b = bots.duplicate(&src.id, &new_name)?;
                    println!(
                        "created {} from {} — brief copied, conversation not",
                        b.id, src.id
                    );
                }
                BotCmd::Set {
                    name,
                    rename,
                    title,
                    description,
                } => {
                    // Report a no-op rather than printing a line that reads as
                    // though something changed.
                    if rename.is_none() && title.is_none() && description.is_none() {
                        anyhow::bail!(
                            "nothing to change — pass --rename, --title or --description"
                        );
                    }
                    let mut b = bots.resolve(&name)?;
                    if let Some(new_name) = rename {
                        b = bots.rename(b.id.as_str(), &new_name)?;
                    }
                    if title.is_some() || description.is_some() {
                        b =
                            bots.describe(b.id.as_str(), title.as_deref(), description.as_deref())?;
                    }
                    // Print the id: a rename does not move it, and the reader
                    // needs to know the conversation came along.
                    println!("{} is now {}", b.id, b.name);
                    if !b.title.is_empty() {
                        render::outln!("  {}", b.title);
                    }
                }

                BotCmd::Hide { name } => {
                    let mut b = bots.resolve(&name)?;
                    b.hidden = true;
                    bots.save(&b)?;
                    println!("hid {} — its work is kept", b.id);

                    // SPEC §8 asks for this to be surfaced clearly. "Its work
                    // is kept" reads as "the data is safe"; it also means a
                    // hidden Bot goes on running, and spending, out of sight.
                    // Only printed when something is still scheduled.
                    let live: Vec<_> = bots
                        .routines(&b.id)?
                        .into_iter()
                        .filter(|r| r.enabled)
                        .collect();
                    if !live.is_empty() {
                        println!(
                            "\n  {} still {} — hiding a Bot does not pause its work:",
                            plural(live.len(), "routine"),
                            if live.len() == 1 { "runs" } else { "run" }
                        );
                        for r in &live {
                            render::outln!("    {}  {}", r.id, r.trigger.describe());
                        }
                        println!("\n  `openbot routine pause {} <id>` to stop one.", b.id);
                    }
                }
                BotCmd::Unhide { name } => {
                    let mut b = bots.resolve(&name)?;
                    b.hidden = false;
                    bots.save(&b)?;
                    println!("unhid {}", b.id);
                }
                BotCmd::Forget { name } => {
                    let b = bots.resolve(&name)?;
                    let n = bots.message_count(&b.id)?;
                    bots.clear_history(&b.id)?;
                    println!("forgot {n} messages; {} kept its brief", b.id);
                }
                BotCmd::Send { to, message, from } => {
                    let msg = message.join(" ");
                    if msg.trim().is_empty() {
                        anyhow::bail!(
                            "nothing to hand over: openbot bot send <to> \"the message\""
                        );
                    }
                    let dest = bots.resolve(&to)?;
                    let sender = match &from {
                        Some(f) => Some(bots.resolve(f)?.id),
                        None => None,
                    };
                    bots.send(sender.as_ref(), &dest.id, &msg)?;
                    let n = bots.inbox(&dest.id)?.len();
                    println!(
                        "handed to {} — {n} waiting; it will pick this up on its next run",
                        dest.id
                    );
                }
                BotCmd::Inbox { name } => {
                    let b = bots.resolve(&name)?;
                    let waiting = bots.inbox(&b.id)?;
                    if waiting.is_empty() {
                        println!("nothing waiting for {}", b.id);
                    }
                    for h in waiting {
                        match h.from {
                            Some(f) => render::outln!("from {f}: {}", h.text),
                            None => render::outln!("from you: {}", h.text),
                        }
                    }
                }
                BotCmd::Rm { name } => {
                    let b = bots.resolve(&name)?;
                    let gone = bots.delete(&b.id)?;
                    println!("deleted {}", b.id);
                    // Report what went with it. Deletion is irreversible, and
                    // an attached routine should not be discovered by noticing
                    // it stopped running.
                    if gone.messages > 0 {
                        render::outln!("  {} messages", gone.messages);
                    }
                    if gone.routines > 0 {
                        render::outln!("  {} routine(s) — they will not run again", gone.routines);
                    }
                    if !gone.left.is_empty() {
                        render::outln!("  removed from {}", gone.left.join(", "));
                    }
                    if !gone.emptied.is_empty() {
                        render::outln!(
                            "  {} had no other members and was deleted too",
                            gone.emptied.join(", ")
                        );
                    }
                }
            }
        }

        Command::Group { home, cmd } => {
            let bots = openbot_bots::BotStore::open(&home)?;
            match cmd {
                GroupCmd::New { name, members } => {
                    let ids = members
                        .iter()
                        .map(|m| bots.resolve(m).map(|b| b.id))
                        .collect::<Result<Vec<_>, _>>()?;
                    let g = bots.create_group(&name, &ids)?;
                    println!(
                        "created {} with {} members; {} coordinates",
                        g.id,
                        g.members.len(),
                        g.members[0]
                    );
                }
                GroupCmd::Ls { json } => {
                    let list = bots.groups(false)?;
                    if json {
                        let mut rows = Vec::new();
                        for g in &list {
                            rows.push(serde_json::json!({
                                "id": g.id.as_str(),
                                "name": g.name,
                                // Id and name together, for the same reason
                                // `routine ls` carries both: a client renders
                                // names, a group stores ids, and a rename
                                // makes the two differ for good.
                                "members": g.members.iter().map(|m| serde_json::json!({
                                    "id": m.as_str(),
                                    "name": bots.get(m)
                                        .map_or_else(|_| m.as_str().to_owned(), |b| b.name),
                                })).collect::<Vec<_>>(),
                                "messages": bots.group_message_count(&g.id).unwrap_or(0),
                            }));
                        }
                        println!("{}", serde_json::to_string(&rows)?);
                        return Ok(());
                    }
                    if list.is_empty() {
                        println!("no groups yet — `openbot group new Launch --members a,b`");
                    }
                    for g in list {
                        let n = bots.group_message_count(&g.id).unwrap_or(0);
                        let who: Vec<_> = g.members.iter().map(|m| m.as_str().to_owned()).collect();
                        render::outln!(
                            "{:<20} {:>4} messages  {}",
                            g.id.as_str(),
                            n,
                            who.join(", ")
                        );
                    }
                }
                GroupCmd::Show { name } => {
                    let g = bots.resolve_group(&name)?;
                    println!("{}  ({})", g.name, g.id);
                    println!("{} messages", bots.group_message_count(&g.id)?);
                    for (i, m) in g.members.iter().enumerate() {
                        let title = bots.get(m).map(|b| b.title).unwrap_or_default();
                        render::outln!(
                            "  {}{}{}",
                            m,
                            if title.is_empty() {
                                String::new()
                            } else {
                                format!("  {title}")
                            },
                            if i == 0 { "  (coordinates)" } else { "" }
                        );
                    }
                }
                GroupCmd::Log {
                    name,
                    limit,
                    full,
                    json,
                } => {
                    let g = bots.resolve_group(&name)?;
                    let total = bots.group_message_count(&g.id)?;
                    let msgs = bots.group_history(&g.id, Some(limit))?;
                    if json {
                        println!("{}", serde_json::to_string(&msgs)?);
                        return Ok(());
                    }
                    if msgs.is_empty() {
                        println!(
                            "{} has said nothing yet — `openbot group post {} \"…\"`",
                            g.id, g.id
                        );
                    } else {
                        let s = render::Style::detect();
                        println!(
                            "{}  {}",
                            s.bold(&g.name),
                            s.dim(&format!("showing {} of {total} messages", msgs.len()))
                        );
                        render::conversation(&msgs, s, full);
                        println!();
                    }
                }
                GroupCmd::Rm { name } => {
                    let g = bots.resolve_group(&name)?;
                    bots.delete_group(&g.id)?;
                    println!("deleted {} — its members are untouched", g.id);
                }
                GroupCmd::Post {
                    name,
                    message,
                    max_steps,
                    verbose,
                    demo,
                    approve,
                    history,
                } => {
                    let text = message.join(" ");
                    if text.trim().is_empty() {
                        anyhow::bail!(
                            "nothing to post: openbot group post <name> \"@writer draft it\""
                        );
                    }
                    let g = bots.resolve_group(&name)?;
                    // One owner per turn. Fanning out to everyone produces
                    // duplicate work and a thread nobody can read.
                    let owner = g.owner_for(&text).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "you mentioned somebody who is not in {} — members are {}",
                            g.id,
                            g.members
                                .iter()
                                .map(|m| m.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                    let bot = bots.get(&owner)?;

                    // The same path a Bot turn takes, with `Thread::Group`
                    // as the one difference: seeded from the thread rather
                    // than from this member's own history, and appended back
                    // to it. Sharing `run_task` keeps one place to fix a turn.
                    let mut r = Renderer::new(verbose);
                    println!("  {}  {}", g.id, bot.id);
                    r.task(&text);
                    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
                    let renderer = tokio::spawn(async move {
                        while let Some(e) = ev_rx.recv().await {
                            r.event(&e);
                        }
                    });
                    let outcome = run_task(Task {
                        hub_url: &cli.hub,
                        server: &cli.server,
                        home: &home,
                        model_opts: &model_opts,
                        bots: &bots,
                        bot: &bot,
                        task: &text,
                        approver: approve::handler(approve),
                        demo,
                        demo_tools: false,
                        demo_secret: false,
                        fallback: "Noted — picking this up.",
                        thread: Thread::Group(&g.id),
                        max_steps,
                        history,
                        watch: Some(ev_tx),
                        cancel: None,
                        redirects: None,
                    })
                    .await;
                    let _ = renderer.await;
                    let outcome = outcome?;

                    if !outcome.succeeded() {
                        std::process::exit(1);
                    }
                }
            }
        }

        Command::Event { home, cmd } => match cmd {
            EventCmd::Post {
                source,
                payload,
                id,
                dry_run,
                approve,
                demo,
            } => {
                let raw = if payload == "-" {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                } else {
                    payload
                };
                let parsed: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("payload is not valid JSON: {e}"))?;

                let bots = openbot_bots::BotStore::open(&home)?;
                let event = openbot_bots::Event {
                    source,
                    id: id.clone(),
                    payload: parsed,
                };
                let hits = bots.triggered_by(&event)?;
                if hits.is_empty() {
                    println!("no routine matched");
                }
                for mut r in hits {
                    if dry_run {
                        render::outln!("would run {}/{}", r.bot, r.id);
                        continue;
                    }
                    let bot = bots.get(&r.bot)?;
                    render::outln!("running {}/{}", r.bot, r.id);
                    // The payload goes to the Bot: a trigger that says only
                    // "something happened" leaves it guessing what.
                    let task = format!(
                        "{}\n\nThe event that triggered this:\n```json\n{}\n```",
                        r.instructions,
                        serde_json::to_string_pretty(&event.payload).unwrap_or_default()
                    );
                    let outcome = run_task(Task {
                        hub_url: &hub_url,
                        server: &server,
                        home: &home,
                        model_opts: &model_opts,
                        bots: &bots,
                        bot: &bot,
                        task: &task,
                        approver: approve::handler(approve),
                        demo,
                        demo_tools: false,
                        demo_secret: false,
                        fallback: "Routine ran.",
                        thread: Thread::Own,
                        max_steps: DEFAULT_MAX_STEPS,
                        history: DEFAULT_HISTORY,
                        watch: None,
                        cancel: None,
                        redirects: None,
                    })
                    .await;
                    let run = match &outcome {
                        Ok(o) => openbot_bots::Run {
                            at: chrono::Utc::now(),
                            ok: o.succeeded(),
                            summary: o.text.chars().take(200).collect(),
                            steps: o.steps,
                            tokens_in: o.usage.input_tokens,
                            tokens_out: o.usage.output_tokens,
                            // Two reasons a run should be repeated: a person
                            // had the computer, or the provider was briefly
                            // unavailable. In neither case has the day's
                            // work happened.
                            retryable: matches!(
                                o.reason,
                                openbot_agent::FinishReason::ComputerBusy { .. }
                                    | openbot_agent::FinishReason::ModelFailed {
                                        transient: true,
                                        ..
                                    }
                            ),
                            manual: false,
                        },
                        Err(e) => openbot_bots::Run {
                            at: chrono::Utc::now(),
                            ok: false,
                            summary: e.to_string().chars().take(200).collect(),
                            steps: 0,
                            tokens_in: 0,
                            tokens_out: 0,
                            // A hub that was restarting is not a reason to
                            // skip the day. Anything unrecognised stays false.
                            retryable: openbot_agent::is_transient(e),
                            manual: false,
                        },
                    };
                    render::outln!("  {} {}", if run.ok { "ok" } else { "failed" }, run.summary);
                    bots.record_run(&mut r, run)?;
                }
                // Remembered after the work, so a crash mid-run lets a retry
                // through rather than silently dropping it.
                if let Some(id) = &id {
                    if !dry_run {
                        bots.remember_event(id)?;
                    }
                }
            }
        },

        Command::Config { home, cmd } => match cmd {
            ConfigCmd::Show => {
                let c = config::load(&home)?;
                println!("{}", config::path(&home).display());
                println!(
                    "model        {}",
                    c.model.id.as_deref().unwrap_or("(not set)")
                );
                println!("dialect      {}", c.model.dialect);
                println!("base_url     {}", c.model.base_url);
                println!(
                    "api_key_env  {}  {}",
                    c.model.api_key_env,
                    if std::env::var(&c.model.api_key_env).is_ok() {
                        "(set)"
                    } else {
                        "(NOT set)"
                    }
                );
                println!("max_tokens   {}", c.model.max_tokens);
            }
            ConfigCmd::Set {
                model,
                dialect,
                base_url,
                api_key_env,
                max_tokens,
            } => {
                let mut c = config::load(&home)?;
                if let Some(v) = model {
                    c.model.id = Some(v);
                }
                if let Some(v) = dialect {
                    // Fail now rather than at the next run.
                    v.parse::<openbot_agent::providers::Dialect>()
                        .map_err(|e| anyhow::anyhow!(e))?;
                    c.model.dialect = v;
                }
                if let Some(v) = base_url {
                    c.model.base_url = v;
                }
                if let Some(v) = api_key_env {
                    c.model.api_key_env = v;
                }
                if let Some(v) = max_tokens {
                    c.model.max_tokens = v;
                }
                config::save(&home, &c)?;
                println!("saved {}", config::path(&home).display());
            }
        },

        Command::Routine { ref home, cmd } => {
            let home = home.clone();
            // Records that a person is present. Only from a terminal: a cron
            // tick reading the routine list is not a person looking at it, and
            // counting it as one would disable the idle guard permanently.
            watching(&home);
            let bots = openbot_bots::BotStore::open(&home)?;
            let now = chrono::Utc::now();
            match cmd {
                RoutineCmd::New {
                    bot,
                    name,
                    cron,
                    instructions,
                    timezone,
                } => {
                    let b = bots.resolve(&bot)?;
                    let r = bots.create_routine(&b.id, &name, &instructions, &cron, &timezone)?;
                    println!("created {}/{}", b.id, r.id);
                    // Echo the schedule in words: an unattended routine is only
                    // checked by a person at the moment it is written.
                    println!("  {}", r.trigger.describe());
                    match r.next_after(now)? {
                        Some(t) => println!("  next {}", t.to_rfc3339()),
                        None => println!("  warning: this schedule never fires"),
                    }
                }
                RoutineCmd::On {
                    bot,
                    name,
                    source,
                    when,
                    instructions,
                } => {
                    let b = bots.resolve(&bot)?;
                    let matches = when
                        .iter()
                        .map(|w| parse_condition(w))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    let r = bots.create_triggered(
                        &b.id,
                        &name,
                        &instructions,
                        openbot_bots::Trigger::Event { source, matches },
                    )?;
                    println!("created {}/{}", b.id, r.id);
                    println!("  {}", r.trigger.describe());
                }
                RoutineCmd::Ls { json } => {
                    let all = bots.all_routines()?;
                    if json {
                        let mut rows = Vec::new();
                        for r in &all {
                            rows.push(serde_json::json!({
                                "bot": r.bot.as_str(),
                                // The name too, because a client shows names
                                // and this is an id. They stop matching the
                                // moment a Bot is renamed (the id is kept),
                                // and a settings panel showing `talent-scout`
                                // beside a sidebar showing "Recruiting" is a
                                // client disagreeing with itself. Falls back
                                // to the id if the Bot is gone.
                                "bot_name": bots.get(&r.bot)
                                    .map_or_else(|_| r.bot.as_str().to_owned(), |b| b.name),
                                "id": r.id,
                                "trigger": r.trigger.describe(),
                                // Absent rather than invented for an event
                                // routine, which has no next time.
                                "next": r.next_after(now)?.map(|t| t.to_rfc3339()),
                                "enabled": r.enabled,
                            }));
                        }
                        println!("{}", serde_json::to_string(&rows)?);
                        return Ok(());
                    }
                    if all.is_empty() {
                        println!("no routines — `openbot routine new <bot> <name> --cron \"0 9 * * *\" --instructions ...`");
                    }
                    for r in all {
                        let when = match r.next_after(now)? {
                            Some(t) => format!("next {}", t.to_rfc3339()),
                            // An event routine has no next time; do not invent
                            // one.
                            None if r.cron().is_none() => "on an event".into(),
                            None => "never".into(),
                        };
                        render::outln!(
                            "{:<14} {:<18} {:<40} {}{}",
                            r.bot.as_str(),
                            r.id,
                            r.trigger.describe(),
                            when,
                            if r.enabled { "" } else { "  (paused)" }
                        );
                    }
                }
                RoutineCmd::Show { bot, id } => {
                    let b = bots.resolve(&bot)?;
                    let r = bots.get_routine(&b.id, &id)?;
                    println!("{} / {}", r.bot, r.id);
                    println!("{}", r.trigger.describe());
                    println!("{}", if r.enabled { "enabled" } else { "paused" });
                    println!("\n{}\n", r.instructions);
                    if r.runs.is_empty() {
                        println!("never run");
                    }
                    let mut total_in = 0u64;
                    let mut total_out = 0u64;
                    for run in r.runs.iter().rev().take(10) {
                        let cost = match (run.tokens_in, run.tokens_out) {
                            (0, 0) => String::new(),
                            (i, o) => format!("  {i}/{o} tok"),
                        };
                        render::outln!(
                            "{}  {}  {}{}{}",
                            run.at.to_rfc3339(),
                            if run.retryable {
                                "held"
                            } else if run.ok {
                                "ok  "
                            } else {
                                "fail"
                            },
                            run.summary,
                            cost,
                            // Marked, because this is the only place a person
                            // can check whether a routine has actually been
                            // running. Three green rows that were all somebody
                            // pressing a button say the opposite of what they
                            // appear to say.
                            if run.manual { "  (test run)" } else { "" }
                        );
                    }
                    for run in &r.runs {
                        total_in += run.tokens_in;
                        total_out += run.tokens_out;
                    }
                    if total_in + total_out > 0 {
                        // Over the kept history, not for all time. Say which:
                        // a number that quietly means something else is worse
                        // than no number.
                        println!(
                            "
{} tokens in / {} out over the last {} runs",
                            total_in,
                            total_out,
                            r.runs.len()
                        );
                    }
                }
                RoutineCmd::Pause { bot, id } => {
                    let b = bots.resolve(&bot)?;
                    let mut r = bots.get_routine(&b.id, &id)?;
                    r.enabled = false;
                    bots.save_routine(&r)?;
                    println!("paused {}/{} — its history is kept", b.id, r.id);
                }
                RoutineCmd::Resume { bot, id } => {
                    let b = bots.resolve(&bot)?;
                    let mut r = bots.get_routine(&b.id, &id)?;
                    r.enabled = true;
                    bots.save_routine(&r)?;
                    match r.next_after(now)? {
                        Some(t) => println!("resumed {}/{} — next {}", b.id, r.id, t.to_rfc3339()),
                        None => println!("resumed {}/{}", b.id, r.id),
                    }
                }
                RoutineCmd::Rm { bot, id } => {
                    let b = bots.resolve(&bot)?;
                    bots.delete_routine(&b.id, &id)?;
                    println!("deleted {}/{}", b.id, id);
                }
                RoutineCmd::Run {
                    bot,
                    id,
                    approve,
                    demo,
                } => {
                    let b = bots.resolve(&bot)?;
                    let mut r = bots.get_routine(&b.id, &id)?;
                    render::outln!("running {}/{} now", b.id, r.id);
                    if !r.enabled {
                        // Said, not refused. A routine you are still getting
                        // right is one you have not armed yet, and rehearsing
                        // it is most of what this command is for.
                        render::outln!("  this routine is paused; running it anyway");
                    }
                    let outcome = run_task(Task {
                        hub_url: &hub_url,
                        server: &server,
                        home: &home,
                        model_opts: &model_opts,
                        bots: &bots,
                        bot: &bots.get(&r.bot)?,
                        task: &r.instructions,
                        approver: approve::handler(approve),
                        demo,
                        demo_tools: false,
                        demo_secret: false,
                        fallback: "Routine ran.",
                        thread: Thread::Own,
                        max_steps: DEFAULT_MAX_STEPS,
                        history: DEFAULT_HISTORY,
                        watch: None,
                        cancel: None,
                        redirects: None,
                    })
                    .await;
                    // No missed-firings preamble, unlike `tick`. This run was
                    // asked for now; telling the Bot it had been asleep and
                    // should cover a gap would make a rehearsal do different
                    // work from the thing being rehearsed.
                    let run = match &outcome {
                        Ok(o) => openbot_bots::Run {
                            at: now,
                            ok: o.succeeded(),
                            summary: if o.text.is_empty() {
                                format!("{:?}", o.reason)
                            } else {
                                o.text.chars().take(200).collect()
                            },
                            steps: o.steps,
                            tokens_in: o.usage.input_tokens,
                            tokens_out: o.usage.output_tokens,
                            // Nothing to retry: nobody is owed this run, so
                            // there is no schedule for a retry to belong to.
                            // `record_manual_run` would ignore it anyway.
                            retryable: false,
                            manual: true,
                        },
                        Err(e) => openbot_bots::Run {
                            at: now,
                            ok: false,
                            summary: e.to_string().chars().take(200).collect(),
                            steps: 0,
                            tokens_in: 0,
                            tokens_out: 0,
                            retryable: false,
                            manual: true,
                        },
                    };
                    render::outln!("  {} {}", if run.ok { "ok" } else { "failed" }, run.summary);
                    // The recorder that leaves the schedule alone. Using the
                    // ordinary one here would set `last_run`, and the firing
                    // this was rehearsing would silently never happen.
                    bots.record_manual_run(&mut r, run)?;
                    match r.next_after(now)? {
                        Some(t) => println!(
                            "
  the schedule is unchanged; next {}",
                            t.to_rfc3339()
                        ),
                        None => println!(
                            "
  the schedule is unchanged"
                        ),
                    }
                }
                RoutineCmd::Tick {
                    dry_run,
                    approve,
                    demo,
                    idle_days,
                } => {
                    // Before anything runs, apply the idle guard.
                    if idle_days > 0 {
                        let idle = bots.idle_routines(now, chrono::Duration::days(idle_days))?;
                        if !idle.is_empty() {
                            let since = bots
                                .idle_since()
                                .map(|t| t.format("%Y-%m-%d").to_string())
                                .unwrap_or_default();
                            for mut r in idle {
                                r.enabled = false;
                                bots.save_routine(&r)?;
                                render::outln!(
                                    "paused {}/{} — nobody has looked since {since}",
                                    r.bot,
                                    r.id
                                );
                            }
                            println!(
                                "\n  Routines pause after {idle_days} days with nobody watching, \
                                 so an agent does not keep spending while you are away.\n  \
                                 `openbot routine resume <bot> <id>` starts one again; \
                                 `--idle-days 0` turns this off."
                            );
                        }
                    }
                    let due = bots.due(now)?;
                    if due.is_empty() {
                        println!("nothing due");
                    }
                    for mut r in due {
                        let missed = r.missed(now)?;
                        if dry_run {
                            render::outln!(
                                "would run {}/{}{}",
                                r.bot,
                                r.id,
                                if missed > 0 {
                                    format!("  ({missed} missed)")
                                } else {
                                    String::new()
                                }
                            );
                            continue;
                        }
                        let bot = bots.get(&r.bot)?;
                        render::outln!("running {}/{}", r.bot, r.id);
                        if missed > 0 {
                            // Report rather than replay: one pass covering the
                            // gap beats many identical digests.
                            render::outln!("  {missed} firings were missed; running once");
                        }

                        let mut task = r.instructions.clone();
                        if missed > 0 {
                            task = format!(
                                "{task}\n\n(You were not running for a while and missed \
                                 {missed} scheduled runs. Cover the whole gap in one pass \
                                 rather than repeating the work.)"
                            );
                        }
                        let outcome = run_task(Task {
                            hub_url: &hub_url,
                            server: &server,
                            home: &home,
                            model_opts: &model_opts,
                            bots: &bots,
                            bot: &bot,
                            task: &task,
                            approver: approve::handler(approve),
                            demo,
                            demo_tools: false,
                            demo_secret: false,
                            fallback: "Routine ran.",
                            thread: Thread::Own,
                            max_steps: DEFAULT_MAX_STEPS,
                            history: DEFAULT_HISTORY,
                            watch: None,
                            cancel: None,
                            redirects: None,
                        })
                        .await;
                        let run = match &outcome {
                            Ok(o) => openbot_bots::Run {
                                at: now,
                                ok: o.succeeded(),
                                summary: if o.text.is_empty() {
                                    format!("{:?}", o.reason)
                                } else {
                                    o.text.chars().take(200).collect()
                                },
                                steps: o.steps,
                                tokens_in: o.usage.input_tokens,
                                tokens_out: o.usage.output_tokens,
                                // As on the event path: a held computer and a
                                // transient provider failure both leave the
                                // work undone, so the routine stays due.
                                retryable: matches!(
                                    o.reason,
                                    openbot_agent::FinishReason::ComputerBusy { .. }
                                        | openbot_agent::FinishReason::ModelFailed {
                                            transient: true,
                                            ..
                                        }
                                ),
                                manual: false,
                            },
                            Err(e) => openbot_bots::Run {
                                at: now,
                                ok: false,
                                summary: e.to_string().chars().take(200).collect(),
                                steps: 0,
                                tokens_in: 0,
                                tokens_out: 0,
                                // As on the event path: an outage is retried,
                                // a refusal is not.
                                retryable: openbot_agent::is_transient(e),
                                manual: false,
                            },
                        };
                        render::outln!(
                            "  {} {}",
                            if run.ok { "ok" } else { "failed" },
                            run.summary
                        );
                        bots.record_run(&mut r, run)?;
                    }
                }
            }
        }

        Command::Computer { store, volume, cmd } => {
            let s = openbot_store::Store::open(&store)?;
            let v = s.volume(&volume)?;
            match cmd {
                ComputerCmd::Status { json } => {
                    let u = v.usage()?;
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "volume": v.id(),
                                "workspace": v.workspace(),
                                "files": u.files,
                                "bytes": u.bytes,
                                "snapshots": u.snapshots,
                            })
                        );
                        return Ok(());
                    }
                    println!("volume     {}", v.id());
                    println!("workspace  {}", v.workspace().display());
                    println!("live       {} files · {}", u.files, human(u.bytes));
                    println!(
                        "snapshots  {} · {} stored",
                        u.snapshots,
                        human(u.snapshot_bytes)
                    );
                    println!(
                        "attached   {}",
                        if v.is_attached() {
                            "yes — a guest is running"
                        } else {
                            "no"
                        }
                    );
                    if let Some(l) = v.latest()? {
                        println!("latest     {}  {}", l.id, l.label);
                    }
                }
                ComputerCmd::Snapshots => {
                    let snaps = v.snapshots()?;
                    if snaps.is_empty() {
                        println!("no snapshots yet");
                    }
                    for s in snaps {
                        render::outln!(
                            "{:<14} {:>6} files  {:>9}  {}",
                            s.id.as_str(),
                            s.files,
                            human(s.bytes),
                            s.label
                        );
                    }
                }
                ComputerCmd::Snapshot { label } => {
                    let s = v.snapshot(&label)?;
                    println!("{}  {} files · {}", s.id, s.files, human(s.bytes));
                }
                ComputerCmd::Restore { id } => {
                    let safety = v.restore(&openbot_store::SnapshotId(id.clone()))?;
                    println!("restored {id}");
                    println!("previous state saved as {} — restore it to undo", safety.id);
                }
                ComputerCmd::ForceDetach => {
                    v.force_detach()?;
                    println!("lock cleared");
                }
                ComputerCmd::Prune { keep } => {
                    let n = v.prune(keep)?;
                    println!("removed {n} snapshots; kept the newest {keep}");
                }
            }
        }

        Command::Status { home } => {
            let s = status::gather(&hub_url, &home, &model_opts).await;
            print!("{}", status::render(&s, render::Style::detect()));
        }

        Command::Servers => {
            let (hub, _p) = HubClient::connect(&cli.hub).await?;
            let servers = hub.list_servers().await?;
            if servers.is_empty() {
                println!("no tool servers are registered with {}", cli.hub);
            }
            for s in servers {
                render::outln!("{}  {}", s.server_id, s.description.unwrap_or_default());
            }
        }

        Command::Up {
            bind,
            home,
            workspace,
            snapshot_every,
            snapshot_keep,
            routines_every,
        } => {
            let r = up::Up {
                bind,
                paths: up::Paths { home, workspace },
                server_id: server.clone(),
                snapshot_every: (snapshot_every > 0)
                    .then(|| std::time::Duration::from_secs(snapshot_every * 60)),
                snapshot_keep,
                routines_every: (routines_every > 0)
                    .then(|| std::time::Duration::from_secs(routines_every * 60)),
            }
            .start()
            .await?;
            print!("{}", up::banner(&r, render::Style::detect()));
            // Every way this is stopped (Ctrl-C, Ctrl-Break, and SIGTERM
            // from an orchestrator), not just the one typed at a terminal.
            openbot_guest::stop_signal().await;
            // Exiting runs no destructors, so `kill_on_drop` does not cover
            // this path; the browser has to be torn down explicitly.
            r.stop().await;
            println!("\n  stopped");
        }

        Command::Watch { port } => {
            let w = watch::Watch::connect(&hub_url, &server).await?;
            // Loopback is decided by `watch`, the module that guarantees it;
            // see `Watch::listen`.
            let listener = watch::Watch::listen(port).await?;
            let addr = listener.local_addr()?;
            println!(
                "watching {server}
"
            );
            println!(
                "  {}
",
                w.url(addr)
            );
            println!("  The link carries a one-time key — anything without it is refused,");
            println!("  so a web page you happen to have open cannot drive this computer.");
            println!("  Ctrl-C to stop; the computer is released automatically.");
            tokio::select! {
                _ = Arc::clone(&w).serve(listener) => {}
                _ = tokio::signal::ctrl_c() => {
                    // Politeness; the hub releases on disconnect regardless.
                    let _ = w.stop().await;
                    println!("\n  stopped");
                }
            }
        }

        Command::Attach { file, json } => {
            let (hub, _p) = HubClient::connect(&cli.hub).await?;
            hub.open_session().await?;
            // Ask the guest that is actually serving, rather than working the
            // path out from the store. See `openbot_proto::META_WORKSPACE`.
            let servers = hub.list_servers().await?;
            let root = servers
                .iter()
                .find(|s| s.server_id.as_str() == cli.server)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        concat!(
                            "no computer called `{}` is attached to {}
",
                            "  Start one with `openbot up`."
                        ),
                        cli.server,
                        cli.hub
                    )
                })?
                .metadata
                .as_ref()
                .and_then(|m| m.get(openbot_proto::META_WORKSPACE))
                .and_then(|w| w.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        concat!(
                            "`{}` did not say where its workspace is, so this ",
                            "cannot know where to put the file. The computer is ",
                            "older than this command; restart `openbot up`."
                        ),
                        cli.server
                    )
                })?
                .to_owned();

            let at = put_attachment(std::path::Path::new(&root), &file)?;
            if json {
                println!("{}", serde_json::json!({ "path": at }));
            } else {
                render::outln!("attached as {at}");
            }
        }

        Command::Tools => {
            let (hub, _p) = HubClient::connect(&cli.hub).await?;
            hub.open_session().await?;
            let tools = hub.bind_server(&cli.server).await?;
            for t in tools {
                render::outln!("{:<24} {}", t.tool_id.as_str(), t.description);
            }
        }

        Command::Call {
            tool,
            args,
            approve,
            no_server,
        } => {
            let args: serde_json::Value = serde_json::from_str(&args).map_err(|e| {
                anyhow::anyhow!("arguments must be one JSON object: {e}\n  openbot call fs.read '{{\"path\":\"notes.md\"}}'")
            })?;

            let (hub, _p) = HubClient::connect_with(&cli.hub, approve::handler(approve)).await?;
            hub.open_session().await?;
            if !no_server {
                // A guest may not be running, and hub-served tools do not need
                // one. Say so rather than failing with a routing error.
                if let Err(e) = hub.bind_server(&cli.server).await {
                    eprintln!("note: no tool server bound ({e}); hub-served tools still work");
                }
            }

            let call_id = openbot_proto::ToolCallId::new(format!("cli-{}", std::process::id()));
            match hub.call_tool(&tool, &call_id, args).await {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                Err(e) => anyhow::bail!("{tool} failed: {e}"),
            }
        }

        Command::Search {
            query,
            home,
            all,
            json,
        } => {
            let needle = query.join(" ");
            let needle = needle.trim();
            if needle.is_empty() {
                anyhow::bail!("nothing to look for: openbot search \"renewal risk\"");
            }
            let bots = openbot_bots::BotStore::open(&home)?;
            let lower = needle.to_lowercase();

            // Text only. A tool's arguments and output are not what "find
            // where we discussed the renewal" means; matching them buries the
            // sentence being looked for under the call that followed it.
            let mut hits = Vec::new();
            let mut scan = |where_: &str, who: &str, msgs: &[openbot_agent::Message]| {
                for (i, m) in msgs.iter().enumerate() {
                    for c in &m.content {
                        let openbot_agent::Content::Text { text } = c else {
                            continue;
                        };
                        if !text.to_lowercase().contains(&lower) {
                            continue;
                        }
                        hits.push(serde_json::json!({
                            "kind": where_,
                            "name": who,
                            "at": i,
                            "role": match m.role {
                                openbot_agent::Role::User => "user",
                                openbot_agent::Role::Assistant => "assistant",
                            },
                            "text": snippet(text, &lower),
                        }));
                        break;
                    }
                }
            };

            for b in bots.list(all)? {
                let msgs = bots.history(&b.id, None)?;
                scan("bot", b.id.as_str(), &msgs);
            }
            for g in bots.groups(all)? {
                let msgs = bots.group_history(&g.id, None)?;
                scan("group", g.id.as_str(), &msgs);
            }

            if json {
                println!("{}", serde_json::to_string(&hits)?);
                return Ok(());
            }
            if hits.is_empty() {
                println!("nothing said `{needle}`");
            }
            for h in &hits {
                render::outln!(
                    "{:<14} {:<20} {}",
                    h["kind"].as_str().unwrap_or(""),
                    h["name"].as_str().unwrap_or(""),
                    h["text"].as_str().unwrap_or("")
                );
            }
        }

        Command::Permission { home, cmd } => match cmd {
            PermissionCmd::Ls { json } => {
                let rules = config::rules(&home)?;
                if json {
                    println!("{}", serde_json::to_string(&rules)?);
                    return Ok(());
                }
                if rules.is_empty() {
                    println!("no rules — the shipped default applies: read is free, change asks");
                }
                for (i, r) in rules.iter().enumerate() {
                    let when = r
                        .when
                        .as_ref()
                        .map(|w| format!(" when {}={}", w.key, w.glob))
                        .unwrap_or_default();
                    render::outln!(
                        // 16 was the width of "requireapproval". The longest
                        // action is "require_approval" at 16, so that column
                        // had exactly no gap after it once the name was
                        // spelled correctly.
                        "{:>2}  {:<17} {}{}{}",
                        i + 1,
                        // Serialised, not `Debug`-lowercased. The two agree
                        // for `allow` and `deny` and not for the one in the
                        // middle: `{:?}` gives "RequireApproval", which
                        // lowercases to "requireapproval" - a word no file may
                        // contain and no error message mentions, printed by the
                        // command people run to check what they wrote.
                        serde_json::to_value(r.action)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_else(|| format!("{:?}", r.action).to_lowercase()),
                        r.tool,
                        when,
                        r.reason
                            .as_ref()
                            .map(|s| format!("  — {s}"))
                            .unwrap_or_default()
                    );
                }
            }
            PermissionCmd::Add {
                action,
                tool,
                when_key,
                when_glob,
                reason,
            } => {
                let act = match action.as_str() {
                    "allow" => openbotd::policy::Action::Allow,
                    "ask" | "require_approval" => openbotd::policy::Action::RequireApproval,
                    "deny" => openbotd::policy::Action::Deny,
                    other => {
                        anyhow::bail!("unknown action `{other}` — use `allow`, `ask`, or `deny`")
                    }
                };
                // A rule that stops or refuses a call must say why: the reason
                // is what a person reads in the approval, and what whoever
                // reads the log afterwards has to go on.
                if reason.is_none() && !matches!(act, openbotd::policy::Action::Allow) {
                    anyhow::bail!(
                        "`--reason` is required for `{action}` — it is what a person is shown when this rule stops a call"
                    );
                }
                let mut rule = openbotd::policy::Rule {
                    action: act,
                    tool,
                    when: None,
                    reason,
                };
                if let (Some(key), Some(glob)) = (when_key, when_glob) {
                    rule = rule.when(&key, &glob);
                }
                let shown = rule.tool.clone();
                config::edit_rules(&home, |list| {
                    list.push(toml::Value::try_from(&rule)?);
                    Ok(())
                })?;
                println!("added a rule for {shown}");
                println!("  restart `openbot up` for it to take effect");
            }
            PermissionCmd::Rm { number } => {
                config::edit_rules(&home, |list| {
                    if number == 0 || number > list.len() {
                        anyhow::bail!(
                            "no rule {number} — there {} {}",
                            if list.len() == 1 { "is" } else { "are" },
                            match list.len() {
                                0 => "none".to_owned(),
                                n => n.to_string(),
                            }
                        );
                    }
                    list.remove(number - 1);
                    Ok(())
                })?;
                println!("removed rule {number}");
                println!("  restart `openbot up` for it to take effect");
            }
        },

        Command::Secret { home, cmd } => {
            let store = openbotd::secrets::SecretStore::open(&home)?;
            match cmd {
                SecretCmd::Set { name } => {
                    let value = read_secret_from_stdin(&name)?;
                    store.set(&name, openbotd::secrets::Secret::new(value))?;
                    let fp = store.fingerprint(&name)?;
                    println!("stored {name} ({fp})");
                    println!(
                        "  use it:  openbot connector add <id> <url> --authorization \
                         \"Bearer ${{{name}}}\""
                    );
                }
                SecretCmd::Ls { json } => {
                    let names = store.names()?;
                    if json {
                        // Names and fingerprints, never a value. This is the
                        // one listing where the interesting field is absent by
                        // design: no client should be able to show a
                        // credential it did not need.
                        let mut rows = Vec::new();
                        for n in &names {
                            rows.push(serde_json::json!({
                                "name": n,
                                "fingerprint": store.fingerprint(n)?,
                            }));
                        }
                        println!("{}", serde_json::to_string(&rows)?);
                        return Ok(());
                    }
                    if names.is_empty() {
                        println!(
                            "no secrets yet — `cat token.txt | openbot secret set linear-token`"
                        );
                    }
                    for n in names {
                        // A fingerprint, never a value: enough to tell two
                        // tokens apart or confirm one was rotated.
                        render::outln!("{:<24} {}", n, store.fingerprint(&n)?);
                    }
                }
                SecretCmd::Rm { name } => {
                    store.remove(&name)?;
                    println!("removed {name}");
                }
            }
        }

        Command::Skill { home, cmd } => {
            let dir = openbotd::skills::dir(&home);
            match cmd {
                SkillCmd::Ls { json } => {
                    let skills = openbotd::skills::Skills::load(&home);
                    if json {
                        // An object, where every sibling `--json` is an
                        // array. This is the one place the convention does
                        // not fit: a skill that failed to load is as much an
                        // answer as one that loaded, and a bare array of the
                        // working ones would make a half-written skill
                        // invisible to any client, which is a Bot silently
                        // ignoring a procedure somebody wrote.
                        //
                        // `body` is left out. A client offering skills by name
                        // needs the name and the description a Bot decides
                        // on; the full text is what `skill show` is for.
                        let out = serde_json::json!({
                            "skills": skills.all()
                                .iter()
                                .map(|s| serde_json::json!({
                                    "name": s.name,
                                    "description": s.description,
                                }))
                                .collect::<Vec<_>>(),
                            "problems": skills.problems()
                                .iter()
                                .map(|(path, why)| serde_json::json!({
                                    "path": path.display().to_string(),
                                    "why": why,
                                }))
                                .collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string(&out)?);
                        return Ok(());
                    }
                    if skills.is_empty() && skills.problems().is_empty() {
                        println!(
                            "no skills yet — try: openbot skill new refund-a-customer --description \"How to issue a refund\""
                        );
                    }
                    for s in skills.all() {
                        render::outln!("{:<28} {}", s.name, s.description);
                    }
                    // The half-written ones matter more than the working ones:
                    // a skill that silently does not load is a Bot that
                    // silently ignores its instructions.
                    for (path, why) in skills.problems() {
                        render::outln!(
                            "
  ! {} — {why}",
                            path.display()
                        );
                    }
                }

                SkillCmd::New { name, description } => {
                    let slug = name.trim().to_lowercase().replace(' ', "-");
                    if slug.is_empty() || slug.contains(['/', '\\']) || slug.contains("..") {
                        anyhow::bail!("a skill name cannot be empty or contain a path");
                    }
                    let d = dir.join(&slug);
                    if d.join("SKILL.md").exists() {
                        anyhow::bail!("`{slug}` already exists at {}", d.display());
                    }
                    std::fs::create_dir_all(&d)?;
                    // Built from lines rather than one multi-line literal: a
                    // literal here would indent every line of the written file
                    // with the indentation of this source.
                    let body = [
                        "---".to_owned(),
                        format!("name: {slug}"),
                        format!("description: {description}"),
                        "---".to_owned(),
                        String::new(),
                        "Write the procedure here, as steps.".to_owned(),
                        String::new(),
                        "A Bot reads this only after deciding the description above".to_owned(),
                        "applies, so put the *when* in the description and the *how*".to_owned(),
                        "here.".to_owned(),
                        String::new(),
                    ]
                    .join(
                        "
",
                    );
                    std::fs::write(d.join("SKILL.md"), body)?;
                    println!("created {}", d.join("SKILL.md").display());
                    println!("  restart openbotd to pick it up");
                }

                SkillCmd::Show { name } => {
                    let skills = openbotd::skills::Skills::load(&home);
                    let s = skills
                        .all()
                        .iter()
                        .find(|s| s.name == name)
                        .ok_or_else(|| anyhow::anyhow!("no skill called `{name}`"))?;
                    // A skill body is arbitrarily long, and the kind of output
                    // that gets piped into a pager.
                    render::outln!("{}\n{}\n\n{}", s.name, s.description, s.body);
                }

                SkillCmd::Rm { name } => {
                    let skills = openbotd::skills::Skills::load(&home);
                    let known = skills.all().iter().any(|s| s.name == name);
                    let d = dir.join(&name);
                    if !known && !d.exists() {
                        anyhow::bail!("no skill called `{name}`");
                    }
                    std::fs::remove_dir_all(&d)?;
                    println!("removed {}", d.display());
                }
            }
        }

        Command::Connector { home, cmd } => {
            let mut cs = openbotd::connector::Connectors::load(&home)?;
            match cmd {
                ConnectorCmd::Add {
                    id,
                    url,
                    authorization,
                    no_verify,
                } => {
                    let c = openbotd::connector::Connector {
                        id: id.clone(),
                        url,
                        authorization,
                    };
                    c.validate()?;

                    // Fail here, where the person still has the context, rather
                    // than later inside an unattended routine.
                    let secrets = Arc::new(openbotd::secrets::SecretStore::open(&home)?);
                    for name in c.secret_refs() {
                        if secrets.get(&name).is_err() {
                            anyhow::bail!(
                                "no secret named `{name}` — store it first:\n  \
                                 cat token.txt | openbot secret set {name}"
                            );
                        }
                    }

                    if !no_verify {
                        let names = probe(&c, Arc::clone(&secrets)).await?;
                        cs.add(c)?;
                        cs.save(&home)?;
                        println!("added {id} — {}", plural(names.len(), "tool"));
                        for n in &names {
                            render::outln!("  {n}");
                        }
                    } else {
                        cs.add(c)?;
                        cs.save(&home)?;
                        println!("added {id} (unverified)");
                    }
                    println!("\nrestart openbotd to pick it up");
                }
                ConnectorCmd::Ls { json } => {
                    if json {
                        let rows: Vec<serde_json::Value> = cs
                            .connectors
                            .iter()
                            .map(|c| {
                                serde_json::json!({
                                    "id": c.id,
                                    "url": c.url,
                                    // The credential names a connector needs,
                                    // never a value; no path reads one back.
                                    "secrets": c.secret_refs(),
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string(&rows)?);
                        return Ok(());
                    }
                    if cs.connectors.is_empty() {
                        println!("no connectors yet");
                    }
                    for c in &cs.connectors {
                        let refs = c.secret_refs();
                        render::outln!(
                            "{:<16} {:<44} {}",
                            c.id,
                            c.url,
                            if refs.is_empty() {
                                "(no credential)".to_owned()
                            } else {
                                refs.join(", ")
                            }
                        );
                    }
                }
                ConnectorCmd::Test { id } => {
                    let c = cs
                        .connectors
                        .iter()
                        .find(|c| c.id == id)
                        .ok_or_else(|| anyhow::anyhow!("no connector called `{id}`"))?;
                    let secrets = Arc::new(openbotd::secrets::SecretStore::open(&home)?);
                    let names = probe(c, secrets).await?;
                    println!("{id} answered — {}", plural(names.len(), "tool"));
                    for n in &names {
                        render::outln!("  {n}");
                    }
                }
                ConnectorCmd::Rm { id } => {
                    let gone = cs.remove(&id)?;
                    cs.save(&home)?;
                    println!("removed {}", gone.id);
                    for n in gone.secret_refs() {
                        render::outln!(
                            "  `{n}` is still stored; `openbot secret rm {n}` to forget it"
                        );
                    }
                }
            }
        }

        Command::Run {
            task,
            max_steps,
            verbose,
            approve,
            demo_secret,
            html,
            demo,
            demo_url,
            bot,
            home,
            history,
        } => {
            let task = task.join(" ");
            if task.trim().is_empty() {
                anyhow::bail!("give me a task: openbot run \"summarise the notes in /workspace\"");
            }

            let acting_as = bot.clone();
            // A computer, whether or not the person started one. See
            // `up::hub_or_start`: the two-terminal dance was an implementation
            // detail leaking into the first thing anyone types.
            let (hub_url, own_stack) = up::hub_or_start(
                &cli.hub,
                up::Paths {
                    home: home.clone(),
                    workspace: None,
                },
                &cli.server,
                render::Style::detect(),
            )
            .await?;
            let (hub, progress) = HubClient::connect_with(&hub_url, approve::handler(approve))
                .await
                .map_err(|e| anyhow::anyhow!("could not reach the hub at {hub_url}: {e}"))?;
            hub.open_session_as(acting_as.as_deref()).await?;
            let tools = hub.bind_server(&cli.server).await.map_err(|e| {
                anyhow::anyhow!(
                    "could not bind tool server `{}`: {e}\n\
                     Is `openbot-guest` running and pointed at this hub?",
                    cli.server
                )
            })?;

            let model: Arc<dyn Model> = if demo_secret {
                Arc::new(secret_demo_script())
            } else if demo {
                Arc::new(demo_script(demo_url.as_deref()))
            } else {
                // Nothing configured is not the same as nothing available. A
                // great many people who would try this already have Ollama or
                // LM Studio running with a model downloaded; telling them to go
                // and get an account is asking them to configure what is
                // already on the machine. See `discover`.
                let model_opts = adopt_local_model_if_needed(&home, model_opts.clone()).await;
                config::build(&home, &model_opts, false, "")?
            };

            // Load the Bot, if one was named: its brief shapes the system
            // prompt and its history seeds the run.
            let (bot, prior, waiting) = match &bot {
                Some(name) => {
                    let bots = openbot_bots::BotStore::open(&home)?;
                    let b = bots.resolve(name)?;
                    // Recover anything a previous crash left mid-drain before
                    // taking the inbox, or those handoffs are lost silently.
                    bots.recover_inbox(&b.id)?;
                    let h = bots.history(&b.id, Some(history))?;
                    let waiting = bots.drain_inbox(&b.id)?;
                    (Some((bots, b)), h, waiting)
                }
                None => (None, Vec::new(), Vec::new()),
            };

            // Handoffs go in front of the task, so the Bot reads what arrived
            // before deciding how to do what was asked.
            let task = if waiting.is_empty() {
                task
            } else {
                format!(
                    "{}{}",
                    openbot_bots::BotStore::handoff_preamble(&waiting),
                    task
                )
            };

            let model_name = model.name().to_owned();
            let mut r = Renderer::new(verbose);
            r.header(&model_name, &hub_url, tools.len());
            if !waiting.is_empty() {
                r.handoffs(waiting.len());
            }
            r.task(&task);

            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
            let want_html = html.is_some();
            let renderer = tokio::spawn(async move {
                let mut seen = Vec::new();
                while let Some(e) = ev_rx.recv().await {
                    r.event(&e);
                    if want_html {
                        seen.push(e);
                    }
                }
                seen
            });

            let system = match &bot {
                Some((_, b)) => b.system_prompt(&openbot_agent::agent::default_system_prompt()),
                None => openbot_agent::agent::default_system_prompt(),
            };
            // Ctrl-C stops the run rather than killing the process, so what
            // the Bot did this turn is written to its conversation instead of
            // thrown away; killing the process would skip the append below.
            //
            // The second press is not swallowed. Once tokio installs a handler
            // the default one is gone for the rest of the process, so a run
            // that will not wind down would otherwise become unkillable from
            // the keyboard. The second press exits, and the first says so.
            let (stop, stopped) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                openbot_guest::stop_signal().await;
                let _ = stop.send(true);
                // stderr: a status line must not land in a piped transcript.
                eprintln!("stopping — press Ctrl-C again to quit without saving");
                openbot_guest::stop_signal().await;
                // 130 is what a shell reports for a run ended by Ctrl-C.
                std::process::exit(130);
            });

            let agent = Agent::new(
                model,
                hub,
                AgentConfig {
                    max_steps,
                    system,
                    token_budget: config::token_budget(&home, &model_opts),
                    ..Default::default()
                },
            )
            .with_history(prior)
            .with_cancel(stopped);
            let started_from = agent.history_len();
            let outcome = agent.run(&task, tools, progress, ev_tx).await;

            // Persist only what this run added, so re-running never duplicates
            // the history it was seeded with.
            if let Some((bots, b)) = &bot {
                let fresh = &outcome.transcript[started_from.min(outcome.transcript.len())..];
                bots.append(&b.id, fresh)?;
            }
            let events = renderer.await.unwrap_or_default();

            if let Some(path) = html {
                let page = html::render(&html::Session {
                    task: &task,
                    model: &model_name,
                    hub: &hub_url,
                    events: &events,
                });
                std::fs::write(&path, page)
                    .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))?;
                println!("  transcript → {}", path.display());
            }

            // Tear down a stack this command started, before either exit
            // below. `std::process::exit` runs no destructors, so the browser
            // teardown has to happen here; the `?` paths above are covered,
            // because returning an error unwinds and `kill_on_drop` reaps the
            // child. Nothing to do when the person started their own hub — it
            // is not ours to stop.
            if let Some(stack) = &own_stack {
                stack.stop().await;
            }

            // A step-limited or failed run must not look like success to a
            // shell script that only checks the exit code.
            //
            // A cancelled one is neither, and gets 130, which is what a shell
            // reports for a process ended by an interrupt. An intentional stop
            // is not a failure of the run, and a wrapper that retries failures
            // should not retry a run that was just stopped; the exit code is
            // how it tells the two apart.
            if matches!(
                outcome.reason,
                openbot_agent::FinishReason::Cancelled { .. }
            ) {
                std::process::exit(130);
            }
            if !outcome.succeeded() {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// What `openbot` on its own does.
///
/// It used to print forty subcommands, which is the least useful thing a
/// program can say to somebody who has just installed it: every one of them is
/// equally plausible and none of them is the next step. This is that screen
/// replaced by the two facts that decide what to do next — whether there is a
/// model, and what to type.
///
/// The setup it offers is the one that needs no account: a model already
/// running on this machine. When there is one, saying yes is the whole of the
/// configuration, and `openbot config set --model … --dialect … --base-url …
/// --api-key-env ''` never has to be read or typed.
///
/// Writing to the config happens only on a yes, and only at a terminal. A
/// person piping this somewhere has not been asked anything and must not have
/// their configuration written for them.
async fn welcome() -> anyhow::Result<()> {
    use std::io::{IsTerminal, Write};

    let st = render::Style::detect();
    // `--home` is declared on each subcommand, with `env = "OPENBOT_HOME"`, and
    // there is no subcommand here — so the variable has to be read directly or
    // this screen reports on a different home than every other command uses.
    let home = std::env::var("OPENBOT_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_HOME.as_str()));

    println!();
    if let Some(model) = config::configured_model(&home) {
        println!("  {}  {}", st.dim("model "), model);
        println!("  {}  {}", st.dim("home  "), home.display());
        println!();
        // Padded rather than spaced inside the literal: a run of spaces in a
        // string is what `messages.rs` looks for when catching a literal that
        // has picked up this file's indentation, and it cannot tell alignment
        // from a wrapped line. `{:<24}` says what is meant anyway.
        for (cmd, what) in [
            ("openbot run \"...\"", "give it a task"),
            ("openbot status", "is anything wrong?"),
            ("openbot --help", "everything else"),
        ] {
            println!("  {}", st.dim(&format!("{cmd:<24}{what}")));
        }
        println!();
        return Ok(());
    }

    println!("  {}", st.dim("No model configured yet."));
    let found = discover::local_model().await;
    let Some(found) = found else {
        // Two honest routes, shortest first, and no list of forty commands.
        println!();
        println!("  A model on this machine needs no account and sends nothing off it:");
        println!(
            "    {}",
            st.dim("install Ollama, then:  ollama pull qwen3:1.7b")
        );
        println!(
            "    {}",
            st.dim("then run `openbot` again and it will be found")
        );
        println!();
        println!("  Or point it at a provider you already pay for:");
        println!(
            "    {}",
            st.dim("openbot config set --model grok-4-5 --api-key-env XAI_API_KEY")
        );
        println!();
        println!(
            "  {}",
            st.dim("openbot run --demo --approve auto \"prove it\"   # no model needed")
        );
        println!();
        return Ok(());
    };

    println!(
        "  Found {} running here, serving {}.",
        found.served_by, found.model
    );
    if !std::io::stdin().is_terminal() {
        // Nothing was asked, so nothing is decided. Printing the command keeps
        // this usable from a script without writing to anyone's config.
        println!();
        println!(
            "  {}",
            st.dim(&format!("use it:  {}", found.to_config_command()))
        );
        println!();
        return Ok(());
    }

    print!("  Use it? [Y/n] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if !(answer.is_empty() || answer == "y" || answer == "yes") {
        println!();
        println!(
            "  {}",
            st.dim(&format!("when you want it:  {}", found.to_config_command()))
        );
        println!();
        return Ok(());
    }

    let mut c = config::load(&home)?;
    c.model.id = Some(found.model.clone());
    c.model.dialect = "openai".to_owned();
    c.model.base_url = found.base_url.clone();
    // Empty means "this endpoint wants no credential", which is the whole point
    // of a model on localhost. Absent would send the next run looking for a key.
    c.model.api_key_env = String::new();
    config::save(&home, &c)?;

    println!();
    println!("  {} {}", st.green("ready ·"), found.model);
    println!(
        "  {}",
        st.dim(&format!("saved to {}", config::path(&home).display()))
    );
    println!();
    println!(
        "  {}",
        st.dim("openbot run \"summarise the notes in the workspace\"")
    );
    println!();
    Ok(())
}

/// Borrow a model that is already running here, when none is configured.
///
/// Only when none is: an explicit `--model`, or one in `config.toml`, is a
/// decision somebody made, and quietly using something else because it happened
/// to be listening would be the worst kind of helpful.
///
/// The adoption lasts for this command. Writing it into `config.toml` would be
/// a decision made on the person's behalf, and they may well have opened this
/// terminal intending to use a frontier model. The command that would make it
/// permanent is printed instead, so the choice stays theirs and costs one
/// paste.
///
/// Announced on stderr, not stdout: `openbot run` output is piped into things.
async fn adopt_local_model_if_needed(
    home: &std::path::Path,
    opts: config::ModelOverrides,
) -> config::ModelOverrides {
    if opts.model.is_some() || config::configured_model(home).is_some() {
        return opts;
    }
    let Some(found) = discover::local_model().await else {
        return opts;
    };

    let st = render::Style::detect();
    let others = match found.also {
        0 => String::new(),
        1 => " (1 other available)".to_owned(),
        n => format!(" ({n} others available)"),
    };
    eprintln!(
        "  {} {} via {}{}",
        st.dim("no model configured; using"),
        found.model,
        found.served_by,
        st.dim(&others)
    );
    eprintln!(
        "  {}",
        st.dim(&format!("keep it:  {}", found.to_config_command()))
    );

    config::ModelOverrides {
        model: Some(found.model),
        dialect: Some("openai".to_owned()),
        base_url: Some(found.base_url),
        // Empty, not absent: it is the configured answer meaning "this endpoint
        // wants no credential". Leaving it unset would send the run looking for
        // a key that a model on localhost never needed.
        api_key_env: Some(String::new()),
        ..opts
    }
}

/// Read a credential from standard input.
///
/// Piped is the intended shape. When run at a terminal anyway, the prompt says
/// that the value will be visible: there is no terminal-echo control here, and
/// a silent prompt that echoes would be worse than one that warns.
fn read_secret_from_stdin(name: &str) -> anyhow::Result<String> {
    use std::io::{IsTerminal, Read};

    let mut buf = String::new();
    if std::io::stdin().is_terminal() {
        eprintln!("Paste the value for `{name}` and press Enter.");
        eprintln!("(it will be visible on screen — pipe it instead to avoid that:");
        eprintln!("   cat token.txt | openbot secret set {name} )");
        std::io::stdin().read_line(&mut buf)?;
    } else {
        std::io::stdin().read_to_string(&mut buf)?;
    }
    if buf.trim().is_empty() {
        anyhow::bail!("nothing on stdin — `cat token.txt | openbot secret set {name}`");
    }
    // Trailing newline handling and header-safety live in the store, so every
    // path in gets the same treatment.
    Ok(buf)
}

/// Ask a connector what it offers, using its stored credential.
async fn probe(
    c: &openbotd::connector::Connector,
    secrets: Arc<openbotd::secrets::SecretStore>,
) -> anyhow::Result<Vec<String>> {
    let tools = openbotd::connector::ConnectorTools::discover(vec![c.clone()], secrets).await;
    let names = tools.tool_names();
    if names.is_empty() {
        anyhow::bail!(
            "`{}` did not answer with a tool list.\n  \
             Check the url and that the credential is current; run with \
             RUST_LOG=openbotd=warn to see why.\n  \
             `--no-verify` saves it anyway.",
            c.id
        );
    }
    Ok(names)
}

/// Record that a person is looking at this account, if one is.
///
/// The signal is a terminal: `openbot routine ls` typed by a person counts,
/// the same command inside a cron job does not. Best-effort: being unable to
/// write the mark must never stop the command that was asked for.
fn watching(home: &std::path::Path) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    if let Ok(bots) = openbot_bots::BotStore::open(home) {
        let _ = bots.mark_seen(chrono::Utc::now());
    }
}

/// Right-pad a status label so the values line up.
pub fn pad_label(s: &str) -> String {
    format!("{s:<10}")
}

/// `1 tool`, `3 tools`.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < UNITS.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{b:.1} {}", UNITS[i])
    }
}

/// Parse a `path=value` or `path~value` condition.
fn parse_condition(s: &str) -> anyhow::Result<openbot_bots::Match> {
    if let Some((path, v)) = s.split_once('~') {
        return Ok(openbot_bots::Match {
            path: path.trim().to_owned(),
            test: openbot_bots::Test::Contains(v.trim().to_owned()),
        });
    }
    if let Some((path, v)) = s.split_once('=') {
        return Ok(openbot_bots::Match {
            path: path.trim().to_owned(),
            test: openbot_bots::Test::Equals(v.trim().to_owned()),
        });
    }
    anyhow::bail!(
        "`{s}` is not a condition — use `path=value` for an exact match or `path~value` for a \
         substring, e.g. --when \"channel=escalations\" --when \"text~needs repro\""
    )
}

/// Defaults for a turn when the caller does not say otherwise.
pub(crate) const DEFAULT_MAX_STEPS: u32 = 24;
pub(crate) const DEFAULT_HISTORY: usize = 40;

/// Where a turn's prior context comes from, and where its transcript goes.
///
/// A Bot turn is seeded with that Bot's own conversation and appends to it. A
/// group turn is seeded with the group thread (a group turn is about what the
/// group has said, not what one member remembers alone), runs under the
/// answering member's brief, and appends back to the thread so the handoff
/// stays visible in one conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Thread<'a> {
    /// The Bot's own conversation.
    Own,
    /// A group's thread, which this Bot is answering in.
    Group(&'a openbot_bots::BotId),
}

/// Everything one turn needs.
///
/// A struct rather than positional parameters: at this width a call site is a
/// column of values whose meaning is their order, and `demo`/`demo_tools` are
/// adjacent `bool`s that would compile and run the wrong demo if swapped.
pub(crate) struct Task<'a> {
    pub hub_url: &'a str,
    pub server: &'a str,
    pub home: &'a std::path::Path,
    pub model_opts: &'a config::ModelOverrides,
    pub bots: &'a openbot_bots::BotStore,
    /// Whose brief the turn runs under. For a group, the answering member.
    pub bot: &'a openbot_bots::Bot,
    pub task: &'a str,
    pub approver: std::sync::Arc<dyn openbot_agent::ApprovalHandler>,
    pub demo: bool,
    pub demo_tools: bool,
    /// Ask for a credential, once, instead of running a model. See the
    /// `--demo-secret` flag.
    pub demo_secret: bool,
    pub fallback: &'a str,
    /// Which conversation seeds the turn and receives it.
    pub thread: Thread<'a>,
    /// How many steps the turn may take, and how much prior conversation
    /// seeds it. Both are flags on `group post` and must be honoured: a flag
    /// accepted and ignored reads as a control that works.
    pub max_steps: u32,
    pub history: usize,
    /// Where the run's events go while it happens. `None` for a routine
    /// nobody is looking at; `Some` for a caller rendering the turn live (the
    /// ACP adapter forwards them to a client as `session/update`).
    pub watch: Option<mpsc::UnboundedSender<AgentEvent>>,
    pub cancel: Option<tokio::sync::watch::Receiver<bool>>,
    /// Where instructions sent while the turn runs arrive.
    pub redirects: Option<openbot_agent::agent::Redirects>,
}

/// Run one turn as a Bot, appending the transcript to the thread it was seeded
/// from.
pub(crate) async fn run_task(t: Task<'_>) -> anyhow::Result<openbot_agent::AgentOutcome> {
    let Task {
        hub_url,
        server,
        home,
        model_opts,
        bots,
        bot,
        task,
        approver,
        demo,
        demo_tools,
        demo_secret,
        fallback,
        thread,
        max_steps,
        history,
        watch,
        cancel,
        redirects,
    } = t;
    // The handler arrives built rather than as an `ApproveMode`, because how a
    // person is asked is the caller's business. This matters: the terminal
    // approver reads stdin, and under ACP stdin carries the protocol. An
    // `openbot acp` that fell back to the TTY approver would consume the
    // client's own messages while prompting a human who is not there.
    let (hub, progress) = HubClient::connect_with(hub_url, approver).await?;
    hub.open_session_as(Some(bot.id.as_str())).await?;
    let tools = hub.bind_server(server).await?;

    let model: Arc<dyn Model> = if demo_secret {
        Arc::new(secret_demo_script())
    } else if demo_tools {
        Arc::new(demo_script(None))
    } else {
        config::build(home, model_opts, demo, fallback)?
    };

    let prior = match thread {
        Thread::Own => bots.history(&bot.id, Some(history))?,
        Thread::Group(g) => bots.group_history(g, Some(history))?,
    };
    let agent = Agent::new(
        model,
        hub,
        AgentConfig {
            max_steps,
            system: bot.system_prompt(&openbot_agent::agent::default_system_prompt()),
            // Routines are the runs nobody watches, so they inherit the
            // configured budget without anyone having to remember a flag.
            token_budget: config::token_budget(home, model_opts),
            ..Default::default()
        },
    )
    .with_history(prior);
    // Opt-in: a caller with no stop button passes None and nothing changes.
    let agent = match cancel {
        Some(c) => agent.with_cancel(c),
        None => agent,
    };
    let agent = match redirects {
        Some(r) => agent.with_redirects(r),
        None => agent,
    };
    let started_from = agent.history_len();

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    // Drained unconditionally. A watcher that goes away (a client that
    // disconnects mid-turn) must not wedge the run by leaving events unread,
    // so a send failure is ignored rather than propagated: the turn is the
    // Bot's work, not the viewer's.
    let drain = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            if let Some(w) = &watch {
                let _ = w.send(e);
            }
        }
    });
    let outcome = agent.run(task, tools, progress, ev_tx).await;
    let _ = drain.await;

    let fresh = &outcome.transcript[started_from.min(outcome.transcript.len())..];
    match thread {
        Thread::Own => bots.append(&bot.id, fresh)?,
        // The thread, not the member's own history: re-reading the group must
        // show the handoff, and a member's log must not fill with a
        // conversation it only took one turn of.
        Thread::Group(g) => bots.append_group(g, fresh)?,
    }
    Ok(outcome)
}

/// The line around a match, so a result is readable on its own.
///
/// Bounded on both sides of the hit rather than from the start of the message:
/// a match two thousand characters into a transcript would otherwise show two
/// thousand characters of something else.
fn snippet(text: &str, lower_needle: &str) -> String {
    const AROUND: usize = 60;
    let at = text.to_lowercase().find(lower_needle).unwrap_or(0);
    let chars: Vec<char> = text.chars().collect();
    // Byte offset to char offset, so a multi-byte character cannot split.
    let at = text[..at].chars().count();
    let from = at.saturating_sub(AROUND);
    let to = (at + lower_needle.chars().count() + AROUND).min(chars.len());
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.extend(chars[from..to].iter());
    if to < chars.len() {
        out.push('…');
    }
    // One line: a result list whose rows are different heights is hard to
    // scan.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where attachments land inside the workspace.
///
/// A subdirectory rather than the root, so what a person put there is
/// distinguishable from what the Bot made, and so an attachment cannot
/// silently replace a file the Bot is working on.
const ATTACHMENTS: &str = "attachments";

/// Copy a local file into the workspace and return its workspace-relative
/// path, which is what `fs.read` takes and what the prompt will name.
///
/// Relative, never `file:///`. The path goes into the task, the task becomes
/// the user turn, and the turn is written to `conversation.jsonl` and replayed
/// for every following turn, so an absolute path would write the host's
/// directory layout into a durable transcript to say something `fs.read` did
/// not need.
///
/// Never overwrites. Two files called `notes.md` from two different folders
/// are the ordinary case, and the second silently replacing the first would
/// lose the file just attached. The suffix is a counter and not a timestamp so
/// the result is reproducible.
fn put_attachment(workspace: &std::path::Path, file: &std::path::Path) -> anyhow::Result<String> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", file.display()))?;
    // Read before creating anything: a missing or unreadable source should
    // fail without leaving an empty `attachments/` behind.
    let bytes =
        std::fs::read(file).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;

    let dir = workspace.join(ATTACHMENTS);
    std::fs::create_dir_all(&dir)?;

    let (stem, ext) = match name.rsplit_once('.') {
        // A dotfile is all stem: `.env` must not become `-2.env`.
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    let mut candidate = name.to_owned();
    for n in 2..1000 {
        if !dir.join(&candidate).exists() {
            break;
        }
        candidate = format!("{stem}-{n}{ext}");
    }
    anyhow::ensure!(
        !dir.join(&candidate).exists(),
        "too many files already named {name} in {ATTACHMENTS}/"
    );

    std::fs::write(dir.join(&candidate), bytes)?;
    // Forward slashes: this string is handed to `fs.read`, which resolves it
    // under the workspace root on every platform, and it is read by a person
    // in the transcript.
    Ok(format!("{ATTACHMENTS}/{candidate}"))
}

/// A one-step script that asks the person for a credential.
///
/// The credential path is the one thing a client cannot exercise without a
/// real model deciding it wants a token, which would leave the join between
/// `openbot acp` and a client's prompt untested. As with `--demo-tools`, a
/// scripted turn puts the real question on the wire with no model and no key.
///
/// Its own script rather than a step inside `demo_script`, because
/// `--demo-tools` is what people run to check a deployment and a demo that
/// stopped to demand a credential would be a worse demo.
fn secret_demo_script() -> Scripted {
    Scripted::builder()
        .say_and_call(
            "I need a credential for that.",
            "secret.request",
            serde_json::json!({
                "name": "demo-token",
                "why": "to show what a credential request looks like"
            }),
        )
        .say("Demo complete — that is what a credential request looks like.")
        .build()
}

/// A fixed script that exercises the whole path (write, read back, report) so
/// a deployment can be verified without a model or a key.
fn demo_script(browse: Option<&str>) -> Scripted {
    let b = Scripted::builder()
        .say_and_call(
            "Writing a note into the workspace, so there is something there when you come back.",
            "fs.write",
            serde_json::json!({
                "path": "openbot-demo.md",
                "contents": "# Notes\n\nA Bot wrote this file into the workspace you gave it, read it back, listed what was there, and ran a command.\n\nNo model and no API key were involved, and every step asked you first.\n"
            }),
        )
        .say_and_call(
            "Reading it back, so the write is confirmed rather than assumed.",
            "fs.read",
            serde_json::json!({ "path": "openbot-demo.md" }),
        )
        .say_and_call(
            "Seeing what else is in the workspace.",
            "fs.list",
            serde_json::json!({}),
        )
        // shell.exec is the only tool that streams progress, so the demo has
        // to include it; otherwise the progress rendering path is never
        // exercised by the one command people run to check a deployment.
        .say_and_call(
            "And running a command, the last of the four things a computer gives a Bot.",
            "shell.exec",
            serde_json::json!({ "command": "echo openbot-ok" }),
        )
        ;
    let b = match browse {
        Some(url) => b
            .say_and_call(
                "Opening the page you pointed me at.",
                "browser.open",
                serde_json::json!({ "url": url }),
            )
            .say_and_call(
                "Reading what is on it.",
                "browser.read",
                serde_json::json!({}),
            )
            .say_and_call(
                "Saving a screenshot into the workspace.",
                "browser.screenshot",
                serde_json::json!({ "path": "page.png" }),
            ),
        None => b,
    };
    b.say(
        "That is the loop: a Bot acting on a real computer, asking before each step. Give it a model in Settings and the steps stop being scripted.",
    )
    .build()
}

#[cfg(test)]
mod attachment_tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().expect("a workspace")
    }

    /// An attachment lands where `fs.read` can reach it, under a relative
    /// path.
    ///
    /// The guest is jailed to the workspace, so a file elsewhere on the host
    /// is unreadable however a prompt describes it; it has to be copied in.
    /// The returned path goes in the task and therefore into
    /// `conversation.jsonl`, so it is workspace-relative: an absolute one
    /// would write the host's directory layout into a durable transcript and
    /// replay it into every following turn.
    #[test]
    fn a_file_lands_in_attachments_under_a_relative_path() {
        let w = ws();
        let src = w.path().join("notes.md");
        std::fs::write(&src, b"hello").unwrap();

        let at = put_attachment(w.path(), &src).expect("put");
        assert_eq!(at, "attachments/notes.md");
        assert!(
            !at.contains('\\'),
            "a backslash reached the transcript: {at}"
        );
        assert!(
            !std::path::Path::new(&at).is_absolute(),
            "the host's layout is in the transcript: {at}"
        );
        assert_eq!(
            std::fs::read(w.path().join("attachments").join("notes.md")).unwrap(),
            b"hello"
        );
    }

    /// Never overwrite. Two files called `notes.md` from two folders is the
    /// ordinary case, and the second replacing the first would silently lose
    /// the file just attached.
    #[test]
    fn a_second_file_of_the_same_name_does_not_replace_the_first() {
        let w = ws();
        let a = w.path().join("a");
        let b = w.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("notes.md"), b"first").unwrap();
        std::fs::write(b.join("notes.md"), b"second").unwrap();

        assert_eq!(
            put_attachment(w.path(), &a.join("notes.md")).unwrap(),
            "attachments/notes.md"
        );
        assert_eq!(
            put_attachment(w.path(), &b.join("notes.md")).unwrap(),
            "attachments/notes-2.md"
        );
        // The first is still what it was.
        assert_eq!(
            std::fs::read(w.path().join("attachments").join("notes.md")).unwrap(),
            b"first"
        );
    }

    /// A dotfile is all name and no extension: `.env` must not become
    /// `-2.env`, which is a different file to anything reading it.
    #[test]
    fn a_dotfile_keeps_its_name() {
        let w = ws();
        let src = w.path().join(".env");
        std::fs::write(&src, b"K=v").unwrap();
        assert_eq!(put_attachment(w.path(), &src).unwrap(), "attachments/.env");
        assert_eq!(
            put_attachment(w.path(), &src).unwrap(),
            "attachments/.env-2"
        );
    }

    /// A source that cannot be read fails, and leaves nothing behind.
    ///
    /// Read before create, so a mistyped path does not leave an empty
    /// `attachments/` in a workspace a Bot then lists.
    #[test]
    fn an_unreadable_source_creates_nothing() {
        let w = ws();
        let err = put_attachment(w.path(), &w.path().join("not-here.md")).unwrap_err();
        assert!(
            err.to_string().contains("not-here.md"),
            "the error does not say which file: {err}"
        );
        assert!(
            !w.path().join("attachments").exists(),
            "a failed attach left a directory in the workspace"
        );
    }
}
