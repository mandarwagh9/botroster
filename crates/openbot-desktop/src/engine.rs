//! The OPENBOT engine: spawn `openbot acp`, speak ACP to it over stdio, and
//! drive sessions.
//!
//! This is the layer the Tauri shell renders: it owns the connection, streams
//! `session/update` notifications, and returns what a prompt turn ended in.
//! Everything here is a client of the ACP adapter, so the tests run the engine
//! against the shipped binary exactly as the adapter's own tests do.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, Meta,
    NewSessionRequest, PermissionOption, PermissionOptionId, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ResourceLink,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent, ToolCallUpdate,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo, LineDirection};

/// How many stderr lines from `openbot acp` are kept to explain a failed
/// connect. The message that matters is the first thing it prints and runs to
/// about four lines; the cap is there so a child that fails by printing
/// forever cannot grow the buffer without bound.
const STDERR_LINES_KEPT: usize = 40;

/// How long the engine keeps an approval request open for the shell to answer.
///
/// The hub enforces the real deadline (`DEFAULT_APPROVAL_TIMEOUT`). This is
/// only how long the engine holds the responder before answering `Cancelled`
/// itself, so a dialog nobody answers cannot pin the connection forever.
const PERMISSION_ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// How to reach the runtime this engine drives.
pub struct Config {
    /// Path to the `openbot` binary.
    pub openbot: PathBuf,
    /// `--home` for the agent: where its Bots live.
    pub home: PathBuf,
    /// Hub URL (`OPENBOT_HUB_URL` equivalent).
    pub hub: String,
    /// `--demo`: one scripted reply, no tools, no key. The shell never sets
    /// this; the tests do, because it is deterministic.
    pub demo: bool,
    /// `--demo-tools`: the scripted tool demo (write, read back, list, shell),
    /// so the approval path can be exercised end to end without a model. The
    /// shell never sets this; the tests do.
    pub demo_tools: bool,
    /// `--demo-secret`: ask once for a credential, no model. The shell never
    /// sets this; the tests do, because it is the only deterministic way to
    /// drive the credential prompt.
    pub demo_secret: bool,
    /// `--bot`: pin every session this agent serves to one Bot, whatever a
    /// client asks for. An operator's decision, above the client's.
    pub bot: Option<String>,
    /// The model's API key, as `(variable name, value)`, handed to the agent
    /// process in its environment.
    ///
    /// The runtime reads the key with `std::env::var` under whatever name
    /// `api_key_env` gives, and `config.toml` deliberately holds that name and
    /// never the key. So a window that collects a key has exactly one place to
    /// put it: the environment of the process it spawns. It is not written to
    /// disk by this crate, and it lives no longer than the connection.
    pub api_key: Option<(String, String)>,
}

impl Config {
    /// An agent with a real model configured; what the shell builds.
    pub fn new(openbot: PathBuf, home: PathBuf, hub: String) -> Self {
        Self {
            openbot,
            home,
            hub,
            demo: false,
            demo_tools: false,
            demo_secret: false,
            bot: None,
            api_key: None,
        }
    }
}

/// A `session/request_permission` from the agent, waiting for a person.
///
/// The shell renders this as a dialog. The decision flows through the oneshot
/// the engine's driver task is waiting on; answering never needs the engine
/// itself, so a dialog can be answered while a turn is in flight.
///
/// Dropped unanswered, it becomes a `Cancelled` (fail closed), and so does a
/// request nobody answers before [`PERMISSION_ANSWER_TIMEOUT`].
pub struct PendingPermission {
    session: SessionId,
    tool_call: ToolCallUpdate,
    options: Vec<PermissionOption>,
    secret: Option<SecretAsk>,
    answer: Option<tokio::sync::oneshot::Sender<Answer>>,
}

/// What the connection task sends back: the person's choice, plus any `_meta`
/// that choice carries. Only a credential answer has the second part.
type Answer = (RequestPermissionOutcome, Option<Meta>);

/// A request for a credential the Bot does not have.
///
/// ACP has no free-text prompt: the only ask an agent can make is
/// `session/request_permission`, answered with the id of an option. So
/// `openbot acp` marks the request in `_meta` and expects the value back the
/// same way. This is that marker, decoded.
///
/// It holds the question, never the answer. The value goes straight from the
/// caller into the reply in [`PendingPermission::supply`] and is not stored on
/// this struct, so there is no credential for a `Debug` to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAsk {
    /// What it will be stored as, and what a connector references.
    pub name: String,
    /// Why the Bot needs it, in its own words. Shown to the person.
    pub why: String,
}

// The `_meta` contract comes from the wire-types crate rather than being
// redeclared here. `openbot acp` is a separate process, so a second copy of
// these strings would compile, pass every test on both sides, and silently
// stop working the moment either was renamed.
use openbot_proto::approval::{SECRET_META, SECRET_PROVIDE as PROVIDE};

impl SecretAsk {
    /// Decode the marker, or `None` for an ordinary approval.
    ///
    /// Fails closed into "ordinary approval": a marker without both fields is
    /// not a credential request, because a prompt that cannot say what it
    /// wants or why cannot be answered responsibly.
    fn from_meta(meta: Option<&Meta>) -> Option<Self> {
        let m = meta?.get(SECRET_META)?;
        Some(Self {
            name: m.get("name")?.as_str()?.to_owned(),
            why: m.get("why")?.as_str()?.to_owned(),
        })
    }
}

impl PendingPermission {
    /// The session the tool call belongs to.
    #[must_use]
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// What the agent wants to call, with what it is asking to run.
    #[must_use]
    pub fn tool_call(&self) -> &ToolCallUpdate {
        &self.tool_call
    }

    /// The choices a person can make about this call.
    #[must_use]
    pub fn options(&self) -> &[PermissionOption] {
        &self.options
    }

    /// Send the person's decision back to the agent, and say whether it
    /// arrived.
    ///
    /// `&mut self` rather than `self` so a shell can answer a borrowed entry
    /// it keeps in a map (the sender is taken out, so a second answer is a
    /// no-op rather than a panic).
    ///
    /// `false` means the decision went nowhere, and a caller must not report
    /// success for it. The request can settle without this answer in more
    /// than one way ([`PERMISSION_ANSWER_TIMEOUT`] expiring, the connection
    /// task going away, the turn ending and taking it with it), and from this
    /// side they are indistinguishable: all of them look like a receiver that
    /// is no longer there. Report the consequence, not a guess at the cause.
    /// Otherwise a person answering a stale dialog sees it close as though the
    /// answer had taken effect when the call was already refused.
    #[must_use = "a decision that did not reach the Bot must not be reported as made"]
    pub fn answer(&mut self, outcome: RequestPermissionOutcome) -> bool {
        self.answer
            .take()
            .is_some_and(|answer| answer.send((outcome, None)).is_ok())
    }

    /// The credential this is asking for, or `None` for an ordinary approval.
    ///
    /// A shell that ignores this renders the usual dialog with a "Provide
    /// credential" button that cannot supply one, which is a refusal rather
    /// than a leak, but not useful. Check it before rendering.
    #[must_use]
    pub fn secret_request(&self) -> Option<&SecretAsk> {
        self.secret.as_ref()
    }

    /// Hand over a credential the person typed.
    ///
    /// Returns `false` on the same terms as [`Self::answer`] (the value went
    /// nowhere and must not be reported as supplied), and additionally if this
    /// is not a credential request at all. That last case is a programming
    /// error rather than a race: answering an ordinary approval with a secret
    /// would put the value in `_meta` for a request nobody reads it from.
    ///
    /// The value is not stored on `self`. It goes into the reply and out of
    /// scope; nothing is left here for a later `Debug` to print.
    #[must_use = "a credential that did not reach the Bot must not be reported as supplied"]
    pub fn supply(&mut self, value: &str) -> bool {
        if self.secret.is_none() {
            return false;
        }
        // An empty answer is a refusal, matching the terminal prompt and
        // `openbot acp`'s own reading. Storing an empty credential under a real
        // name is worse than storing nothing: it fails against a service later
        // with nothing on this side to explain why.
        let value = value.trim_end();
        if value.trim().is_empty() {
            return false;
        }
        let mut meta = Meta::new();
        meta.insert(
            SECRET_META.to_owned(),
            serde_json::json!({ "value": value }),
        );
        let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            PermissionOptionId::new(PROVIDE),
        ));
        self.answer
            .take()
            .is_some_and(|answer| answer.send((outcome, Some(meta))).is_ok())
    }
}

/// A cheap, cloneable way to talk to a running [`Engine`].
///
/// Exists so a caller can send a command without holding whatever lock the
/// engine lives behind. Waiting for a round trip while holding that lock
/// deadlocks: `new_session` holds it across the reply, the reply waits on the
/// turn, and the turn needs the lock to deliver the approval it is blocked
/// on. Take a handle, drop the lock, then await.
#[derive(Clone)]
pub struct EngineHandle {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
}

impl EngineHandle {
    /// Open a session on a directory. The adapter binds it to the Bot for
    /// that directory.
    pub async fn new_session(&self, cwd: impl Into<String>) -> anyhow::Result<SessionId> {
        self.new_session_for(None, cwd).await
    }

    /// Open a session on a named Bot, which is what a client with a roster
    /// wants: a teammate picked from a sidebar rather than a folder. The
    /// directory still travels, because the agent needs one and the Bot
    /// still works somewhere.
    ///
    /// `None` is the editor's behaviour: the Bot for the directory.
    pub async fn new_session_for(
        &self,
        who: Option<Who>,
        cwd: impl Into<String>,
    ) -> anyhow::Result<SessionId> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Command::NewSession {
            cwd: cwd.into(),
            who,
            reply: tx,
        })?;
        rx.await
            .map_err(|_| anyhow::anyhow!("openbot acp is gone"))?
            .map_err(anyhow::Error::msg)
    }

    /// Start a prompt turn, returning the receiver its stop reason lands on.
    pub fn prompt_start(
        &self,
        session: &SessionId,
        text: impl Into<String>,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Result<StopReason, String>>> {
        self.prompt_start_with(session, text, Vec::new())
    }

    /// The same, carrying files the person attached.
    ///
    /// A separate entry point rather than a changed signature: most callers
    /// mean "no attachments", and keeping them distinct shows which call sites
    /// can actually carry files.
    pub fn prompt_start_with(
        &self,
        session: &SessionId,
        text: impl Into<String>,
        attached: Vec<String>,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Result<StopReason, String>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Command::Prompt {
            session: session.clone(),
            text: text.into(),
            attached,
            reply: tx,
        })?;
        Ok(rx)
    }

    /// Ask the agent to stop the current turn, cooperatively.
    pub async fn cancel(&self, session: &SessionId) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Command::Cancel {
            session: session.clone(),
            reply: tx,
        })?;
        rx.await
            .map_err(|_| anyhow::anyhow!("openbot acp is gone"))?
            .map_err(anyhow::Error::msg)
    }

    /// Re-attach a session id to the durable conversation for a directory,
    /// and have the agent replay it.
    ///
    /// The updates arrive as ordinary `session/update` notifications before
    /// this returns, so a caller draining the update stream afterwards has the
    /// whole transcript. The id need not be one this process handed out;
    /// openbot resolves the conversation from `cwd`.
    pub async fn load_session(
        &self,
        session: &SessionId,
        cwd: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.load_session_for(session, None, cwd).await
    }

    /// Reopen a named Bot's conversation. Same precedence as
    /// [`EngineHandle::new_session_for`].
    pub async fn load_session_for(
        &self,
        session: &SessionId,
        who: Option<Who>,
        cwd: impl Into<String>,
    ) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Command::Load {
            session: session.clone(),
            cwd: cwd.into(),
            who,
            reply: tx,
        })?;
        rx.await
            .map_err(|_| anyhow::anyhow!("openbot acp is gone"))?
            .map_err(anyhow::Error::msg)
    }

    fn send(&self, cmd: Command) -> anyhow::Result<()> {
        self.commands
            .send(cmd)
            .map_err(|e| anyhow::anyhow!("openbot acp is gone: {e}"))
    }
}

/// Check that a binary exists at this path, or on `PATH` under this name.
///
/// # Errors
/// If it does not, naming what was looked for and what to do. This is the
/// error a first run produces, so it must give a next step.
fn found(openbot: &Path) -> anyhow::Result<()> {
    if openbot.components().count() > 1 || openbot.is_absolute() {
        if openbot.is_file() {
            return Ok(());
        }
        anyhow::bail!(
            "no openbot binary at {} — check the path, or pick it with the … button",
            openbot.display()
        );
    }
    // A bare name: whatever the OS would search.
    if on_path(openbot).is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "`{}` is not on your PATH. OPENBOT drives the openbot runtime and does not include it: install it with `cargo install --path crates/openbot-cli`, or point the openbot binary field at the executable",
        openbot.display()
    )
}

/// The first match for a bare name on `PATH`, with the platform's extensions.
fn on_path(name: &Path) -> Option<std::path::PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE".into())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let base = dir.join(name);
        if base.is_file() {
            return Some(base);
        }
        for ext in &exts {
            let with = base.with_extension(ext.trim_start_matches('.'));
            if with.is_file() {
                return Some(with);
            }
        }
    }
    None
}

/// openbot's corner of ACP's `_meta`, or nothing at all.
///
/// `None` in, `None` out: a request with no Bot named carries no `_meta`, so
/// an agent that has never heard of openbot sees an unmodified request.
fn openbot_meta(who: Option<Who>) -> Option<agent_client_protocol::schema::v1::Meta> {
    let value = match who? {
        Who::Bot(name) => serde_json::json!({ "bot": name }),
        Who::Group(name) => serde_json::json!({ "group": name }),
    };
    let mut meta = agent_client_protocol::schema::v1::Meta::new();
    meta.insert("openbot".to_owned(), value);
    Some(meta)
}

/// Which conversation a session should answer in.
///
/// A group is not a Bot: several teammates answer in it and which one depends
/// on who was `@mentioned`, so the session names the group and the agent
/// decides per message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    Bot(String),
    Group(String),
}

/// A connection to one `openbot acp`, driving one session.
pub struct Engine {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    updates: tokio::sync::mpsc::UnboundedReceiver<(SessionId, SessionUpdate)>,
    permissions: tokio::sync::mpsc::UnboundedReceiver<PendingPermission>,
    task: tokio::task::JoinHandle<Result<(), agent_client_protocol::Error>>,
    /// Cleared when the agent's stdout reaches EOF. See [`Engine::alive`].
    open: Arc<AtomicBool>,
}

/// One command to the agent-driver task, answered over a oneshot.
enum Command {
    NewSession {
        cwd: String,
        who: Option<Who>,
        reply: tokio::sync::oneshot::Sender<Result<SessionId, String>>,
    },
    Prompt {
        session: SessionId,
        text: String,
        /// Workspace-relative paths of files the person attached, already
        /// copied in by `openbot computer put`.
        attached: Vec<String>,
        reply: tokio::sync::oneshot::Sender<Result<StopReason, String>>,
    },
    Cancel {
        session: SessionId,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Load {
        session: SessionId,
        cwd: String,
        who: Option<Who>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

impl Engine {
    /// Spawn `openbot acp` and take it through the initialize handshake.
    ///
    /// Returns an error if the agent dies, answers a protocol version this
    /// client cannot speak, or goes quiet for half a minute.
    pub async fn connect(cfg: Config) -> anyhow::Result<Self> {
        // Look for the binary before spawning it. The SDK reports a failed
        // spawn as a connection that ended, which would surface as "openbot acp
        // ended before the handshake": a protocol message for what is a
        // missing file, and the first thing a person sees after installing.
        found(&cfg.openbot)?;

        // What the child said on its way out.
        //
        // `openbot acp` refuses to start when nothing is configured and states
        // both the fault and the fix on stderr. That is the only useful account
        // of a failed connect, and without this it is discarded: the SDK's own
        // child-exit report carries a stderr tail, but the transport-closed
        // error beats it to the task's return value, so awaiting the task
        // yields `Incoming transport closed` and nothing about the cause.
        // Reading the lines as they arrive is the one route that does not race.
        //
        // Bounded: a child that fails by printing forever must not be able to
        // grow this without limit, and the tail is what carries the message.
        let said: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();
        let sink = std::sync::Arc::clone(&said);

        let agent = AcpAgent::new(
            AcpAgentConfig::new(&cfg.openbot)
                .arg("acp")
                .arg("--home")
                .arg(cfg.home.to_str().expect("a utf-8 home path"))
                .env("OPENBOT_HUB_URL", &cfg.hub)
                .env("NO_COLOR", "1")
                // The key, when the window collected one. `Option` is an
                // iterator of at most one pair, so an absent key adds nothing
                // rather than an empty variable — which the runtime would read
                // as a key that is set and blank.
                .envs(cfg.api_key.clone())
                .args(if cfg.demo {
                    vec!["--demo".to_owned()]
                } else if cfg.demo_tools {
                    vec!["--demo-tools".to_owned()]
                } else if cfg.demo_secret {
                    vec!["--demo-secret".to_owned()]
                } else {
                    vec![]
                })
                .args(match &cfg.bot {
                    Some(bot) => vec!["--bot".to_owned(), bot.clone()],
                    None => vec![],
                }),
        )
        .with_debug(move |line, direction| {
            if matches!(direction, LineDirection::Stderr) {
                if let Ok(mut lines) = sink.lock() {
                    if lines.len() < STDERR_LINES_KEPT {
                        lines.push(line.trim_end().to_owned());
                    }
                }
            }
        });

        let (commands, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (updates, update_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permissions, permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handshake, handshake_rx) = tokio::sync::oneshot::channel();

        // Whether the agent is still on the other end of the pipe.
        //
        // Nothing else in this file can answer that. The driver task below is
        // not the signal: `connect_with` documents that a clean incoming EOF
        // "does not cancel unrelated work in `main_fn`", and this `main_fn`
        // sits on `command_rx.recv()` for the life of the `Engine` — so
        // `task.is_finished()` stays false over a corpse, forever. Asking the
        // task would have produced a check that always says yes.
        let open = Arc::new(AtomicBool::new(true));
        let closing = Arc::clone(&open);

        let task = tokio::spawn(async move {
            agent_client_protocol::Client
                .builder()
                // The agent's stdout reached EOF: it exited, crashed or was
                // killed. `Ok(())` rather than an error, deliberately —
                // returning an error here tears the connection down and
                // cancels `connect_with`, which would race every in-flight
                // turn's own report of what happened. The flag is enough: it
                // is what `alive` reads, and the shell polls that.
                .on_close(async move |_cx| {
                    closing.store(false, Ordering::Relaxed);
                    Ok(())
                })
                // Everything the agent says during a turn, forwarded out of
                // the SDK's event loop. The shell's job is showing this
                // stream, so it must never be stuck behind a request.
                .on_receive_notification(
                    async move |note: SessionNotification, _cx| {
                        let _ = updates.send((note.session_id, note.update));
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                // `session/request_permission` is how the hub's approval
                // engine reaches a human. The ask is handed to the shell as a
                // PendingPermission, and the decision is awaited off the event
                // loop, which must keep reading the wire (a cancel, a streamed
                // update) while the person decides. Unanswered, it times out
                // to `Cancelled`: a turn that needs approval must not proceed
                // silently under a client that cannot ask. The hub enforces
                // the decision either way; this is the asking surface, not
                // the gate.
                .on_receive_request(
                    async move |req: RequestPermissionRequest, responder, connection| {
                        let (answer, answer_rx) = tokio::sync::oneshot::channel();
                        // Decoded here rather than handed on raw: `Meta` is a
                        // `serde_json::Map` whose `Debug` prints its contents.
                        // The request half carries only a name and a reason,
                        // but keeping the map alive across the shell would put
                        // the answer's map in reach too.
                        let secret = SecretAsk::from_meta(req.meta.as_ref());
                        let _ = permissions.send(PendingPermission {
                            session: req.session_id,
                            tool_call: req.tool_call,
                            options: req.options,
                            secret,
                            answer: Some(answer),
                        });
                        let _ = connection.spawn(async move {
                            let (outcome, meta) =
                                tokio::time::timeout(PERMISSION_ANSWER_TIMEOUT, answer_rx)
                                    .await
                                    .ok()
                                    .and_then(|r| r.ok())
                                    .unwrap_or((RequestPermissionOutcome::Cancelled, None));
                            let _ = responder
                                .respond(RequestPermissionResponse::new(outcome).meta(meta));
                            Ok(())
                        });
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, |conn: ConnectionTo<Agent>| async move {
                    let init = conn
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    if init.protocol_version != ProtocolVersion::V1 {
                        return Err(anyhow::anyhow!(
                            "openbot answered protocol version {:?}, we speak v1",
                            init.protocol_version
                        )
                        .into());
                    }
                    // The client depends on session loading: without it,
                    // opening a folder shows an empty transcript beside a Bot
                    // that remembers everything. Fail loudly rather than
                    // silently degrading to a blank window.
                    if !init.agent_capabilities.load_session {
                        return Err(anyhow::anyhow!(
                            "openbot does not offer session loading, so a conversation could never be shown"
                        )
                        .into());
                    }
                    // The handshake is the first thing in the loop, so the
                    // caller waiting on this oneshot sees "up" or "dead".
                    let _ = handshake.send(());

                    // Every round trip is spawned rather than awaited here.
                    //
                    // Awaiting inline means one command at a time, and a
                    // prompt occupies this loop for the whole turn. `cancel`,
                    // whose purpose is to interrupt a running turn, would sit
                    // in the queue behind the prompt it was meant to cancel.
                    // `new_session` would be worse: the shell holds its own
                    // lock waiting for a reply that cannot come until the turn
                    // ends, and the turn cannot end because it needs that lock
                    // to deliver an approval.
                    while let Some(cmd) = command_rx.recv().await {
                        match cmd {
                            Command::NewSession { cwd, who, reply } => {
                                let mut req = NewSessionRequest::new(cwd);
                                req.meta = openbot_meta(who);
                                let sent = conn.send_request(req);
                                conn.spawn(async move {
                                    let res = sent.block_task().await;
                                    let _ = reply
                                        .send(res.map(|r| r.session_id).map_err(|e| e.to_string()));
                                    Ok(())
                                })?;
                            }
                            Command::Prompt {
                                session,
                                text,
                                attached,
                                reply,
                            } => {
                                // A link, never the contents. `openbot acp`
                                // folds a resource link into the task as an
                                // `[attached: ...]` line, so the Bot is told
                                // where the file is and reads it with
                                // `fs.read` if it needs to, under whatever
                                // policy the operator set. Sending the bytes
                                // instead would put them in the transcript,
                                // replay them into every following turn
                                // through the history window, and reach the
                                // model without passing the gate that decides
                                // whether this Bot may read files at all.
                                let mut blocks =
                                    vec![ContentBlock::Text(TextContent::new(text))];
                                for path in attached {
                                    let name = path
                                        .rsplit('/')
                                        .next()
                                        .unwrap_or(&path)
                                        .to_owned();
                                    blocks.push(ContentBlock::ResourceLink(ResourceLink::new(
                                        name, path,
                                    )));
                                }
                                let sent =
                                    conn.send_request(PromptRequest::new(session, blocks));
                                conn.spawn(async move {
                                    let res = sent.block_task().await;
                                    let _ = reply.send(
                                        res.map(|r| r.stop_reason).map_err(|e| e.to_string()),
                                    );
                                    Ok(())
                                })?;
                            }
                            Command::Load {
                                session,
                                cwd,
                                who,
                                reply,
                            } => {
                                let mut req = LoadSessionRequest::new(session, cwd);
                                req.meta = openbot_meta(who);
                                let sent = conn.send_request(req);
                                conn.spawn(async move {
                                    let res = sent.block_task().await;
                                    let _ = reply
                                        .send(res.map(|_| ()).map_err(|e| e.to_string()));
                                    Ok(())
                                })?;
                            }
                            Command::Cancel { session, reply } => {
                                let res = conn
                                    .send_notification(CancelNotification::new(session))
                                    .map_err(|e| e.to_string());
                                let _ = reply.send(res);
                            }
                        }
                    }
                    Ok(())
                })
                .await
        });

        // Connection-level failures after spawn are not errors on the wire;
        // they show up as this oneshot, or the task ending. Either way,
        // `connect` reports them instead of leaving the shell with a window
        // that has no agent behind it.
        //
        // The engine is not assembled until the handshake lands, because
        // `Drop for Engine` aborts the task and the failure path needs to
        // *read* it instead. See below.
        match tokio::time::timeout(Duration::from_secs(30), handshake_rx).await {
            Ok(Ok(())) => Ok(Self {
                commands,
                updates: update_rx,
                permissions: permission_rx,
                task,
                open,
            }),
            Ok(Err(_)) => {
                // The sender was dropped, so the task has already ended — and
                // it ended holding the only useful account of why. The SDK
                // formats a nonzero child exit as `Process exited with
                // {status}: {stderr}`, and `openbot acp` refuses to start with
                // a message that says exactly what is wrong and how to fix it:
                //
                //     Error: no usable model: no model configured.
                //     Set one once:  openbot config set --model grok-4-5
                //
                // Aborting the task threw that away and left "ended before the
                // handshake" — a protocol event standing in for a
                // configuration fact, and the first thing a person meets after
                // installing. This is the same fault the pre-spawn `found`
                // check above was added to prevent, one step later.
                //
                // The wait is bounded because the task has ended in every case
                // that reaches this arm; the timeout is for the one where the
                // oneshot was dropped for some other reason and the task is
                // still winding down. A missing reason is not worth hanging a
                // window on, so it falls back to the old wording.
                task.abort();
                let why = said
                    .lock()
                    .ok()
                    .map(|lines| {
                        lines.join(
                            "
",
                        )
                    })
                    .filter(|said| !said.trim().is_empty());
                Err(match why {
                    Some(why) => anyhow::anyhow!(
                        "openbot acp did not start.
{why}"
                    ),
                    None => anyhow::anyhow!("openbot acp ended before the handshake"),
                })
            }
            Err(_) => {
                task.abort();
                Err(anyhow::anyhow!(
                    "openbot acp went quiet for 30 s during the handshake"
                ))
            }
        }
    }

    /// Is `openbot acp` still on the other end?
    ///
    /// An `Engine` whose agent has died looks exactly like a working one from
    /// the outside: the struct is still here, the command channel still
    /// accepts sends, and the window goes on saying "connected" over a process
    /// that is gone. Every prompt after that fails, and nothing on screen says
    /// why. This is the check that lets it say why.
    ///
    /// Two signals, and both are read because the SDK documents two different
    /// endings:
    ///
    /// - `open` is cleared by the `on_close` callback on clean incoming EOF.
    /// - the driver task finishes, which the SDK says a *clean* EOF does not
    ///   cause — but a transport error does, and a child that dies of a signal
    ///   or a nonzero exit ends the transport that way.
    ///
    /// Measured, because the documentation reads as though only the first
    /// would fire and that is not what happens: killing the child trips both,
    /// every time, and each was checked alone against
    /// `an_agent_that_was_killed_stops_reporting_itself_alive`. The redundancy
    /// is kept for the ending that test cannot stage — an agent that exits
    /// zero, which is the case the SDK's own caveat is about, and which would
    /// otherwise leave this saying yes forever.
    ///
    /// Cheap enough to poll: an atomic load and a flag on a `JoinHandle`.
    /// Nothing is sent to the agent, so a dying agent is not asked to answer.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.open.load(Ordering::Relaxed) && !self.task.is_finished()
    }

    /// A handle for sending commands without holding whatever lock this
    /// engine lives behind. See [`EngineHandle`].
    #[must_use]
    pub fn handle(&self) -> EngineHandle {
        EngineHandle {
            commands: self.commands.clone(),
        }
    }

    /// Open a session on a directory, as the shell does when the user opens
    /// a project folder. The adapter binds it to the Bot for that directory.
    pub async fn new_session(&self, cwd: impl Into<String>) -> anyhow::Result<SessionId> {
        self.handle().new_session(cwd).await
    }

    /// Open a session on a named Bot. See [`EngineHandle::new_session_for`].
    pub async fn new_session_for(
        &self,
        who: Option<Who>,
        cwd: impl Into<String>,
    ) -> anyhow::Result<SessionId> {
        self.handle().new_session_for(who, cwd).await
    }

    /// Reopen a named Bot's conversation. See
    /// [`EngineHandle::load_session_for`].
    pub async fn load_session_for(
        &self,
        session: &SessionId,
        who: Option<Who>,
        cwd: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.handle().load_session_for(session, who, cwd).await
    }

    /// Re-attach a session id to the durable conversation for a directory and
    /// have the agent replay it. See [`EngineHandle::load_session`].
    ///
    /// The replayed updates are waiting on [`Engine::next_update`] when this
    /// returns.
    pub async fn load_session(
        &self,
        session: &SessionId,
        cwd: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.handle().load_session(session, cwd).await
    }

    /// Run one prompt turn on the session, collecting what the agent said.
    ///
    /// The `(session, update)` stream is drained into the result, so a caller
    /// gets the words and the way the turn ended together. That is the whole
    /// renderable unit a chat view needs.
    ///
    /// Any approval the agent asks for during the turn is answered
    /// `Cancelled` (fail closed): this convenience API has nobody to ask.
    /// The shell uses [`Engine::prompt_start`] and answers [`PendingPermission`]s
    /// as they arrive instead.
    pub async fn prompt(
        &mut self,
        session: &SessionId,
        text: impl Into<String>,
    ) -> anyhow::Result<(StopReason, Vec<SessionUpdate>)> {
        let turn = self.prompt_start(session, text).await?;
        let stop = turn
            .await
            .map_err(|_| anyhow::anyhow!("openbot acp is gone"))?
            .map_err(anyhow::Error::msg)?;
        let mut said = Vec::new();
        while let Some((sid, update)) = self.next_update() {
            if sid == *session {
                said.push(update);
            }
        }
        while let Some(mut pending) = self.next_permission() {
            // Refusing on the way out; delivery does not matter. Whether this
            // lands or the request had already settled, the outcome is the
            // same refusal, and nobody is told a decision was made.
            let _settled = pending.answer(RequestPermissionOutcome::Cancelled);
        }
        Ok((stop, said))
    }

    /// Start a prompt turn, returning the receiver its stop reason lands on.
    ///
    /// The shell loops over [`Engine::next_update`] and
    /// [`Engine::next_permission`] until this fires, answering dialogs as the
    /// agent asks for them. A chat whose turn may pause on an approval must
    /// not be stuck behind one `&mut self` call.
    pub async fn prompt_start(
        &self,
        session: &SessionId,
        text: impl Into<String>,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Result<StopReason, String>>> {
        self.handle().prompt_start(session, text)
    }

    /// The same, carrying files the person attached.
    pub async fn prompt_start_with(
        &self,
        session: &SessionId,
        text: impl Into<String>,
        attached: Vec<String>,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Result<StopReason, String>>> {
        self.handle().prompt_start_with(session, text, attached)
    }

    /// The next session update the agent sent, if one is waiting.
    pub fn next_update(&mut self) -> Option<(SessionId, SessionUpdate)> {
        self.updates.try_recv().ok()
    }

    /// The next approval the agent is waiting on, if one is waiting.
    pub fn next_permission(&mut self) -> Option<PendingPermission> {
        self.permissions.try_recv().ok()
    }

    /// Ask the agent to stop the current turn, cooperatively.
    ///
    /// The agent ends the turn at its next boundary and answers the
    /// outstanding `session/prompt` with `cancelled`; the client always gets
    /// its response, and cancelling only changes which one. Any approval the
    /// agent was still waiting on is answered `Cancelled` first, as the
    /// protocol expects of a client that gives up.
    pub async fn cancel(&mut self, session: &SessionId) -> anyhow::Result<()> {
        while let Some(mut pending) = self.next_permission() {
            // As in `prompt`: a refusal that finds the request already
            // refused changes nothing.
            let _settled = pending.answer(RequestPermissionOutcome::Cancelled);
        }
        self.handle().cancel(session).await
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parked() -> (PendingPermission, tokio::sync::oneshot::Receiver<Answer>) {
        parked_kind(None)
    }

    /// A parked request that is, or is not, asking for a credential.
    fn parked_kind(
        secret: Option<SecretAsk>,
    ) -> (PendingPermission, tokio::sync::oneshot::Receiver<Answer>) {
        let (answer, rx) = tokio::sync::oneshot::channel();
        let ask = PendingPermission {
            session: SessionId::new("s1"),
            tool_call: ToolCallUpdate::new(
                agent_client_protocol::schema::v1::ToolCallId::new("c1"),
                agent_client_protocol::schema::v1::ToolCallUpdateFields::default(),
            ),
            options: vec![],
            secret,
            answer: Some(answer),
        };
        (ask, rx)
    }

    fn asking() -> (PendingPermission, tokio::sync::oneshot::Receiver<Answer>) {
        parked_kind(Some(SecretAsk {
            name: "linear-token".into(),
            why: "to file the issue".into(),
        }))
    }

    fn meta_of(v: serde_json::Value) -> Meta {
        let mut m = Meta::new();
        m.insert(SECRET_META.to_owned(), v);
        m
    }

    /// A decision that reaches nobody must be reported as undelivered.
    ///
    /// The request can settle without the person's answer in more than one
    /// way (`PERMISSION_ANSWER_TIMEOUT` expiring, the turn ending, the
    /// connection going away), and every one of them leaves this side holding
    /// a sender whose receiver is gone. Ignoring the failed `send` would let
    /// a stale dialog close as though its answer had taken effect.
    #[test]
    fn an_answer_nobody_is_listening_for_is_reported_as_undelivered() {
        let (mut ask, rx) = parked();
        drop(rx); // the timeout fired, or the turn ended
        assert!(
            !ask.answer(RequestPermissionOutcome::Cancelled),
            "a decision sent into a closed channel was reported as delivered"
        );
    }

    #[test]
    fn an_answer_the_turn_is_waiting_for_is_delivered() {
        let (mut ask, mut rx) = parked();
        assert!(
            ask.answer(RequestPermissionOutcome::Cancelled),
            "a live request refused the decision meant for it"
        );
        assert!(rx.try_recv().is_ok(), "the outcome never arrived");
    }

    /// Answering twice is a no-op rather than a panic (a shell may hold the
    /// entry in a map), and the second answer is not a delivery.
    #[test]
    fn a_second_answer_is_not_reported_as_delivered() {
        let (mut ask, _rx) = parked();
        assert!(ask.answer(RequestPermissionOutcome::Cancelled));
        assert!(
            !ask.answer(RequestPermissionOutcome::Cancelled),
            "answering an already-answered request claimed to have delivered it"
        );
    }

    /// This is the first error a person sees after installing, so it must
    /// name the missing file and a next step rather than report a protocol
    /// failure.
    #[test]
    fn a_missing_binary_says_what_is_missing_and_what_to_do() {
        let err = found(Path::new("definitely-not-a-real-binary-9f2a"))
            .expect_err("a name that is not on PATH should not look findable");
        let text = format!("{err:#}");
        assert!(
            text.contains("PATH"),
            "the error should say where it looked: {text}"
        );
        assert!(
            text.contains("cargo install") || text.contains("point the openbot binary"),
            "the error should say what to do: {text}"
        );
        assert!(
            !text.contains("handshake"),
            "a missing file is not a protocol problem: {text}"
        );
    }

    /// A path the person typed or picked is checked as a path, and the error
    /// quotes it back; "no openbot binary at C:\wrong\place" is actionable in a
    /// way that "not on PATH" would not be.
    #[test]
    fn a_wrong_path_is_named_rather_than_blamed_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("openbot.exe");
        let err = found(&missing).expect_err("nothing is there");
        let text = format!("{err:#}");
        assert!(
            text.contains(&missing.display().to_string()),
            "the error should quote the path it tried: {text}"
        );
        assert!(
            !text.contains("PATH"),
            "an explicit path was given, so PATH is irrelevant: {text}"
        );
    }

    /// A real file is found, whatever it is called.
    #[test]
    fn a_binary_that_is_there_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("openbot.exe");
        std::fs::write(&exe, b"not really a binary").unwrap();
        found(&exe).expect("a file that exists should be found");
    }

    /// A directory is not a binary; reporting it as found would only move
    /// the failure to spawn time.
    #[test]
    fn a_directory_is_not_a_binary() {
        let dir = tempfile::tempdir().unwrap();
        assert!(found(dir.path()).is_err());
    }

    // Credential requests.

    /// An incomplete marker is an ordinary approval, not a broken credential
    /// prompt.
    ///
    /// The name and the reason are what make handing over a secret
    /// answerable. A dialog that showed an input with neither would be asking
    /// for a credential for an unnamed purpose.
    #[test]
    fn only_a_complete_marker_is_a_credential_request() {
        assert_eq!(SecretAsk::from_meta(None), None, "no _meta at all");
        assert_eq!(
            SecretAsk::from_meta(Some(&Meta::new())),
            None,
            "an empty _meta"
        );
        let mut other = Meta::new();
        other.insert(
            "someone/else".into(),
            serde_json::json!({"name":"x","why":"y"}),
        );
        assert_eq!(
            SecretAsk::from_meta(Some(&other)),
            None,
            "another extension's key"
        );
        for (what, v) in [
            ("no name", serde_json::json!({ "why": "y" })),
            ("no why", serde_json::json!({ "name": "x" })),
            (
                "a name that is not a string",
                serde_json::json!({ "name": 1, "why": "y" }),
            ),
            ("not an object", serde_json::json!("nope")),
        ] {
            assert_eq!(SecretAsk::from_meta(Some(&meta_of(v))), None, "{what}");
        }
        assert_eq!(
            SecretAsk::from_meta(Some(&meta_of(
                serde_json::json!({ "name": "linear-token", "why": "to file the issue" })
            ))),
            Some(SecretAsk {
                name: "linear-token".into(),
                why: "to file the issue".into(),
            })
        );
    }

    /// A supplied credential reaches the agent under the agreed key.
    #[test]
    fn supplying_sends_the_value_as_the_provide_option() {
        let (mut ask, mut rx) = asking();
        assert_eq!(
            ask.secret_request().map(|s| s.name.as_str()),
            Some("linear-token")
        );
        assert!(
            ask.supply(
                "sk-live-abc
"
            ),
            "the value did not reach the agent"
        );

        let (outcome, meta) = rx.try_recv().expect("an answer");
        match outcome {
            RequestPermissionOutcome::Selected(s) => assert_eq!(&*s.option_id.0, PROVIDE),
            other => panic!("not a selection: {other:?}"),
        }
        let meta = meta.expect("the value travels in _meta");
        assert_eq!(
            meta.get(SECRET_META)
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_str()),
            // The trailing newline a text field adds is stripped; nothing
            // else is.
            Some("sk-live-abc")
        );
    }

    /// An empty answer is a refusal. Storing an empty credential under a real
    /// name is worse than storing none: it fails against a service later with
    /// nothing on this side to explain why.
    #[test]
    fn an_empty_answer_supplies_nothing_and_leaves_the_request_open() {
        for empty in [
            "", "   ", "
", "  
",
        ] {
            let (mut ask, mut rx) = asking();
            assert!(!ask.supply(empty), "{empty:?} was accepted as a credential");
            assert!(
                rx.try_recv().is_err(),
                "{empty:?} answered the request anyway"
            );
            // Still answerable, so a person who pressed Enter by accident can
            // decline properly rather than being left with a dead dialog.
            assert!(ask.answer(RequestPermissionOutcome::Cancelled));
        }
    }

    /// An ordinary approval cannot be answered with a credential.
    #[test]
    fn an_approval_that_asked_for_nothing_refuses_to_take_a_secret() {
        let (mut ask, mut rx) = parked();
        assert!(ask.secret_request().is_none());
        assert!(
            !ask.supply("sk-live-abc"),
            "a credential was attached to an ordinary approval"
        );
        assert!(rx.try_recv().is_err(), "it answered the request anyway");
    }

    /// Supplying twice is a no-op, like answering twice.
    #[test]
    fn a_second_supply_goes_nowhere() {
        let (mut ask, _rx) = asking();
        assert!(ask.supply("sk-live-abc"));
        assert!(
            !ask.supply("sk-live-other"),
            "the second value was sent too"
        );
    }
}
