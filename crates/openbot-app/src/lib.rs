//! OPENBOT, the desktop client for openbot (SPEC §9).
//!
//! A thin Tauri shell over the engine: the window calls commands, the
//! commands drive `openbot acp`, and what the agent says is emitted to the page
//! as it arrives. Nothing here knows the wire protocol; the engine owns all
//! of that.
//!
//! This is a library with `main.rs` as a minimal entry point, so the command
//! layer can be built on `tauri::test`'s mock runtime and driven without a
//! window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ContentBlock, PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionId, SessionUpdate, StopReason, ToolCallStatus,
};
use openbot_desktop::engine::{Config, Engine, EngineHandle, PendingPermission};
use openbot_desktop::{hub, policy, roster, secrets, settings, skills, viewer};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::Mutex;

/// How long the chat loop sleeps between drains of the engine's streams.
/// Long enough not to busy-spin, short enough that a word, a turn end or an
/// approval ask is noticed promptly.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// What the binary was told to be.
///
/// Set only from the command line; the window cannot change it. A scripted
/// demo therefore cannot be mistaken for a misconfigured agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// A real model, configured the usual way.
    #[default]
    Live,
    /// `--demo`: one scripted reply. No tools, no key.
    Reply,
    /// `--demo-tools`: the scripted tool run (write, read back, list, shell)
    /// so the approval dialog can be exercised end to end without a model.
    Tools,
    /// `--demo-secret`: ask once for a credential and stop, so the credential
    /// prompt can be exercised without a model.
    ///
    /// A separate mode rather than a step inside `Tools`: `--demo-tools` is
    /// used to check a deployment, and a demo that stops to demand a
    /// credential would not serve that purpose.
    Secret,
}

impl Mode {
    /// Read the mode off a command line.
    ///
    /// Precedence is `--demo` over `--demo-secret` over `--demo-tools`,
    /// ordered by how little each one does. When two demos are requested the
    /// smaller claim wins: `--demo` makes no tool call at all,
    /// `--demo-secret` makes one that only asks a question, and
    /// `--demo-tools` writes a file and runs a shell command.
    #[must_use]
    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mode = Self::Live;
        for arg in args {
            match arg.as_ref() {
                "--demo" => return Self::Reply,
                // Not `return`: a later `--demo` still wins. Taking the first
                // flag seen would make precedence depend on argument order.
                "--demo-secret" => mode = Self::Secret,
                "--demo-tools" if mode != Self::Secret => mode = Self::Tools,
                _ => {}
            }
        }
        mode
    }

    /// Apply the mode to a fresh engine config.
    fn apply(self, cfg: &mut Config) {
        cfg.demo = self == Self::Reply;
        cfg.demo_tools = self == Self::Tools;
        cfg.demo_secret = self == Self::Secret;
    }
}

/// Updates taken off the engine's stream but not yet handed to their session.
///
/// The engine has one stream for every session, so whoever drains it sees
/// other conversations' updates too. If a drainer kept only what matched its
/// own session and discarded the rest, one open conversation would silently
/// eat another's transcript. Nothing is discarded: everything drained is kept
/// here, and each caller takes only its own.
#[derive(Debug, Default)]
struct Inbox(HashMap<String, Vec<SessionUpdate>>);

impl Inbox {
    /// Keep updates for later, in the order they arrived.
    fn absorb(&mut self, updates: impl IntoIterator<Item = (SessionId, SessionUpdate)>) {
        for (session, update) in updates {
            self.0
                .entry(session.0.to_string())
                .or_default()
                .push(update);
        }
    }

    /// Everything waiting for one session, oldest first, removed.
    fn take(&mut self, session: &str) -> Vec<SessionUpdate> {
        self.0.remove(session).unwrap_or_default()
    }

    /// Forget every backlog. Updates held for sessions that can no longer be
    /// addressed are a slow leak.
    fn clear(&mut self) {
        self.0.clear();
    }
}

/// Shell state: an engine when connected, nothing when not, plus the
/// approvals the window is showing that have not yet been answered.
pub struct AppState {
    engine: Mutex<Option<Engine>>,
    pending: Mutex<HashMap<String, PendingPermission>>,
    /// Where the binary and the Bots are, remembered at `connect` so the
    /// roster can be read without the window passing them back every time.
    /// `None` until connected, the same condition as `engine`.
    where_: Mutex<Option<Where>>,
    /// The Agent Computer, while the panel is open. Dropping it kills the
    /// viewer, so closing the panel closes the port instead of leaving
    /// something that can drive a signed-in computer listening.
    computer: Mutex<Option<viewer::Viewer>>,
    /// A computer this window started, when there was none to connect to.
    /// `None` when one was already running: that one belongs to whoever
    /// started it, and disconnecting must not take it away from them.
    /// Dropping this kills the child, which is how it stops.
    started: Mutex<Option<hub::Started>>,
    /// Updates drained from the engine's one stream, kept per session so no
    /// conversation can eat another's.
    inbox: Mutex<Inbox>,
    mode: Mode,
}

/// The two paths every roster read needs.
#[derive(Clone)]
struct Where {
    openbot: PathBuf,
    home: PathBuf,
    /// The hub the viewer attaches to, which is the same one the agent uses.
    hub: String,
}

impl AppState {
    /// A disconnected shell in the given mode.
    #[must_use]
    pub fn new(mode: Mode) -> Self {
        Self {
            engine: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            where_: Mutex::new(None),
            computer: Mutex::new(None),
            started: Mutex::new(None),
            inbox: Mutex::new(Inbox::default()),
            mode,
        }
    }
}

/// Where a line in the transcript came from.
///
/// A type rather than a string, because the page styles on it. Every variant
/// needs a `.msg.<name>` rule in `styles.css`; one without renders as
/// undecorated body text, which is neither a crash nor a visible error, just
/// a tool call that no longer looks like one. The total match in `as_str` and
/// the `every_message_kind_is_styled` test together make a new variant fail
/// the build until it has a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The Bot speaking.
    Agent,
    /// The person.
    User,
    /// The Bot's reasoning, when a model streams it separately.
    Thought,
    /// A tool call, with its arguments.
    Tool,
    /// A stage a long call reported while it ran.
    Progress,
    /// What a tool call returned.
    Result,
}

impl Kind {
    /// Every variant, for the test that checks each one is styled. Listed
    /// rather than derived; the total match in `as_str` keeps it complete,
    /// since adding a variant fails to compile until it is handled there.
    pub const ALL: [Kind; 6] = [
        Kind::Agent,
        Kind::User,
        Kind::Thought,
        Kind::Tool,
        Kind::Progress,
        Kind::Result,
    ];

    /// The class the page puts on the element. Intentionally a total match.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::User => "user",
            Kind::Thought => "thought",
            Kind::Tool => "tool",
            Kind::Progress => "progress",
            Kind::Result => "result",
        }
    }

    fn as_wire<S: serde::Serializer>(k: &Kind, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(k.as_str())
    }
}

/// One renderable piece of a turn, emitted the moment it arrives.
///
/// Emitted rather than returned: the engine streams so the window can show
/// words as they are said, and buffering a whole turn here would discard
/// that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Chunk {
    /// Which conversation this belongs to; the window may show one and hold
    /// others.
    pub session: String,
    /// Where the text came from. The page styles on this, so it is a closed
    /// vocabulary enforced by the type rather than by convention.
    #[serde(serialize_with = "Kind::as_wire")]
    pub kind: Kind,
    pub text: String,
    /// A tool call's arguments, as data rather than as the display string.
    ///
    /// `text` is truncated to a readable length, which leaves the JSON in it
    /// unparseable: an `fs.write` carrying a real file lost its summary and
    /// printed raw JSON, while a short `fs.read` beside it read as a sentence.
    /// The page summarises from this instead, so the length of an argument no
    /// longer decides whether a step is legible.
    ///
    /// `None` for every kind but a tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Turn {
    /// The protocol's own word, for logs and tests.
    pub stop: String,
    /// The same thing in words a person reads.
    pub note: String,
}

/// An approval the window is asking the person about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionAsk {
    /// The id the window echoes when the person clicks; the key under which
    /// the shell parked the `PendingPermission` while it waits.
    pub id: String,
    pub session: String,
    /// The tool title, e.g. `fs.write`.
    pub tool: String,
    /// What the action would do, as named fields rather than a JSON blob.
    ///
    /// The docs ask a person to "review the target, scope, and values", and a
    /// pretty-printed object buries the target in the payload: for an
    /// `fs.write` the filename sits above however many lines of file contents.
    /// Fields put the short values first, which are the target and the scope.
    pub fields: Vec<AskField>,
    pub options: Vec<AskOption>,
    /// Set when the Bot is asking for a credential rather than for permission
    /// to act. The window shows an input instead of allow/deny buttons, and
    /// answers with `supply_secret`.
    ///
    /// `None` for every ordinary approval, so a page that does not know about
    /// this renders exactly what it did before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretPrompt>,
}

/// What to put in front of a person being asked for a credential.
///
/// There is no field here for the value. It travels from the input to
/// `supply_secret` and into the reply; nothing on the way keeps a copy. This
/// mirrors `SecretStoredResult` on the hub side, for the same reason: a
/// struct that cannot hold a credential cannot leak one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretPrompt {
    /// What it will be stored as, e.g. `linear-token`.
    pub name: String,
    /// Why the Bot needs it, in its own words.
    pub why: String,
}

/// One argument of a proposed action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AskField {
    /// The agent's own name for it (`path`, `command`, `url`), never a label
    /// invented by this shell: a relabelled argument cannot be checked
    /// against what actually ran.
    pub name: String,
    pub value: String,
    /// Long enough to need its own block rather than a line beside its name.
    pub long: bool,
}

/// Past this, a value is a block rather than a line.
const FIELD_INLINE_LIMIT: usize = 72;

/// Break a proposed action's arguments into fields a person can scan.
///
/// Nothing is dropped and nothing is shortened. This is the surface where a
/// person decides whether an action may run, so every argument the agent sent
/// appears in full. The only decision made here is what gets a line and what
/// gets a block.
///
/// Short scalars come first because they are the target and the scope (the
/// file, the command, the URL); long values are the payload, which is checked
/// second.
#[must_use]
pub fn fields_of(input: Option<&serde_json::Value>) -> Vec<AskField> {
    let Some(input) = input else {
        return Vec::new();
    };
    let mut fields: Vec<AskField> = match input {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(name, value)| AskField::new(name.clone(), value))
            .collect(),
        // A tool whose arguments are not an object still has to be reviewable.
        // No name, because inventing one would label it something the agent
        // never called it.
        other => vec![AskField::new(String::new(), other)],
    };
    // Stable order: short before long, alphabetical within each. Fields that
    // move between two calls to the same tool make the dialog harder to
    // review.
    fields.sort_by(|a, b| a.long.cmp(&b.long).then_with(|| a.name.cmp(&b.name)));
    fields
}

impl AskField {
    fn new(name: String, value: &serde_json::Value) -> Self {
        // Strings unquoted (a path reads as `notes.md`, not `"notes.md"`),
        // everything else as compact JSON.
        let value = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let long = value.chars().count() > FIELD_INLINE_LIMIT || value.contains('\n');
        Self { name, value, long }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AskOption {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Whether this button refuses something, decided here rather than in the
    /// page. See [`refuses`].
    pub danger: bool,
}

/// Does this option refuse the action?
///
/// Decided in Rust, and fail-closed. `PermissionOptionKind` is
/// `#[non_exhaustive]`, so ACP may add a kind that is neither `allow_*` nor
/// `reject_*`. Classifying in the page with a prefix match would style such a
/// button as the permitted choice, in the one dialog where allow and deny
/// must not look alike.
///
/// A wildcard rather than a total match, because there is no total form to
/// write for a `#[non_exhaustive]` enum (`prompt_text` is in the same
/// position with `ContentBlock`). The fallback is the cautious one: an option
/// this build cannot classify is presented as a refusal, never as an allow.
/// Erring that way makes the person read the card again; erring the other way
/// makes them click through it.
#[must_use]
pub fn refuses(kind: PermissionOptionKind) -> bool {
    match kind {
        PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways => false,
        PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways => true,
        _ => true,
    }
}

/// Model settings collected on the connect panel.
///
/// The window is the only surface that exists before a connection, so it is the
/// only place these can be set by somebody who has just installed this. Before
/// this, a fresh install could not reach a model at all without a terminal:
/// `Settings` lives inside the workspace, which is hidden until the connect
/// that needs a model succeeds.
///
/// `api_key` is not written anywhere. It goes into the agent process's
/// environment under `api_key_env`, which is where the runtime reads it from,
/// and `config.toml` keeps the name and never the value.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelInput {
    /// The model id, e.g. `grok-4-5`. Empty leaves the stored settings alone.
    #[serde(default)]
    id: String,
    #[serde(default)]
    dialect: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

/// Connect to a `openbot acp` agent. The engine verifies the handshake before
/// the window is told it worked.
///
/// `demo` starts the connection in the scripted tool demo instead of against a
/// model. The window offers it when a connect failed for want of a model or a
/// key, because the demo needs neither and is the only way to watch a Bot use
/// real tools before being asked to configure anything. `Option` so the
/// existing callers that send four arguments keep working.
#[tauri::command]
async fn connect(
    state: State<'_, AppState>,
    openbot: String,
    home: String,
    hub: String,
    demo: Option<bool>,
    model: Option<ModelInput>,
) -> Result<Connected, String> {
    let mut engine = state.engine.lock().await;
    if engine.is_some() {
        return Err("already connected".into());
    }
    let here = Where {
        openbot: PathBuf::from(openbot),
        home: PathBuf::from(home),
        hub,
    };
    let mut cfg = Config::new(here.openbot.clone(), here.home.clone(), here.hub.clone());
    state.mode.apply(&mut cfg);
    // A demo asked for by the window, over whatever the process was launched
    // with. Reusing `Mode` rather than setting the flag directly keeps one
    // definition of what a demo is; setting `cfg.demo_tools` here would be a
    // second one, free to drift.
    if demo.unwrap_or(false) {
        Mode::Tools.apply(&mut cfg);
    }

    // Settings the person just typed, before anything tries to use them. The
    // id and the key are handled separately on purpose: somebody whose model is
    // already configured and who only needs to supply a key should not have to
    // retype the model to do it.
    if let Some(model) = &model {
        let id = model.id.trim();
        if !id.is_empty() {
            settings::save_model(
                &here.openbot,
                &here.home,
                id,
                model.dialect.as_deref(),
                model.base_url.as_deref(),
                model.api_key_env.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;
        }
        if let Some(key) = model
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            let var = model
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("XAI_API_KEY");
            cfg.api_key = Some((var.to_owned(), key.to_owned()));
        }
    }
    let connected = Engine::connect(cfg).await.map_err(|e| e.to_string())?;
    *engine = Some(connected);
    // Ask whether there is a computer, rather than letting "connected" mean
    // "the agent started". `openbot acp` reaches the hub lazily, per turn, so
    // the handshake above succeeds against a hub that is wrong or down and the
    // failure would otherwise surface only on the first sent message.
    //
    // Not fatal: the roster reads without a hub, and a mistyped URL has to be
    // fixable from inside the window. The reported state just has to be
    // accurate.
    let reach = hub::reach(&here.openbot, &here.hub)
        .await
        .unwrap_or_else(|e| hub::Reach::Unreachable(e.to_string()));

    // Nothing there: start one. A window whose answer to "no computer" is
    // "open a terminal" is not an application, and the person who installed it
    // may not have a terminal open or know what to type in it.
    //
    // Only when this window started it does it own it. A hub that was already
    // serving belongs to whoever started it, and `started` stays `None` so
    // disconnecting leaves it running.
    let reach = match reach {
        serving @ hub::Reach::Serving(_) => serving,
        hub::Reach::Unreachable(first) => {
            match hub::start(&here.openbot, &here.home, &here.hub, STARTUP_PATIENCE).await {
                Ok(child) => {
                    *state.started.lock().await = Some(child);
                    hub::reach(&here.openbot, &here.hub)
                        .await
                        .unwrap_or_else(|e| hub::Reach::Unreachable(e.to_string()))
                }
                // Report why starting one failed, not the original refusal:
                // "connection refused" is what a person expects to see before
                // anything is running, and it says nothing about why the
                // attempt to run it did not work. The refusal is kept as the
                // tail so a wrong hub URL is still visible.
                Err(e) => hub::Reach::Unreachable(format!("{e} (before that: {first})")),
            }
        }
    };

    *state.where_.lock().await = Some(here);
    Ok(match reach {
        hub::Reach::Serving(tools) => Connected {
            computer: true,
            tools,
            why: None,
        },
        hub::Reach::Unreachable(why) => Connected {
            computer: false,
            tools: 0,
            why: Some(why),
        },
    })
}

/// How long the window waits for a computer it started to answer.
///
/// A first run creates the workspace and starts a browser, which is slower
/// than every run after it. Long enough for a cold start on a slow disk, short
/// enough that a computer which will never answer is reported rather than left
/// spinning.
const STARTUP_PATIENCE: Duration = Duration::from_secs(30);

/// What connecting found: an agent, and whether there is a computer behind it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Connected {
    /// Whether the hub answered. `false` means the agent is running and has
    /// nothing to run work on.
    pub computer: bool,
    /// How many tools it serves. A count is stronger evidence that the
    /// computer is present than a bare "ok".
    pub tools: usize,
    /// Why not, in the binary's own words.
    pub why: Option<String>,
}

/// Drop the engine: the adapter is killed, the agent task aborted. Anything
/// the window was still asking about is answered `Cancelled` first, so a turn
/// left hanging by a closed window does not stay pending on the other end.
#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut pending = state.pending.lock().await;
    for (_, mut ask) in pending.drain() {
        // Tearing down: a request that had already settled is no different
        // from one refused here. Both end refused, and there is nobody left to
        // tell either way.
        let _settled = ask.answer(RequestPermissionOutcome::Cancelled);
    }
    drop(pending);
    *state.engine.lock().await = None;
    *state.where_.lock().await = None;
    // The viewer does not outlive the connection. A disconnected window with
    // a live port behind it is a port nobody is watching.
    *state.computer.lock().await = None;
    // And a computer this window started. Dropping it kills the child. One it
    // did not start is `None` here and is left running, because disconnecting
    // a window is not a reason to stop a computer somebody else is using.
    *state.started.lock().await = None;
    // And the backlog: updates held for sessions that can no longer be
    // addressed.
    state.inbox.lock().await.clear();
    Ok(())
}

/// Whether an engine is currently connected. The window asks on load so a
/// reload lands on the right panel instead of offering to connect twice.
#[tauri::command]
async fn connected(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.engine.lock().await.is_some())
}

/// Every Bot, for the sidebar.
///
/// `hidden` is the docs' "Show hidden chats". It travels to the binary rather
/// than filtering a list the client already has: they are two distinct
/// requests, not one list with a checkbox over it.
///
/// A failure here is an error and never an empty list. An empty sidebar looks
/// exactly like having no Bots, which sends a person looking for their work
/// instead of for the message.
#[tauri::command]
async fn roster(state: State<'_, AppState>, hidden: bool) -> Result<Vec<roster::Entry>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::list(&here.openbot, &here.home, hidden)
        .await
        .map_err(|e| e.to_string())
}

/// What the window gets back when it opens a Bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opened {
    /// The session to address for the rest of the conversation.
    pub session: String,
    /// The Bot it landed on, echoed back so the header is never a guess.
    pub name: String,
    /// What this Bot already remembers, oldest first.
    ///
    /// Returned, not emitted. A live turn streams because the words are being
    /// said now; a replay exists in full before the window asks for it. There
    /// is also an ordering constraint: the page filters `chunk` events by
    /// session id and does not learn the id until this command returns, so
    /// replayed chunks emitted as events would arrive while the page still
    /// compares against `null` and be dropped. Returning the history removes
    /// the ordering question rather than answering it.
    pub history: Vec<Chunk>,
}

/// Open a Bot from the sidebar, and replay what it already remembers.
///
/// One round trip rather than two: `session/load` resolves the Bot by name,
/// creates it if this is the first time, binds the session id the client
/// chose, and replays the conversation. Opening an existing teammate and
/// starting a new one are therefore the same act.
///
/// The transcript is returned in `Opened::history`.
#[tauri::command]
async fn open_bot(state: State<'_, AppState>, name: String) -> Result<Opened, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("a Bot needs a name".into());
    }
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    let handle = engine_handle(&state).await?;

    // The client names its own session: the id must exist before the load
    // that would otherwise return one, and openbot resolves the conversation
    // from the Bot rather than from this string.
    let session = SessionId::new(format!("openbot-{}", uuid::Uuid::new_v4()));
    handle
        .load_session_for(
            &session,
            Some(openbot_desktop::engine::Who::Bot(name.clone())),
            here.home.to_string_lossy().to_string(),
        )
        .await
        .map_err(|e| e.to_string())?;

    // Drain the replay. The updates are already waiting: the agent sends them
    // before it answers `session/load`, so a client can treat the response as
    // "the transcript is complete".
    let sid = session.0.to_string();
    let history = collect(&state, &session)
        .await?
        .into_iter()
        .filter_map(|u| render(&sid, u))
        .collect();

    Ok(Opened {
        session: sid,
        name,
        history,
    })
}

/// Open a group and replay its thread, the way `open_bot` opens a teammate.
///
/// A group session names the group; which member answers is decided per
/// message by who was `@mentioned`, so the composer is live here, unlike the
/// read-only `group_log`.
#[tauri::command]
async fn open_group(state: State<'_, AppState>, name: String) -> Result<Opened, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("a group needs a name".into());
    }
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    let handle = engine_handle(&state).await?;
    let session = SessionId::new(format!("openbot-{}", uuid::Uuid::new_v4()));
    handle
        .load_session_for(
            &session,
            Some(openbot_desktop::engine::Who::Group(name.clone())),
            here.home.to_string_lossy().to_string(),
        )
        .await
        .map_err(|e| e.to_string())?;

    let sid = session.0.to_string();
    let history = collect(&state, &session)
        .await?
        .into_iter()
        .filter_map(|u| render(&sid, u))
        .collect();
    Ok(Opened {
        session: sid,
        name,
        history,
    })
}

/// Find a phrase in what the Bots and groups have said.
///
/// The palette switches between teammates; this finds a conversation without
/// knowing which teammate had it.
#[tauri::command]
async fn search(state: State<'_, AppState>, query: String) -> Result<Vec<roster::Hit>, String> {
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::search(&here.openbot, &here.home, &query)
        .await
        .map_err(|e| e.to_string())
}

/// Every group: several Bots on one thread.
#[tauri::command]
async fn groups(state: State<'_, AppState>) -> Result<Vec<roster::Group>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::groups(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// One group's thread, rendered the way a conversation is.
///
/// A reader, not the whole surface: it shows what groups are for (the
/// handoffs, in one conversation) without opening a session. `open_group` is
/// the interactive path; this is the read-only one, used where a session is
/// not wanted.
#[tauri::command]
async fn group_log(state: State<'_, AppState>, name: String) -> Result<Vec<Chunk>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    let raw = roster::group_log(&here.openbot, &here.home, &name)
        .await
        .map_err(|e| e.to_string())?;
    let messages: Vec<ThreadMessage> =
        serde_json::from_value(raw).map_err(|e| format!("the thread could not be read: {e}"))?;
    Ok(thread_chunks(&name, &messages))
}

/// One message in a stored thread, as the binary prints it.
///
/// Declared here rather than shared with `openbot-agent`, so this pins the JSON
/// the client actually parses. A shared type would agree with itself under
/// any rename.
#[derive(Debug, Deserialize)]
struct ThreadMessage {
    role: String,
    #[serde(default)]
    content: Vec<ThreadContent>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ThreadContent {
    Text {
        text: String,
    },
    /// Tool calls and results, which the group view does not render: a group
    /// thread is read for the handoffs between members, and a tool transcript
    /// buries them. The payload is intentionally dropped; this arm exists so
    /// an unfamiliar entry cannot fail the whole thread.
    Other(serde::de::IgnoredAny),
}

/// A group thread as renderable chunks, oldest first.
fn thread_chunks(session: &str, messages: &[ThreadMessage]) -> Vec<Chunk> {
    let mut out = Vec::new();
    for message in messages {
        for piece in &message.content {
            let ThreadContent::Text { text } = piece else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            out.push(Chunk {
                session: session.to_owned(),
                // Everything a Bot said, including a handoff to another Bot,
                // is the group speaking. Only the person is `user`.
                kind: if message.role == "user" {
                    Kind::User
                } else {
                    Kind::Agent
                },
                text: text.clone(),
                // Replayed prose from a group thread, not a tool call.
                args: None,
            });
        }
    }
    out
}

/// Which apps are connected.
#[tauri::command]
async fn connectors(state: State<'_, AppState>) -> Result<Vec<settings::Connector>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    settings::connectors(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// What runs on a schedule, and whether it is paused.
///
/// A routine runs unattended, so the window has to show that it exists and
/// whether it is paused; otherwise there is no way to notice it stopped.
#[tauri::command]
async fn routines(state: State<'_, AppState>) -> Result<Vec<settings::Routine>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    settings::routines(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// Edit a Bot's profile: what it is called, what it does, what it is for.
///
/// The docs' "Edit a Bot". Every field is optional and absent means
/// unchanged, so the window can send only what was edited; a form that posts
/// all three would overwrite a description with whatever was on screen when
/// it loaded.
///
/// A rename keeps the Bot's id and therefore its whole conversation. The
/// window addresses Bots by name, so it must refresh the roster afterwards
/// via `roster`.
#[tauri::command]
async fn bot_describe(
    state: State<'_, AppState>,
    bot: String,
    rename: Option<String>,
    title: Option<String>,
    description: Option<String>,
) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::describe(
        &here.openbot,
        &here.home,
        &bot,
        rename.as_deref(),
        title.as_deref(),
        description.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Copy a Bot's brief as the start of another.
///
/// The docs' "Duplicate a Bot". The copy intentionally does not carry the
/// conversation: a Bot cloned to cover a second region must not answer with
/// facts about the first. The window states this explicitly.
#[tauri::command]
async fn bot_duplicate(
    state: State<'_, AppState>,
    bot: String,
    new_name: String,
) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::duplicate(&here.openbot, &here.home, &bot, &new_name)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a Bot, its conversation, and its routines.
///
/// The window must confirm with the person first. This is irreversible and
/// takes more than the Bot: the conversation and any routines go with it, and
/// it is removed from every group that holds it. Nothing here can tell
/// whether a person was shown that, so the page does the asking and this
/// command does not second-guess it.
#[tauri::command]
async fn bot_delete(state: State<'_, AppState>, bot: String) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::delete(&here.openbot, &here.home, &bot)
        .await
        .map_err(|e| e.to_string())
}

/// Take a Bot out of the sidebar, or put it back.
///
/// The docs' "Hide from sidebar". Hiding is neither archiving nor pausing:
/// the Bot keeps its conversation and keeps running whatever it has
/// scheduled, which SPEC §8 calls out as a footgun and `openbot bot hide` warns
/// about. The window must warn too (it reads the Bot's routines before it
/// asks), or hiding here is the same act with the safeguard removed.
#[tauri::command]
async fn bot_hide(state: State<'_, AppState>, bot: String, hidden: bool) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    roster::set_hidden(&here.openbot, &here.home, &bot, hidden)
        .await
        .map_err(|e| e.to_string())
}

/// Stop a routine firing, or start it again.
///
/// The docs' "pausable", and the other half of the warning the hide dialog
/// gives: naming a routine that keeps running is of little use if the window
/// has no way to stop it. Pausing keeps the definition and the history, so
/// unlike deleting a Bot this needs no confirmation; the act is reversible
/// with the same button.
#[tauri::command]
async fn routine_pause(
    state: State<'_, AppState>,
    bot: String,
    routine: String,
    paused: bool,
) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    settings::set_paused(&here.openbot, &here.home, &bot, &routine, paused)
        .await
        .map_err(|e| e.to_string())
}

/// The saved procedures the composer's `/` offers.
///
/// Both halves travel: what loaded, and what did not. A skill that fails to
/// parse is still on disk and still absent from every Bot's reasoning, so
/// the window must be able to say so rather than quietly offer a shorter list
/// than the person wrote.
#[tauri::command]
async fn skills(state: State<'_, AppState>) -> Result<skills::Catalog, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    skills::catalog(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// Every permission rule this home configures.
///
/// The docs' auto-review list: what always stops for a person, what may
/// proceed, and (fixed, not configurable from here) that stopping wins when
/// both match.
#[tauri::command]
async fn policy_list(state: State<'_, AppState>) -> Result<Vec<policy::Rule>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    policy::list(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// Add a rule.
///
/// This does not change a hub that is already running. Rules are read when
/// it boots, and the window says so rather than letting a person believe a
/// control is live when it is not.
#[tauri::command]
async fn policy_add(state: State<'_, AppState>, rule: policy::Rule) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    policy::add(&here.openbot, &here.home, &rule)
        .await
        .map_err(|e| e.to_string())
}

/// Remove the rule at this position in [`policy_list`], counting from one.
#[tauri::command]
async fn policy_remove(state: State<'_, AppState>, number: usize) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    policy::remove(&here.openbot, &here.home, number)
        .await
        .map_err(|e| e.to_string())
}

/// Every credential the hub holds: names and fingerprints, never values.
///
/// The value is intentionally absent from the listing. A window that could
/// show a stored credential would undo the reason the store exists.
#[tauri::command]
async fn secret_list(state: State<'_, AppState>) -> Result<Vec<secrets::Entry>, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    secrets::list(&here.openbot, &here.home)
        .await
        .map_err(|e| e.to_string())
}

/// Store a credential supplied in the window.
///
/// The value reaches the store down a pipe and is not kept, logged, emitted
/// as an event, or echoed in the error if this fails. It never enters a
/// transcript and is never sent to a model; the hub substitutes it into an
/// outgoing header at the moment of the call and nowhere else.
#[tauri::command]
async fn secret_set(state: State<'_, AppState>, name: String, value: String) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    secrets::set(&here.openbot, &here.home, &name, &value)
        .await
        .map_err(|e| e.to_string())
}

/// Forget a credential.
#[tauri::command]
async fn secret_remove(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    secrets::remove(&here.openbot, &here.home, &name)
        .await
        .map_err(|e| e.to_string())
}

/// Open the Agent Computer, and return where to point the panel.
///
/// The docs' "Agent Computer": watch the shared desktop, and take control for
/// a password, a 2FA code or a CAPTCHA rather than typing any of those into
/// chat. The address carries a one-time key, without which the viewer refuses
/// everything. Loopback is not a boundary in a browser: any page can POST to
/// `127.0.0.1`, and CORS stops it reading the reply, not performing the
/// action.
///
/// Opening twice returns the same viewer rather than starting a second one.
#[tauri::command]
async fn open_computer(state: State<'_, AppState>) -> Result<String, String> {
    let Some(here) = state.where_.lock().await.clone() else {
        return Err("not connected".into());
    };
    let mut computer = state.computer.lock().await;
    if let Some(open) = computer.as_mut() {
        if open.alive() {
            return Ok(open.url().to_owned());
        }
        // The viewer died. Its last address would leave a panel pointed at
        // nothing, indistinguishable from a frozen computer.
        *computer = None;
    }
    let view = viewer::open(&here.openbot, &here.hub)
        .await
        .map_err(|e| e.to_string())?;
    let url = view.url().to_owned();
    *computer = Some(view);
    Ok(url)
}

/// Is the computer still being served?
///
/// The panel is an iframe onto another process. When that process dies the
/// frame keeps showing whatever it last painted: a still picture of a
/// computer that is no longer there, indistinguishable from one sitting idle.
/// The page polls this so it can say so.
#[tauri::command]
async fn computer_alive(state: State<'_, AppState>) -> Result<bool, String> {
    let mut computer = state.computer.lock().await;
    Ok(computer.as_mut().is_some_and(viewer::Viewer::alive))
}

/// Close the panel, and stop the viewer with it.
#[tauri::command]
async fn close_computer(state: State<'_, AppState>) -> Result<(), String> {
    *state.computer.lock().await = None;
    Ok(())
}

/// Where to find the runtime, as the connect panel should offer it.
///
/// An installed OPENBOT ships `openbot` beside its own executable (Tauri's
/// `externalBin`), so the installer is not a client with nothing to drive.
/// When present it is the right default: it is the build tested against this
/// one, and it needs no PATH.
///
/// The bare name is the fallback for a developer running from source or
/// someone who installed the runtime separately.
#[tauri::command]
fn default_openbot() -> String {
    sidecar().map_or_else(|| "openbot".to_owned(), |p| p.display().to_string())
}

/// The runtime shipped beside this executable, if one was.
fn sidecar() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.parent()?.join(if cfg!(windows) {
        "openbot.exe"
    } else {
        "openbot"
    });
    beside.is_file().then_some(beside)
}

/// Where a person's Bots should live, as a path that exists on this machine.
///
/// The field is filled with a real, expanded path, the way the runtime field
/// is filled by [`default_openbot`]. A literal `~/.openbot` must not be used:
/// nothing expands a tilde on the way to a subprocess, so openbot would take
/// it literally and create a directory called `~` beside wherever OPENBOT was
/// launched from.
///
/// The value comes from [`openbot_proto::default_home`], which the runtime
/// uses for its own default too. One definition on purpose: this window
/// offered `~/.openbot` while the runtime defaulted to `./openbot-data`, so
/// connecting a window to a computer started in a terminal read each from a
/// different home. Nothing errored; the roster was just empty. Held by
/// `the_window_and_the_binary_default_to_the_same_home`.
#[tauri::command]
fn default_home() -> String {
    connect_panel_home()
}

/// The home the connect panel offers, reachable from a test.
///
/// Separate from the command above because `#[tauri::command]` generates
/// same-named macros that collide when the function it decorates is public.
#[must_use]
pub fn connect_panel_home() -> String {
    openbot_proto::default_home().display().to_string()
}

/// A native folder picker; the window has no file access of its own.
#[tauri::command]
async fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Choose the `openbot` binary.
///
/// This is the field more likely to be wrong: a home that does not exist is
/// created, and a binary that does not exist is the failure a new person
/// cannot diagnose. No extension filter: `openbot` has none on Linux and macOS,
/// and a filter that hid the file on two of three platforms would be worse
/// than none.
#[tauri::command]
async fn pick_binary() -> Option<String> {
    rfd::FileDialog::new()
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Open a session on a directory. Returns the session id the window will
/// address for the rest of the conversation.
#[tauri::command]
async fn new_session(state: State<'_, AppState>, cwd: String) -> Result<String, String> {
    // Take the handle, drop the lock, then await. Holding the lock across
    // this round trip deadlocks the window: opening a session during a turn
    // waits on a reply that cannot arrive until the turn ends, and the turn
    // cannot end because `prompt` needs this same lock to hand over the
    // approval it is blocked on.
    let handle = engine_handle(&state).await?;
    let session = handle.new_session(cwd).await.map_err(|e| e.to_string())?;
    Ok(session.0.to_string())
}

/// A handle to the connected engine, with the lock released before returning.
async fn engine_handle(state: &State<'_, AppState>) -> Result<EngineHandle, String> {
    let engine = state.engine.lock().await;
    Ok(engine.as_ref().ok_or("not connected")?.handle())
}

/// Run one prompt turn, emitting what the agent says as it says it.
///
/// Every word leaves as a `chunk` event the moment the engine hands it over;
/// the return value is only how the turn ended. The turn may pause on an
/// approval: each `session/request_permission` is parked in the state and
/// forwarded as a `permission-request` event, and the turn does not move
/// until the window answers through [`answer_permission`] (or the agent gives
/// up waiting).
#[tauri::command]
async fn prompt<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    session: String,
    text: String,
    // Workspace-relative paths from `attach_file`, in the order they were
    // attached. `None` for an ordinary message, so a page that never attaches
    // anything sends what it always sent.
    attached: Option<Vec<String>>,
) -> Result<Turn, String> {
    let sid = SessionId::new(session.clone());
    let out = run_turn(
        &app,
        &state,
        &sid,
        &session,
        text,
        attached.unwrap_or_default(),
    )
    .await;
    // Runs however the turn ended. Nothing is waiting on those questions any
    // more: the agent has stopped, so an approval still parked here is a
    // dialog asking about a call that will never be made.
    //
    // After the turn rather than inside it, so an error path (an early `?`)
    // cannot skip it.
    withdraw_session(&app, &state, &sid).await;
    out
}

/// Refuse and take down every approval still parked for a session.
///
/// The ids go to the page as `permission-withdrawn`, the same way `cancel`
/// returns what it withdrew: a dialog that can no longer be honoured must not
/// stay on screen looking answerable.
async fn withdraw_session<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, AppState>,
    sid: &SessionId,
) {
    let mut gone = Vec::new();
    {
        let mut pending = state.pending.lock().await;
        pending.retain(|id, ask| {
            if ask.session() == sid {
                // Delivery is irrelevant: either this refuses it, or it had
                // already settled. Both end with nothing to answer.
                let _settled = ask.answer(RequestPermissionOutcome::Cancelled);
                gone.push(id.clone());
                false
            } else {
                true
            }
        });
    }
    if !gone.is_empty() {
        let _ = app.emit("permission-withdrawn", gone);
    }
}

async fn run_turn<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, AppState>,
    sid: &SessionId,
    session: &str,
    text: String,
    attached: Vec<String>,
) -> Result<Turn, String> {
    let mut turn = engine_handle(state)
        .await?
        .prompt_start_with(sid, text, attached)
        .map_err(|e| e.to_string())?;

    loop {
        drain_updates(app, state, sid, session).await?;

        let asks = {
            let mut engine = state.engine.lock().await;
            let engine = engine.as_mut().ok_or("not connected")?;
            let mut out = Vec::new();
            while let Some(ask) = engine.next_permission() {
                out.push(ask);
            }
            out
        };
        for ask in asks {
            let id = uuid::Uuid::new_v4().to_string();
            // Parked before it is announced. The window answers by id, and
            // `answer_permission` refuses an id it cannot find, so a question
            // put on screen before it is answerable is one a fast answer would
            // bounce off. Do not reorder these two statements.
            let shown = describe(&id, session, &ask);
            state.pending.lock().await.insert(id, ask);
            let _ = app.emit("permission-request", shown);
        }

        match turn.try_recv() {
            Ok(reply) => {
                let stop = reply.map_err(|e| e.to_string())?;
                // One last drain. The stop reason and the turn's closing
                // words race: the agent sends the text and then answers the
                // prompt, and if the answer is seen first the loop above would
                // return without emitting the last thing the Bot said, which
                // looks like the Bot stopped mid-thought.
                drain_updates(app, state, sid, session).await?;
                return Ok(ended(&stop));
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Err("openbot acp is gone".into());
            }
        }
    }
}

/// Drain the engine's shared stream into the inbox, and take one session's.
///
/// Every caller drains everything and takes only what is theirs, so an
/// update belonging to another conversation is kept for it rather than
/// dropped by whoever happened to look first.
async fn collect(
    state: &State<'_, AppState>,
    session: &SessionId,
) -> Result<Vec<SessionUpdate>, String> {
    let mut drained = Vec::new();
    {
        let mut engine = state.engine.lock().await;
        let engine = engine.as_mut().ok_or("not connected")?;
        while let Some(pair) = engine.next_update() {
            drained.push(pair);
        }
    }
    let mut inbox = state.inbox.lock().await;
    inbox.absorb(drained);
    Ok(inbox.take(&session.0))
}

/// Move whatever the agent has said into the page, as `chunk` events.
async fn drain_updates<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, AppState>,
    sid: &SessionId,
    session: &str,
) -> Result<(), String> {
    let updates = collect(state, sid).await?;
    for update in updates {
        if let Some(chunk) = render(session, update) {
            let _ = app.emit("chunk", chunk);
        }
    }
    Ok(())
}

/// The person answered an approval: `option_id` is one of the ids the window
/// was offered (or `""` for a plain refusal).
#[tauri::command]
async fn answer_permission(
    state: State<'_, AppState>,
    id: String,
    option_id: String,
) -> Result<(), String> {
    let mut pending = state.pending.lock().await;
    let Some(mut ask) = pending.remove(&id) else {
        return Err("no such permission request".into());
    };
    let outcome = if option_id.is_empty() {
        RequestPermissionOutcome::Cancelled
    } else {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
    };
    if ask.answer(outcome) {
        return Ok(());
    }
    // The decision went nowhere. The request settled without it (the engine's
    // own timeout, the turn ending, the connection going away), and those are
    // indistinguishable from here, so the error names the consequence rather
    // than guessing the cause. Reporting success would close the dialog
    // exactly as if the person had allowed something the Bot never received.
    Err("this was already settled without your answer — the Bot did not receive it, and whatever it asked about did not happen".into())
}

/// Put a file the person chose where the Bot can read it.
///
/// Returns the workspace-relative path it landed at, which is what the prompt
/// will carry and what `fs.read` takes. The window shows the file's own name;
/// this path is what travels.
///
/// The copy is done by `openbot attach`, which asks the running guest where its
/// workspace is. Doing it here would mean reproducing `openbot-store`'s layout
/// and getting it silently wrong for anyone running `openbot up --workspace`:
/// the copy would still succeed and only the Bot would be unable to open the
/// file.
#[tauri::command]
async fn attach_file(state: State<'_, AppState>) -> Result<Attached, String> {
    let Some(file) = rfd::FileDialog::new().pick_file() else {
        // Cancelled the picker. Not an error, and not an attachment.
        return Err(CANCELLED.to_owned());
    };
    let (openbot, hub) = {
        let w = state.where_.lock().await;
        let w = w.as_ref().ok_or("not connected")?;
        (w.openbot.clone(), w.hub.clone())
    };
    let path = openbot_desktop::attach::put(&openbot, &hub, &file)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Attached {
        name: file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone()),
        path,
    })
}

/// What `attach_file` gives the window: a name to show, a path to send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Attached {
    /// The file's own name, for the chip in the composer.
    pub name: String,
    /// Workspace-relative, for the prompt.
    pub path: String,
}

/// The picker was dismissed. A sentinel rather than an error string the window
/// would show: choosing nothing is not a failure and must not raise a message.
const CANCELLED: &str = "cancelled";

/// The person typed a credential the Bot asked for.
///
/// Separate from `answer_permission` rather than an extra argument on it,
/// because the two are different acts and confusing them is expensive: an
/// approval id echoed back with a value would attach a credential to whatever
/// request happened to be parked under that id. `PendingPermission::supply`
/// refuses anything that is not a credential request, so this cannot put a
/// value on an ordinary approval even if asked to.
///
/// The value is not logged, not stored here, and not returned. It is moved
/// into the reply and dropped. An empty answer leaves the request pending.
#[tauri::command]
async fn supply_secret(
    state: State<'_, AppState>,
    id: String,
    value: String,
) -> Result<(), String> {
    let mut pending = state.pending.lock().await;
    let Some(mut ask) = pending.remove(&id) else {
        return Err("no such permission request".into());
    };
    if ask.secret_request().is_none() {
        return Err("that request was not asking for a credential".into());
    }
    if value.trim().is_empty() {
        // Put it back: an empty box is somebody who has not answered yet, and
        // taking the dialog down would strand the turn until it timed out.
        pending.insert(id, ask);
        return Err("nothing was entered".into());
    }
    if ask.supply(&value) {
        return Ok(());
    }
    // Same reasoning as `answer_permission`: the request settled without this,
    // and the causes are indistinguishable from here. Naming the consequence
    // matters more for a credential than for an approval: the person has just
    // typed a secret and must know it went nowhere rather than watching the
    // box close as though it had been stored.
    Err("this was already settled without your answer — the credential was not stored, and the Bot did not receive it".into())
}

/// Ask the agent to stop the current turn, cooperatively.
///
/// Any approval the turn was waiting on is refused first, so the turn cannot
/// hang on an unanswered question, and the ids of those refused asks are
/// returned so the window can take their dialogs down.
#[tauri::command]
async fn cancel(state: State<'_, AppState>, session: String) -> Result<Vec<String>, String> {
    let sid = SessionId::new(session);
    let mut withdrawn = Vec::new();
    let mut pending = state.pending.lock().await;
    pending.retain(|id, ask| {
        if ask.session() == &sid {
            // The id is returned whether or not the refusal landed.
            // `withdrawn` tells the window which dialogs to take down, and a
            // request that had already settled is one whose dialog should not
            // still be on screen. The opposite error, reporting a decision as
            // made when it was not, is what `answer_permission` guards
            // against.
            let _settled = ask.answer(RequestPermissionOutcome::Cancelled);
            withdrawn.push(id.clone());
            false
        } else {
            true
        }
    });
    drop(pending);
    // Same reason as `new_session`: never await a reply while holding this
    // lock, least of all here. A Stop that waits for the turn it is stopping
    // does not stop anything.
    let handle = engine_handle(&state).await?;
    handle.cancel(&sid).await.map_err(|e| e.to_string())?;
    Ok(withdrawn)
}

/// Turn an update into something the page can show; things with no words
/// (usage, plan, mode changes) are not chat and are dropped.
///
/// `pub` for the tests: what this drops never reaches the window, so the
/// decisions are asserted from outside rather than inferred from rendered
/// output.
pub fn render(session: &str, update: SessionUpdate) -> Option<Chunk> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => text_chunk(session, Kind::Agent, chunk.content),
        SessionUpdate::UserMessageChunk(chunk) => text_chunk(session, Kind::User, chunk.content),
        SessionUpdate::AgentThoughtChunk(chunk) => {
            text_chunk(session, Kind::Thought, chunk.content)
        }
        SessionUpdate::ToolCall(call) => Some(Chunk {
            session: session.to_owned(),
            kind: Kind::Tool,
            args: call.raw_input.clone(),
            text: match call.raw_input {
                Some(input) => format!("{} {}", call.title, one_line(&input.to_string())),
                None => call.title,
            },
        }),
        // What the machine is doing while it does it, and how it ended. The
        // docs call the first of these "computer use" and promise the
        // transcript shows it.
        SessionUpdate::ToolCallUpdate(update) => {
            if let Some(stage) = update.fields.content.as_deref().and_then(first_text) {
                return Some(Chunk {
                    session: session.to_owned(),
                    kind: Kind::Progress,
                    args: None,
                    text: stage,
                });
            }
            // Two statuses mean a result; anything else is not one yet.
            // `ToolCallStatus` is `#[non_exhaustive]` and openbot sends
            // `InProgress` on the opening `ToolCall`, so a binary
            // `status == Completed` test would draw a red ✗ for a tool that
            // is still running. When this build cannot tell whether a call
            // worked it says nothing: a missing result line is an absence a
            // person can notice, and a ✗ is a claim they will believe.
            let ok = match update.fields.status? {
                ToolCallStatus::Completed => true,
                ToolCallStatus::Failed => false,
                _ => return None,
            };
            let out = update
                .fields
                .raw_output
                .as_ref()
                .map(|v| match v {
                    // Already-rendered text (a replayed result) reads better
                    // unquoted than as a JSON string literal.
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Some(Chunk {
                session: session.to_owned(),
                kind: Kind::Result,
                args: None,
                text: format!("{} {}", if ok { "✓" } else { "✗" }, one_line(&out))
                    .trim_end()
                    .to_owned(),
            })
        }

        // Named explicitly so the drop reads as a decision rather than an
        // oversight. openbot has no plan surface, so `openbot acp` never sends
        // this. `every_update_the_adapter_sends_is_rendered` catches the day
        // that changes: an update nobody renders is a message that never
        // reaches the window.
        SessionUpdate::Plan(_) => None,

        // Every named variant is handled above; this arm exists only because
        // `SessionUpdate` is `#[non_exhaustive]` and a total match cannot be
        // written. Removing it fails the build with `_ not covered` and
        // nothing else, which confirms the list above is complete. A schema
        // bump lands here silently, so re-read the new variants when
        // `agent-client-protocol` moves.
        _ => None,
    }
}

/// How much of a tool's output belongs in a chat log.
///
/// A `fs.read` of a large file would otherwise put the whole thing in the
/// transcript, between the question and the answer. The full value is still
/// on the wire for a client that wants to show it; this is the chat line, not
/// the record.
const SUMMARY_LIMIT: usize = 160;

/// One line, bounded, with the truncation visible rather than silent.
fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SUMMARY_LIMIT {
        return flat;
    }
    let kept: String = flat.chars().take(SUMMARY_LIMIT).collect();
    format!("{kept}…")
}

/// The first piece of text in a tool call's content, if it has any.
fn first_text(content: &[agent_client_protocol::schema::v1::ToolCallContent]) -> Option<String> {
    content.iter().find_map(|c| match c {
        agent_client_protocol::schema::v1::ToolCallContent::Content(inner) => {
            match &inner.content {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            }
        }
        _ => None,
    })
}

fn text_chunk(session: &str, kind: Kind, content: ContentBlock) -> Option<Chunk> {
    match content {
        ContentBlock::Text(t) => Some(Chunk {
            session: session.to_owned(),
            kind,
            text: t.text,
            // Prose, not a tool call.
            args: None,
        }),
        _ => None,
    }
}

/// Describe a parked approval for the window.
fn describe(id: &str, session: &str, ask: &PendingPermission) -> PermissionAsk {
    PermissionAsk {
        id: id.to_owned(),
        session: session.to_owned(),
        tool: ask.tool_call().fields.title.clone().unwrap_or_default(),
        fields: fields_of(ask.tool_call().fields.raw_input.as_ref()),
        options: ask
            .options()
            .iter()
            .map(|o| AskOption {
                id: o.option_id.0.to_string(),
                name: o.name.clone(),
                // `to_value` then `as_str`, not `to_string`: `to_string` of a
                // string-shaped enum yields the JSON literal, quotes and all,
                // and the page could never match on `"allow_once"` with the
                // quotes inside the value.
                kind: serde_json::to_value(o.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into()),
                danger: refuses(o.kind),
            })
            .collect(),
        secret: ask.secret_request().map(|s| SecretPrompt {
            name: s.name.clone(),
            why: s.why.clone(),
        }),
    }
}

/// How a turn ended, in the protocol's words and in a person's.
///
/// The protocol word is kept because logs and tests want the exact thing;
/// `note` is what goes in front of a human. `EndTurn` gets no note at all:
/// the ordinary ending needs no announcement.
fn ended(stop: &StopReason) -> Turn {
    let note = match stop {
        StopReason::EndTurn => "",
        StopReason::MaxTokens => "stopped: the reply hit the token limit",
        StopReason::MaxTurnRequests => "stopped: too many steps in one turn",
        StopReason::Refusal => "the Bot declined",
        StopReason::Cancelled => "stopped",
        // The SDK's StopReason is non_exhaustive: a newer agent may end a turn
        // in a way this build has no word for, and inventing one would be
        // worse than saying so.
        _ => "the turn ended in a way this build does not recognise",
    };
    Turn {
        stop: format!("{stop:?}"),
        note: note.to_owned(),
    }
}

/// The shell, ready to build. Shared by `main` and by the tests, so what the
/// tests drive is the command set that ships.
///
/// Generic over the runtime because `tauri::test::mock_builder()` hands back
/// a `Builder<MockRuntime>`: the same handlers, no window.
#[must_use]
pub fn shell<R: Runtime>(builder: tauri::Builder<R>, mode: Mode) -> tauri::Builder<R> {
    builder
        .manage(AppState::new(mode))
        .invoke_handler(tauri::generate_handler![
            connect,
            disconnect,
            connected,
            roster,
            open_bot,
            open_computer,
            close_computer,
            computer_alive,
            secret_list,
            secret_set,
            supply_secret,
            attach_file,
            secret_remove,
            policy_list,
            policy_add,
            policy_remove,
            connectors,
            routines,
            skills,
            bot_describe,
            bot_duplicate,
            bot_delete,
            bot_hide,
            routine_pause,
            groups,
            group_log,
            open_group,
            search,
            default_openbot,
            default_home,
            pick_folder,
            pick_binary,
            new_session,
            prompt,
            answer_permission,
            cancel
        ])
}

/// Open the window.
///
/// # Panics
/// If Tauri cannot start; there is no window to report it in.
pub fn run() {
    let mode = Mode::from_args(std::env::args());
    shell(tauri::Builder::default(), mode)
        .run(tauri::generate_context!())
        .expect("OPENBOT failed to run");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_command_line_means_a_real_agent() {
        assert_eq!(Mode::from_args(["openbot-app"]), Mode::Live);
    }

    #[test]
    fn the_demo_flags_are_read_off_the_command_line() {
        assert_eq!(Mode::from_args(["openbot-app", "--demo"]), Mode::Reply);
        assert_eq!(
            Mode::from_args(["openbot-app", "--demo-tools"]),
            Mode::Tools
        );
        assert_eq!(
            Mode::from_args(["openbot-app", "--demo-secret"]),
            Mode::Secret
        );
    }

    /// Asked for more than one demo, take the smaller claim, ordered by what
    /// each does rather than by which was typed first.
    ///
    /// `--demo` makes no tool call at all; `--demo-secret` makes one that only
    /// asks a question; `--demo-tools` writes a file and runs a shell command.
    /// Both orderings of every pair are checked, since precedence that
    /// depends on argument order cannot be relied on.
    #[test]
    fn asked_for_more_than_one_demo_the_quieter_one_wins() {
        for pair in [
            ["--demo-tools", "--demo"],
            ["--demo", "--demo-tools"],
            ["--demo-secret", "--demo"],
            ["--demo", "--demo-secret"],
        ] {
            assert_eq!(
                Mode::from_args(["openbot-app", pair[0], pair[1]]),
                Mode::Reply,
                "{pair:?} should give the quietest demo"
            );
        }
        for pair in [
            ["--demo-tools", "--demo-secret"],
            ["--demo-secret", "--demo-tools"],
        ] {
            assert_eq!(
                Mode::from_args(["openbot-app", pair[0], pair[1]]),
                Mode::Secret,
                "{pair:?}: asking for a credential does less than writing files"
            );
        }
        // All three, in the order that would trip a first-flag-wins reading.
        assert_eq!(
            Mode::from_args(["openbot-app", "--demo-tools", "--demo-secret", "--demo"]),
            Mode::Reply
        );
    }

    /// The window has no way to ask for a demo, so a mode can only come from
    /// the command line, and `Live` must leave every engine demo flag off.
    #[test]
    fn a_live_shell_sets_no_demo_flag_on_the_engine() {
        let mut cfg = Config::new("openbot".into(), "home".into(), "ws://x".into());
        cfg.demo = true;
        cfg.demo_tools = true;
        cfg.demo_secret = true;
        Mode::Live.apply(&mut cfg);
        assert!(!cfg.demo, "live must clear --demo");
        assert!(!cfg.demo_tools, "live must clear --demo-tools");
        assert!(!cfg.demo_secret, "live must clear --demo-secret");
    }

    #[test]
    fn each_demo_mode_sets_exactly_its_own_flag() {
        let mut cfg = Config::new("openbot".into(), "home".into(), "ws://x".into());
        for (mode, want) in [
            (Mode::Reply, (true, false, false)),
            (Mode::Tools, (false, true, false)),
            (Mode::Secret, (false, false, true)),
            (Mode::Live, (false, false, false)),
        ] {
            mode.apply(&mut cfg);
            assert_eq!(
                (cfg.demo, cfg.demo_tools, cfg.demo_secret),
                want,
                "{mode:?} set the wrong engine flags"
            );
        }
    }

    fn agent_text(text: &str) -> SessionUpdate {
        use agent_client_protocol::schema::v1::{ContentChunk, TextContent};
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text.to_owned(),
        ))))
    }

    #[test]
    fn what_the_agent_says_is_rendered_as_the_agent() {
        let chunk = render("s1", agent_text("hello")).expect("agent text renders");
        assert_eq!(chunk.kind, Kind::Agent);
        assert_eq!(chunk.text, "hello");
        assert_eq!(
            chunk.session, "s1",
            "a chunk must say which conversation it belongs to"
        );
    }

    #[test]
    fn a_thought_is_not_labelled_as_speech() {
        use agent_client_protocol::schema::v1::{ContentChunk, TextContent};
        let update = SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("hmm".to_owned()),
        )));
        let chunk = render("s1", update).expect("a thought renders");
        assert_eq!(
            chunk.kind,
            Kind::Thought,
            "a thought rendered as speech puts words in the Bot's mouth"
        );
    }

    #[test]
    fn a_tool_call_carries_what_it_was_asked_to_do() {
        use agent_client_protocol::schema::v1::{ToolCall, ToolCallId};
        let mut call = ToolCall::new(ToolCallId::new("t1"), "fs.write".to_owned());
        call.raw_input = Some(serde_json::json!({"path": "notes.md"}));
        let chunk = render("s1", SessionUpdate::ToolCall(call)).expect("a tool call renders");
        assert_eq!(chunk.kind, Kind::Tool);
        assert!(
            chunk.text.starts_with("fs.write"),
            "the tool's name leads, got {:?}",
            chunk.text
        );
        assert!(
            chunk.text.contains("notes.md"),
            "a tool call with no arguments shown tells the person nothing, got {:?}",
            chunk.text
        );
    }

    /// Updates with no words are not chat. Rendering them would put empty
    /// bubbles in the log.
    #[test]
    fn wordless_updates_are_not_chat() {
        use agent_client_protocol::schema::v1::{CurrentModeUpdate, SessionModeId};
        let update =
            SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(SessionModeId::new("ask")));
        assert!(render("s1", update).is_none());
    }

    fn tool_update(
        fields: agent_client_protocol::schema::v1::ToolCallUpdateFields,
    ) -> SessionUpdate {
        use agent_client_protocol::schema::v1::{ToolCallId, ToolCallUpdate};
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new("c1"), fields))
    }

    /// "Computer use", from the docs' list of what a transcript shows.
    #[test]
    fn a_stage_reaches_the_page_while_the_tool_runs() {
        use agent_client_protocol::schema::v1::{
            Content, TextContent, ToolCallContent, ToolCallUpdateFields,
        };
        let update =
            tool_update(
                ToolCallUpdateFields::default().content(vec![ToolCallContent::Content(
                    Content::new(ContentBlock::Text(TextContent::new(
                        "running echo openbot-ok".to_owned(),
                    ))),
                )]),
            );
        let chunk = render("s1", update).expect("a stage renders");
        assert_eq!(chunk.kind, Kind::Progress);
        assert_eq!(chunk.text, "running echo openbot-ok");
    }

    #[test]
    fn a_result_says_whether_it_worked_and_what_it_produced() {
        use agent_client_protocol::schema::v1::ToolCallUpdateFields;
        let ok = render(
            "s1",
            tool_update(
                ToolCallUpdateFields::default()
                    .status(ToolCallStatus::Completed)
                    .raw_output(serde_json::json!({ "written": "notes.md" })),
            ),
        )
        .expect("a result renders");
        assert_eq!(ok.kind, Kind::Result);
        assert!(ok.text.starts_with('✓'), "got {:?}", ok.text);
        assert!(ok.text.contains("notes.md"), "got {:?}", ok.text);

        let failed = render(
            "s1",
            tool_update(ToolCallUpdateFields::default().status(ToolCallStatus::Failed)),
        )
        .expect("a failure renders");
        assert!(
            failed.text.starts_with('✗'),
            "a failed tool that looks successful is worse than one that says nothing: {:?}",
            failed.text
        );
    }

    /// An `fs.read` of a large file would otherwise put the whole thing in the
    /// chat log, between the question and the answer.
    #[test]
    fn a_large_result_is_summarised_and_says_that_it_was() {
        use agent_client_protocol::schema::v1::ToolCallUpdateFields;
        let huge = "x".repeat(4000);
        let chunk = render(
            "s1",
            tool_update(
                ToolCallUpdateFields::default()
                    .status(ToolCallStatus::Completed)
                    .raw_output(serde_json::Value::String(huge)),
            ),
        )
        .expect("a result renders");
        assert!(
            chunk.text.chars().count() < SUMMARY_LIMIT + 8,
            "the whole file went into the chat log: {} chars",
            chunk.text.chars().count()
        );
        assert!(
            chunk.text.ends_with('…'),
            "truncation must be visible, not silent: {:?}",
            chunk.text
        );
    }

    /// Newlines in a tool's output would otherwise break the one-line shape
    /// the transcript's tool rows are built on.
    #[test]
    fn a_result_is_one_line() {
        assert_eq!(
            one_line(
                "a
  b	c"
            ),
            "a b c"
        );
    }

    /// An update with neither a stage nor a status has nothing to say, and an
    /// empty row in a transcript is a rendering artefact.
    #[test]
    fn an_update_with_nothing_in_it_is_not_a_message() {
        use agent_client_protocol::schema::v1::ToolCallUpdateFields;
        assert!(render("s1", tool_update(ToolCallUpdateFields::default())).is_none());
    }

    // ---- the approval card ----

    /// The docs: "review the target, scope, and values". The target is the
    /// short field (the file, the command) and it must be readable without
    /// scrolling past the payload.
    #[test]
    fn the_target_of_an_action_comes_before_its_payload() {
        let input = serde_json::json!({
            "contents": "# openbot

If you can read this, the whole path works.
",
            "path": "openbot-demo.md",
        });
        let fields = fields_of(Some(&input));
        assert_eq!(
            fields[0].name, "path",
            "the file being written should be the first thing read, got {fields:?}"
        );
        assert_eq!(fields[0].value, "openbot-demo.md");
        assert!(
            !fields[0].long,
            "a filename belongs on a line, not in a block"
        );
        assert!(
            fields[1].long,
            "file contents belong in a block, not on a line"
        );
    }

    /// Nothing may be hidden. This is where a person decides whether an
    /// action runs, so every argument the agent sent appears in full. A
    /// dialog that truncates gets a command approved that was never read.
    #[test]
    fn every_argument_survives_in_full() {
        let long = "x".repeat(5000);
        let input = serde_json::json!({
            "command": long.clone(),
            "cwd": "/workspace",
            "timeout_ms": 30000,
        });
        let fields = fields_of(Some(&input));
        assert_eq!(fields.len(), 3, "an argument went missing: {fields:?}");
        let command = fields
            .iter()
            .find(|f| f.name == "command")
            .expect("command");
        assert_eq!(
            command.value, long,
            "the command was shortened in the dialog that approves it"
        );
    }

    /// A path reads as `notes.md`, not `"notes.md"`: the quotes are JSON's,
    /// not the value's, and a person checking a filename should not have to
    /// mentally strip them.
    #[test]
    fn strings_are_shown_as_themselves() {
        let input = serde_json::json!({ "path": "notes.md", "overwrite": true });
        let fields = fields_of(Some(&input));
        let path = fields.iter().find(|f| f.name == "path").expect("path");
        assert_eq!(path.value, "notes.md");
        let overwrite = fields.iter().find(|f| f.name == "overwrite").expect("flag");
        assert_eq!(
            overwrite.value, "true",
            "a non-string is shown as the JSON it is"
        );
    }

    /// The same tool asked about twice must lay out the same way. A dialog
    /// whose fields move between calls is one people stop reading, and then
    /// approve.
    #[test]
    fn the_order_does_not_depend_on_how_the_agent_wrote_it() {
        let a = serde_json::json!({ "path": "a.md", "contents": "x", "mode": "w" });
        let b = serde_json::json!({ "mode": "w", "contents": "x", "path": "a.md" });
        let names = |v: serde_json::Value| {
            fields_of(Some(&v))
                .into_iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(a), names(b));
    }

    /// A tool whose arguments are not an object still has to be reviewable,
    /// and it gets no invented name: labelling it something the agent never
    /// called it is worse than leaving it unlabelled.
    #[test]
    fn arguments_that_are_not_an_object_are_still_shown() {
        let fields = fields_of(Some(&serde_json::json!(["a", "b"])));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "");
        assert_eq!(fields[0].value, r#"["a","b"]"#);
    }

    /// A tool with no arguments asks about nothing, and an empty field list is
    /// how the page knows to show no table at all.
    #[test]
    fn no_arguments_is_no_fields() {
        assert!(fields_of(None).is_empty());
        assert!(fields_of(Some(&serde_json::json!({}))).is_empty());
    }

    // ---- group threads ----

    #[test]
    fn a_group_thread_reads_as_a_conversation() {
        let json = serde_json::json!([
            {"role":"user","content":[{"text":"@Researcher gather the sources"}]},
            {"role":"assistant","content":[{"text":"Handing the draft to @Writer"}]}
        ]);
        let messages: Vec<ThreadMessage> = serde_json::from_value(json).expect("parses");
        let chunks = thread_chunks("g1", &messages);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, Kind::User);
        assert_eq!(
            chunks[1].kind,
            Kind::Agent,
            "a handoff between members is the group speaking, not the person"
        );
        assert!(
            chunks[1].text.contains("@Writer"),
            "the handoff is the point"
        );
    }

    /// A group thread is read for the handoffs. Tool calls and results bury
    /// them, and the group view is not where a person debugs a tool.
    #[test]
    fn tool_traffic_is_not_rendered_into_a_group_thread() {
        let json = serde_json::json!([
            {"role":"assistant","content":[
                {"id":"t1","name":"fs.read","input":{}},
                {"text":"Sources gathered."}
            ]}
        ]);
        let messages: Vec<ThreadMessage> = serde_json::from_value(json).expect("parses");
        let chunks = thread_chunks("g1", &messages);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Sources gathered.");
    }

    #[test]
    fn an_empty_thread_is_no_chunks_rather_than_an_empty_bubble() {
        let messages: Vec<ThreadMessage> = serde_json::from_value(
            serde_json::json!([{"role":"assistant","content":[{"text":"  "}]}]),
        )
        .expect("parses");
        assert!(thread_chunks("g1", &messages).is_empty());
    }

    // ---- the inbox ----

    fn text_update(t: &str) -> SessionUpdate {
        use agent_client_protocol::schema::v1::{ContentChunk, TextContent};
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            t.to_owned(),
        ))))
    }

    /// Nothing drained is discarded. The engine has one stream for every
    /// session, so whoever looks first sees other conversations' updates too;
    /// dropping the ones that do not match would let one conversation
    /// silently eat another's transcript.
    #[test]
    fn one_session_draining_does_not_eat_anothers_updates() {
        let mut inbox = Inbox::default();
        inbox.absorb(vec![
            (SessionId::new("a"), text_update("first for a")),
            (SessionId::new("b"), text_update("for b")),
            (SessionId::new("a"), text_update("second for a")),
        ]);

        // The session that looks first takes only its own.
        let a = inbox.take("a");
        assert_eq!(a.len(), 2, "a lost an update");
        // And b's is still there afterwards.
        let b = inbox.take("b");
        assert_eq!(b.len(), 1, "b's update was eaten by a's drain");
    }

    /// Order is the contract here too: a transcript reassembled out of order
    /// reads as a conversation that did not happen.
    #[test]
    fn a_sessions_updates_come_back_in_the_order_they_arrived() {
        let mut inbox = Inbox::default();
        inbox.absorb(vec![
            (SessionId::new("a"), text_update("one")),
            (SessionId::new("a"), text_update("two")),
            (SessionId::new("a"), text_update("three")),
        ]);
        let said: Vec<String> = inbox
            .take("a")
            .into_iter()
            .filter_map(|u| render("a", u).map(|c| c.text))
            .collect();
        assert_eq!(said, vec!["one", "two", "three"]);
    }

    /// The second take is empty; otherwise every poll of a running turn would
    /// re-render the whole conversation.
    #[test]
    fn taking_a_backlog_removes_it() {
        let mut inbox = Inbox::default();
        inbox.absorb(vec![(SessionId::new("a"), text_update("once"))]);
        assert_eq!(inbox.take("a").len(), 1);
        assert!(
            inbox.take("a").is_empty(),
            "the same update came back twice"
        );
    }

    /// A session nobody has heard from has nothing waiting, rather than
    /// whatever the last one left.
    #[test]
    fn an_unknown_session_has_an_empty_backlog() {
        let mut inbox = Inbox::default();
        inbox.absorb(vec![(SessionId::new("a"), text_update("mine"))]);
        assert!(inbox.take("someone-else").is_empty());
    }

    /// Disconnecting drops every backlog. Updates held for sessions that can
    /// no longer be addressed are a slow leak.
    #[test]
    fn clearing_forgets_every_backlog() {
        let mut inbox = Inbox::default();
        inbox.absorb(vec![(SessionId::new("a"), text_update("mine"))]);
        inbox.clear();
        assert!(inbox.take("a").is_empty());
    }

    #[test]
    fn an_ordinary_ending_says_nothing_to_the_person() {
        let turn = ended(&StopReason::EndTurn);
        assert_eq!(turn.stop, "EndTurn", "the protocol word is kept for logs");
        assert!(
            turn.note.is_empty(),
            "a status line reading EndTurn after every reply is noise, got {:?}",
            turn.note
        );
    }

    #[test]
    fn every_other_ending_is_explained_in_words() {
        for stop in [
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled,
        ] {
            let turn = ended(&stop);
            assert!(
                !turn.note.is_empty(),
                "{stop:?} left the person with no explanation"
            );
            assert!(
                !turn.note.contains(&turn.stop),
                "{stop:?} leaked the protocol word into the note: {:?}",
                turn.note
            );
        }
    }
}
