//! `botroster acp`: BOTROSTER as an Agent Client Protocol agent, over stdio.
//!
//! The shape of this file is dictated by one property of the SDK: handler
//! callbacks run on the event loop, and the connection cannot read new
//! messages while one is running. A `session/prompt` handler that awaited the
//! turn inline would hold the loop; the turn would ask the client for
//! approval, the client would answer, and that answer could never be read,
//! because the task waiting for it is the task blocking the reader. It would
//! hang until the hub's approval timeout denied the call. So the prompt
//! handler spawns and returns, and the `Responder` (which takes `self` by
//! value) goes with it to answer later.
//!
//! Sessions map to Bots by working directory. ACP supplies a `cwd` per
//! session; BOTROSTER's unit of memory is a named Bot. Binding them means one
//! Bot per project, and reopening a project resumes the same Bot's
//! conversation rather than starting a new one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1;
use botroster_agent::AgentEvent;
use botroster_proto::approval::{ApprovalDecision, ApprovalRequestParams, SecretRequestParams};
use tokio::sync::mpsc;

/// Everything `botroster acp` needs that an ACP client cannot tell it.
///
/// An editor spawns this as a bare subprocess, so these come from flags and
/// the environment: the same `--hub`, `--server` and `BOTROSTER_HOME` every
/// other command reads.
pub struct Config {
    pub hub: String,
    pub server: String,
    pub home: PathBuf,
    pub model_opts: crate::config::ModelOverrides,
    pub demo: bool,
    /// `--demo-tools`: serve without a model, but play the tool script (write,
    /// read back, list, shell) so a client can see tool calls and approvals
    /// without a key or any tokens.
    pub demo_tools: bool,
    /// `--demo-secret`: serve without a model and ask once for a credential,
    /// so a client's credential prompt can be driven without one.
    pub demo_secret: bool,
    /// Pin every session to one Bot instead of deriving it from the cwd.
    pub bot: Option<String>,
}

/// What a live ACP session is attached to on BOTROSTER's side.
///
/// A Bot session is one Bot's conversation. A group session is a thread
/// several Bots answer in, and which one answers is decided per message by
/// who was `@mentioned`, so the session binds to the group and the owner is
/// resolved at each turn, not once at the start.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Bot(botroster_bots::BotId),
    Group(botroster_bots::BotId),
}

struct Session {
    target: Target,
    /// Present while a turn is running: where a prompt sent *during* it goes,
    /// and how a caller learns that turn ended.
    ///
    /// A direct message can redirect the current turn. BOTROSTER keeps one turn
    /// at a time per session (two turns answering one conversation is what the
    /// lock prevents), so a second prompt does not start a turn; it joins the
    /// one running and is answered by it.
    running: Option<Running>,
    /// Present while a turn is running, so `session/cancel` has something to
    /// flip. `None` between turns: cancelling an idle session is not an error,
    /// it is simply nothing to do.
    cancel: Option<tokio::sync::watch::Sender<bool>>,
}

/// The turn in flight on a session.
struct Running {
    redirects: botroster_agent::agent::Redirects,
    /// Flipped when the turn ends, so a joined prompt knows what to answer.
    /// `Ok(stop)` when the turn ended having heard everything sent to it;
    /// `Err(why)` when it ended with something still queued.
    done: tokio::sync::watch::Sender<Option<Result<v1::StopReason, String>>>,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

/// Take the session for one turn, or say why not.
///
/// One turn at a time per session. The check and the claim happen under a
/// single lock so two prompts arriving together cannot both win.
///
/// A second concurrent turn would overwrite the first's cancel sender, leaving
/// the first running and unstoppable, and on finishing would clear the
/// second's, so neither could be cancelled. Both would also read the Bot's
/// history before either appends, so the second would answer a conversation
/// it cannot see.
///
/// This is not the same as refusing the second message. A prompt that arrives
/// mid-turn joins the running turn as a redirect; see [`join_turn`], which is
/// tried first. This runs only when there is no turn to join, and what it
/// rejects is a second turn, never a second message.
///
/// Returns the Bot to run as, and the receiver the turn watches for a stop.
fn claim_turn(sessions: &Sessions, sid: &str) -> Result<Claim, String> {
    let mut map = sessions.lock().expect("sessions lock");
    let Some(s) = map.get_mut(sid) else {
        return Err(format!("no such session: {sid}"));
    };
    if s.cancel.is_some() {
        return Err(format!(
            "session {sid} is already running a turn — cancel it with `session/cancel` before sending another prompt"
        ));
    }
    let (stop, stopped) = tokio::sync::watch::channel(false);
    s.cancel = Some(stop);
    let redirects = botroster_agent::agent::Redirects::new();
    let (done, _) = tokio::sync::watch::channel(None);
    s.running = Some(Running {
        redirects: redirects.clone(),
        done,
    });
    Ok(Claim {
        target: s.target.clone(),
        cancel: stopped,
        redirects,
    })
}

/// Hand a prompt to the turn already running, and say how to wait for it.
///
/// `None` means nothing is running, so the caller should start a turn.
fn join_turn(sessions: &Sessions, sid: &str, text: &str) -> Option<JoinedTurn> {
    let map = sessions.lock().expect("sessions lock");
    let running = map.get(sid)?.running.as_ref()?;
    running.redirects.send(text);
    Some(JoinedTurn {
        done: running.done.subscribe(),
    })
}

/// A session taken for one turn: who runs it, how it is stopped, and where
/// something said during it arrives.
///
/// Grouped so `one_turn` does not take these as loose positional parameters.
#[derive(Debug)]
struct Claim {
    target: Target,
    cancel: tokio::sync::watch::Receiver<bool>,
    redirects: botroster_agent::agent::Redirects,
}

/// A prompt that joined a turn already in flight.
struct JoinedTurn {
    done: tokio::sync::watch::Receiver<Option<Result<v1::StopReason, String>>>,
}

/// Point a session id at a Bot, unless a turn is already running on it.
///
/// `session/new` mints a fresh id so it can never collide. `session/load`
/// takes an id from the client, which may be one this process is already
/// using, and a bare `insert` there would replace the entry and drop the
/// running turn's cancel sender. Cancellation would go dead, and
/// [`claim_turn`] would then hand the same session to a second turn, which is
/// what it exists to prevent.
///
/// Refusing is also the correct semantic: replaying a stored transcript
/// underneath a turn that is still speaking would interleave history with live
/// updates, and the client has no way to tell which is which.
fn bind_session(sessions: &Sessions, sid: &str, target: Target) -> Result<(), String> {
    let mut map = sessions.lock().expect("sessions lock");
    if let Some(existing) = map.get(sid) {
        if existing.cancel.is_some() {
            return Err(format!(
                "session {sid} is running a turn — cancel it with `session/cancel` before loading a conversation into it"
            ));
        }
    }
    map.insert(
        sid.to_owned(),
        Session {
            target,
            running: None,
            cancel: None,
        },
    );
    Ok(())
}

/// Give the session back when the turn ends.
///
/// A stale sender left behind would make the next turn start already
/// cancelled, and would keep the session looking busy forever.
fn release_turn(sessions: &Sessions, sid: &str, stop: v1::StopReason) {
    if let Some(s) = sessions.lock().expect("sessions lock").get_mut(sid) {
        s.cancel = None;
        if let Some(running) = s.running.take() {
            // Anything still queued never reached the turn. Redirects are
            // picked up at the top of a step, so a message sent during the
            // final model call is still here now. The sender is told so rather
            // than being handed this turn's stop reason as though the Bot had
            // heard the message.
            let left = running.redirects.undelivered();
            let answer = if left.is_empty() {
                Ok(stop)
            } else {
                Err(format!(
                    "the turn ended before this reached it, so the Bot never saw it — send it again: {}",
                    left.join(" / ")
                ))
            };
            let _ = running.done.send(Some(answer));
        }
    }
}

/// The Bot a working directory belongs to.
///
/// A directory named `payments-api` becomes a Bot called `payments-api`. The
/// mapping is stable, so the same project always reaches the same Bot and its
/// accumulated conversation.
fn bot_name_for(cwd: &Path) -> String {
    let raw = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_lowercase();
    if trimmed.is_empty() {
        // A cwd of `/` or a drive root has no name to borrow.
        "workspace".to_owned()
    } else {
        trimmed
    }
}

/// Everything the person sent, as one string, including what BOTROSTER could
/// not read.
///
/// ACP sends a prompt as content blocks and the agent takes a task, so the
/// blocks have to become text. Two kinds are not BOTROSTER's to decline:
///
/// Resource links are baseline. The schema says "All agents MUST support
/// resource links in prompts", and no capability declines them. BOTROSTER cannot
/// inline the resource, but it can hand over the link, which its file and
/// fetch tools can act on under the usual policy, so a link is named as an
/// attachment rather than dropped. It reads as something the person attached,
/// not as an instruction: `name` is client-supplied text and is trusted no
/// further than the rest of the prompt.
///
/// Anything unreadable is reported. Images, audio and embedded context are
/// gated behind capabilities BOTROSTER advertises as `false`, so a well-behaved
/// client never sends them, but one that does is told, and told in the task.
/// That placement matters: the task becomes the user turn in the Bot's
/// history, so the note survives `session/load` and replays with the
/// conversation. A note sent only as a notification would show up live and
/// vanish on reopen, leaving a transcript that reads as though the image had
/// been looked at. It also lets the Bot say what it did not receive instead
/// of guessing at a screenshot it never saw.
fn prompt_text(blocks: &[v1::ContentBlock]) -> Result<Prompt, String> {
    let mut out = Vec::new();
    let mut unread = Vec::new();
    for b in blocks {
        match b {
            v1::ContentBlock::Text(t) => out.push(t.text.clone()),
            v1::ContentBlock::ResourceLink(l) => {
                out.push(format!("[attached: {} — {}]", l.name, l.uri));
            }
            v1::ContentBlock::Image(_) => unread.push("image"),
            v1::ContentBlock::Audio(_) => unread.push("audio clip"),
            // `Resource`, and whatever a later schema adds: named for what it
            // is to the person rather than by its variant.
            //
            // A wildcard, unlike `carried`, which is total precisely so a new
            // event kind cannot be dropped silently. `ContentBlock` is
            // `#[non_exhaustive]`, so there is no total form to write. The
            // fallback is acceptable because an unknown block is still
            // counted and still named to the person, so the failure is
            // degraded rather than silent.
            //
            // The set of variants can only change on a dependency bump:
            // `agent-client-protocol` 2.0.0 depends on
            // `agent-client-protocol-schema` with `=1.5.0`, an exact
            // requirement, so `cargo update` alone cannot introduce one.
            // Bumping `agent-client-protocol` can. When that happens, re-read
            // the new `ContentBlock` for anything the protocol calls
            // MUST-support and give it an arm here.
            _ => unread.push("attached document"),
        }
    }
    if out.is_empty() {
        return Err(if unread.is_empty() {
            "the prompt was empty".to_owned()
        } else {
            format!(
                "the prompt held no text botroster can read — only {}",
                counted(&unread)
            )
        });
    }
    let note = if unread.is_empty() {
        None
    } else {
        let note = format!(
            "[botroster reads text and links. It could not read {}, so anything \
             shown only there is not part of this message.]",
            counted(&unread)
        );
        out.push(note.clone());
        Some(note)
    };
    Ok(Prompt {
        task: out.join("\n\n"),
        note,
    })
}

/// A prompt, ready to run, and the part of it to show live.
///
/// The note is carried separately only so it can be shown live. It is already
/// inside `task`, which becomes the user turn in history and comes back on
/// `session/load`; sending it live as well is what makes the two renderings
/// agree. Without it a person would see the notice only after reopening the
/// conversation, which [`super::replay`] exists to rule out: the live and
/// replayed views must match.
#[derive(Debug)]
struct Prompt {
    /// What the Bot is asked, notice included.
    task: String,
    /// The notice alone, when something could not be read.
    note: Option<String>,
}

/// "1 image", "2 images and 1 audio clip": what was not read, in words.
///
/// Counted and named, because "3 blocks" says nothing about what the Bot
/// missed. Order follows the message, so it matches what was sent.
fn counted(kinds: &[&str]) -> String {
    let mut tally: Vec<(&str, usize)> = Vec::new();
    for kind in kinds {
        match tally.iter_mut().find(|(name, _)| name == kind) {
            Some((_, n)) => *n += 1,
            None => tally.push((kind, 1)),
        }
    }
    tally
        .iter()
        .map(|(name, n)| {
            if *n == 1 {
                format!("1 {name}")
            } else {
                format!("{n} {name}s")
            }
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// The stage a tool is at, if it reported one. `shell.exec` streams these;
/// most tools do not.
fn stage_of(payload: &serde_json::Value) -> Option<&str> {
    payload.get("stage").and_then(serde_json::Value::as_str)
}

/// Whether an event reaches a client, and if not, why not.
///
/// A total function over every `AgentEvent`: adding a variant stops compiling
/// here until somebody decides what a person should see. The transcript is
/// documented as showing tool activity, computer use, created files, questions
/// and approval requests, and that guarantee erodes one event kind at a time
/// behind a `_ => None`.
///
/// Compiled only under `cfg(test)`: it exists to make the decision explicit
/// and to fail the build when a new `AgentEvent` variant arrives without one,
/// and the tests are where that is checked.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum Carried {
    /// Reaches the client as a `session/update`.
    Rendered,
    /// Not sent, for this reason.
    Dropped(&'static str),
}

#[cfg(test)]
fn carried(event: &AgentEvent) -> Carried {
    match event {
        AgentEvent::AssistantText { .. } => Carried::Rendered,
        // The person interrupted, and the transcript has to show it or the
        // Bot's next answer arrives with no visible reason for the change of
        // direction.
        AgentEvent::Redirected { .. } => Carried::Rendered,
        AgentEvent::ToolCallStarted { .. } => Carried::Rendered,
        AgentEvent::ToolCallFinished { .. } => Carried::Rendered,
        AgentEvent::ToolProgress { payload, .. } => match stage_of(payload) {
            Some(_) => Carried::Rendered,
            // Progress payloads are tool-specific. A tool that reports
            // structure nobody has agreed on has nothing a client could put in
            // front of a person, and inventing a rendering would show a shape
            // BOTROSTER made up.
            None => Carried::Dropped("no stage to show"),
        },
        AgentEvent::Started { .. } => {
            Carried::Dropped("`session/prompt` is the start; announcing it again says nothing")
        }
        AgentEvent::Thinking { .. } => {
            Carried::Dropped("a step number and no words; rendering it would invent the thought")
        }
        AgentEvent::Finished { .. } => {
            Carried::Dropped("this is the response to `session/prompt`, not an update about it")
        }
    }
}

fn update_for(event: &AgentEvent) -> Option<v1::SessionUpdate> {
    let chunk =
        |text: String| v1::ContentChunk::new(v1::ContentBlock::Text(v1::TextContent::new(text)));
    match event {
        AgentEvent::AssistantText { text, .. } => {
            Some(v1::SessionUpdate::AgentMessageChunk(chunk(text.clone())))
        }
        // As the person speaking, because they did.
        AgentEvent::Redirected { text } => {
            Some(v1::SessionUpdate::UserMessageChunk(chunk(text.clone())))
        }
        AgentEvent::ToolCallStarted {
            tool,
            call_id,
            args,
            ..
        } => Some(v1::SessionUpdate::ToolCall(
            // The title is what a person reads in the client, so it is the
            // tool's name rather than an id they cannot look up.
            v1::ToolCall::new(
                v1::ToolCallId::new(call_id.as_str().to_owned()),
                tool.clone(),
            )
            .status(v1::ToolCallStatus::InProgress)
            // The arguments travel with it. Without them a client shows
            // "fs.write" and nothing about what; the replayed form of the same
            // call carries them, so leaving them off here would make a
            // conversation render differently live and on reopen.
            .raw_input(args.clone()),
        )),
        // This is the one event that says what the machine is doing while it
        // is doing it.
        AgentEvent::ToolProgress { call_id, payload } => {
            let stage = stage_of(payload)?;
            let fields =
                v1::ToolCallUpdateFields::default().content(vec![v1::ToolCallContent::Content(
                    v1::Content::new(v1::ContentBlock::Text(v1::TextContent::new(
                        stage.to_owned(),
                    ))),
                )]);
            Some(v1::SessionUpdate::ToolCallUpdate(v1::ToolCallUpdate::new(
                v1::ToolCallId::new(call_id.as_str().to_owned()),
                fields,
            )))
        }
        AgentEvent::ToolCallFinished {
            call_id,
            ok,
            output,
            ..
        } => {
            let fields = v1::ToolCallUpdateFields::default()
                .status(if *ok {
                    v1::ToolCallStatus::Completed
                } else {
                    v1::ToolCallStatus::Failed
                })
                // What it produced, structured. A client that wants to show a
                // written file or a command's output has it; one that wants a
                // tick can ignore it. Sending only a status would make created
                // files unreachable by any client.
                .raw_output(output.clone());
            Some(v1::SessionUpdate::ToolCallUpdate(v1::ToolCallUpdate::new(
                v1::ToolCallId::new(call_id.as_str().to_owned()),
                fields,
            )))
        }
        AgentEvent::Started { .. } | AgentEvent::Thinking { .. } | AgentEvent::Finished { .. } => {
            None
        }
    }
}

/// Asks the person, through whatever client is attached.
///
/// This is not where the decision takes effect; the hub enforces (SPEC §6.0).
/// It is how a human's answer gets back, and nothing more.
struct AcpApprover {
    session: String,
    tx: mpsc::UnboundedSender<ClientAsk>,
}

/// One question for the person, handed to the connection task that owns the
/// client. Two kinds, because the answers are different types: an approval
/// comes back as a decision, a credential request comes back as a value or a
/// refusal.
enum ClientAsk {
    Approval {
        session: String,
        req: ApprovalRequestParams,
        reply: tokio::sync::oneshot::Sender<ApprovalDecision>,
    },
    Secret {
        session: String,
        req: SecretRequestParams,
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
}

#[async_trait::async_trait]
impl botroster_agent::ApprovalHandler for AcpApprover {
    async fn decide(&self, req: &ApprovalRequestParams) -> ApprovalDecision {
        let (reply, wait) = tokio::sync::oneshot::channel();
        let ask = ClientAsk::Approval {
            session: self.session.clone(),
            req: req.clone(),
            reply,
        };
        if self.tx.send(ask).is_err() {
            // The connection is gone. Nobody can answer, and a call that
            // cannot be approved is not permitted.
            return ApprovalDecision::deny().with_note("the ACP client disconnected");
        }
        wait.await
            .unwrap_or_else(|_| ApprovalDecision::deny().with_note("no answer from the ACP client"))
    }

    /// Ask the attached client for a credential.
    ///
    /// Without this the default applies (`None`, a refusal) and every ACP
    /// client, the desktop client included, could only time out on a
    /// `secret.request`. That is safe and useless: the Bot then has no way to
    /// get a token except to ask for one in conversation, which is the failure
    /// the broker exists to prevent.
    async fn supply(&self, req: &SecretRequestParams) -> Option<String> {
        let (reply, wait) = tokio::sync::oneshot::channel();
        let ask = ClientAsk::Secret {
            session: self.session.clone(),
            req: req.clone(),
            reply,
        };
        if self.tx.send(ask).is_err() {
            return None;
        }
        // A dropped sender is nobody answering, which is a refusal: the same
        // reading `decide` takes, and the same one the hub takes on timeout.
        wait.await.ok().flatten()
    }

    /// False. `denies_everything` tells the agent loop that retrying is
    /// futile because nobody is there, which is wrong here: a client is
    /// attached and may say yes to the next request.
    fn denies_everything(&self) -> bool {
        false
    }
}

/// Put one approval in front of the person and wait for the answer.
async fn ask_client(
    conn: &agent_client_protocol::ConnectionTo<agent_client_protocol::Client>,
    session: &str,
    req: &ApprovalRequestParams,
) -> ApprovalDecision {
    // The tool call is what the person is judging, so it carries the tool's
    // name and the hub's stated reason rather than an opaque id.
    let fields = v1::ToolCallUpdateFields::default()
        .title(format!("{}: {}", req.tool_id.as_str(), req.reason))
        .raw_input(req.args.clone());
    let ask = v1::RequestPermissionRequest::new(
        v1::SessionId::new(session.to_owned()),
        v1::ToolCallUpdate::new(v1::ToolCallId::new(req.call_id.as_str().to_owned()), fields),
        super::permission_options(),
    );

    // `block_task`, as the SDK's own client example does: this await is a task
    // that is legitimately parked waiting on the other side, and the runtime
    // needs to know that rather than treating it as work in progress.
    match conn.send_request(ask).block_task().await {
        Ok(res) => {
            match res.outcome {
                v1::RequestPermissionOutcome::Selected(s) => super::decision(&s.option_id.0)
                    .unwrap_or_else(|| {
                        // An id that was never offered. Not a decision, and not
                        // guessed at; see `super::decision`.
                        ApprovalDecision::deny().with_note(
                            "the ACP client answered with an option botroster did not offer",
                        )
                    }),
                // The client withdrew the question. Nothing was permitted.
                v1::RequestPermissionOutcome::Cancelled => ApprovalDecision::deny()
                    .with_note("the approval was cancelled by the ACP client"),
                _ => ApprovalDecision::deny()
                    .with_note("an approval outcome botroster does not understand"),
            }
        }
        Err(e) => ApprovalDecision::deny().with_note(format!("asking the ACP client failed: {e}")),
    }
}

use botroster_proto::approval::{SECRET_META, SECRET_PROVIDE as PROVIDE};

/// Ask the person for a credential, through whatever client is attached.
///
/// The value must not appear anywhere a client renders as conversation. The
/// title and `raw_input` here carry the credential's name and the Bot's stated
/// reason and nothing else: a `ToolCallUpdate` is drawn in the transcript,
/// which is the one place the value must never be.
///
/// Fails closed at every step, because each one is a place a client can be
/// old, wrong or hostile: an outcome that is not `Selected`, an option id
/// BOTROSTER never offered, a missing `_meta`, a `_meta` holding something that
/// is not a string, or an empty string, are all "no credential". A client that
/// does not implement this extension sees two ordinary options and cannot
/// accidentally succeed; picking "provide" without sending a value is a
/// refusal, not an empty credential.
async fn ask_client_for_secret(
    conn: &agent_client_protocol::ConnectionTo<agent_client_protocol::Client>,
    session: &str,
    req: &SecretRequestParams,
) -> Option<String> {
    let mut meta = serde_json::Map::new();
    meta.insert(
        SECRET_META.to_owned(),
        serde_json::json!({ "name": req.name, "why": req.why }),
    );

    let fields = v1::ToolCallUpdateFields::default()
        .title(format!("credential needed: {}", req.name))
        .raw_input(serde_json::json!({ "name": req.name, "why": req.why }));
    let ask = v1::RequestPermissionRequest::new(
        v1::SessionId::new(session.to_owned()),
        v1::ToolCallUpdate::new(v1::ToolCallId::new(format!("secret-{}", req.name)), fields),
        vec![
            super::option(
                PROVIDE,
                "Provide credential",
                v1::PermissionOptionKind::AllowOnce,
            ),
            super::option(
                "reject-once",
                "Not this time",
                v1::PermissionOptionKind::RejectOnce,
            ),
        ],
    )
    .meta(meta);

    let res = conn.send_request(ask).block_task().await.ok()?;
    supplied_value(res)
}

/// Read the credential out of a client's answer, or refuse.
///
/// Separated from the request because every branch here is a way a client can
/// be old, wrong or hostile, and a decision written inline next to an `await`
/// cannot be tested; `typed_secret` in `approve.rs` is separate for the same
/// reason.
///
/// Takes the response by value and returns an owned `String`. `Meta` is
/// `serde_json::Map`, a type this crate does not own and so cannot give a
/// redacting `Debug` to; anything that formats one prints the credential.
/// Consuming the response means there is no response left for a caller to
/// log.
fn supplied_value(res: v1::RequestPermissionResponse) -> Option<String> {
    let v1::RequestPermissionOutcome::Selected(s) = res.outcome else {
        return None;
    };
    if &*s.option_id.0 != PROVIDE {
        return None;
    }
    let typed = res
        .meta?
        .get(SECRET_META)?
        .get("value")?
        .as_str()?
        .to_owned();
    // Same rule as the terminal prompt: trailing whitespace goes and nothing
    // else, and an empty answer is a refusal rather than an empty credential.
    let typed = typed.trim_end().to_owned();
    (!typed.trim().is_empty()).then_some(typed)
}

/// How many messages a `session/load` replays.
///
/// One notification per message, so this is the number that decides how long
/// opening a conversation takes. 500 is far more than anybody scrolls back
/// through and keeps the open fast; the rest stays on disk and is found by
/// search.
const REPLAY_LIMIT: usize = 500;

/// Said first when a load did not carry the whole conversation.
///
/// A transcript that starts partway through with no explanation reads as the
/// whole conversation, and a reader would conclude the Bot has forgotten
/// things it has not. As with unreadable blocks in `prompt_text`: say what is
/// missing, in the transcript, where it stays.
fn replay_notice(total: usize, shown: usize) -> Option<v1::SessionUpdate> {
    if total <= shown {
        return None;
    }
    let earlier = total - shown;
    Some(v1::SessionUpdate::UserMessageChunk(v1::ContentChunk::new(
        v1::ContentBlock::Text(v1::TextContent::new(format!(
            concat!(
                "[{} earlier messages are not shown. ",
                "They are still on disk — search finds them.]"
            ),
            earlier
        ))),
    )))
}

/// Serve ACP on stdin/stdout until the client goes away.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    // Fail here rather than at the first prompt. An editor that connects
    // successfully and then errors on every message looks like a broken agent;
    // an `initialize` that refuses with a reason is a fixable misconfiguration.
    let bots = botroster_bots::BotStore::open(&cfg.home)
        .map_err(|e| anyhow::anyhow!("cannot open the Bot store at {}: {e}", cfg.home.display()))?;
    if !cfg.demo && !cfg.demo_tools && !cfg.demo_secret {
        crate::config::build(&cfg.home, &cfg.model_opts, false, "botroster acp")
            .map_err(|e| anyhow::anyhow!("no usable model: {e}"))?;
    }

    let cfg = Arc::new(cfg);
    let bots = Arc::new(bots);
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    let new_cfg = Arc::clone(&cfg);
    let new_bots = Arc::clone(&bots);
    let new_sessions = Arc::clone(&sessions);
    let load_sessions = Arc::clone(&sessions);
    let load_bots = Arc::clone(&bots);
    let load_cfg = Arc::clone(&cfg);

    let run_cfg = Arc::clone(&cfg);
    let run_sessions = Arc::clone(&sessions);
    let run_bots = Arc::clone(&bots);
    let cancel_sessions = Arc::clone(&sessions);

    agent_client_protocol::Agent
        .builder()
        .name("botroster")
        .on_receive_request(
            async move |req: v1::InitializeRequest, responder, _conn| {
                responder.respond(
                    v1::InitializeResponse::new(req.protocol_version)
                        // Everything left false is a promise not to ask for it.
                        // `fs` and `terminal` are the client's to offer and
                        // BOTROSTER's to never call; see SPEC §9.
                        //
                        // `load_session` is the exception, and BOTROSTER needs it
                        // more than most agents do: a Bot's conversation is the
                        // durable thing here and an ACP session is an ephemeral
                        // name for it, so without a replay every client shows an
                        // empty transcript beside a Bot that remembers
                        // everything.
                        .agent_capabilities(v1::AgentCapabilities::new().load_session(true)),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: v1::NewSessionRequest, responder, _conn| {
                let target = match resolve_target(&new_bots, &new_cfg, req.meta.as_ref(), &req.cwd)
                {
                    Ok(t) => t,
                    Err(e) => return responder.respond_with_internal_error(e),
                };
                let id = format!("botroster-{}", uuid::Uuid::new_v4());
                new_sessions.lock().expect("sessions lock").insert(
                    id.clone(),
                    Session {
                        target,
                        running: None,
                        cancel: None,
                    },
                );
                responder.respond(v1::NewSessionResponse::new(v1::SessionId::new(id)))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: v1::LoadSessionRequest, responder, conn| {
                // Resolve from the cwd, not from the session id. The id is the
                // client's name for the conversation and does not survive a
                // restart of this process; the Bot for a directory does. So a
                // load re-attaches whatever name the client is holding to the
                // durable conversation. Same precedence as `session/new`: a
                // client that opened a Bot or a group by name must be able to
                // reopen the same one.
                let target =
                    match resolve_target(&load_bots, &load_cfg, req.meta.as_ref(), &req.cwd) {
                        Ok(t) => t,
                        Err(e) => return responder.respond_with_internal_error(e),
                    };
                // Bounded, and the client is told when the bound applies. A
                // load replays one notification per message, so an unbounded
                // one is linear in the whole history, and the cost is not the
                // disk read but thousands of separately framed JSON-RPC
                // messages down a pipe.
                //
                // Not silent. `replay_notice` goes first and says what was
                // left out and where to find it, because a transcript that
                // quietly starts partway through reads as the whole
                // conversation.
                let history = match &target {
                    Target::Bot(b) => load_bots.history(b, Some(REPLAY_LIMIT)),
                    // The group's thread, which is the whole reason to open
                    // one: the handoffs between members live there and in no
                    // member's own log.
                    Target::Group(g) => load_bots.group_history(g, Some(REPLAY_LIMIT)),
                };
                let history = match history {
                    Ok(h) => h,
                    Err(e) => {
                        return responder.respond_with_internal_error(format!(
                            "cannot read the conversation: {e}"
                        ))
                    }
                };
                let total = match &target {
                    Target::Bot(b) => load_bots.message_count(b).unwrap_or(history.len()),
                    Target::Group(g) => load_bots.group_message_count(g).unwrap_or(history.len()),
                };
                let id = req.session_id.clone();
                if let Err(e) = bind_session(&load_sessions, &id.0, target) {
                    return responder.respond_with_internal_error(e);
                }
                // Replay before responding. A client is entitled to treat the
                // response as "the transcript is now on screen"; sending the
                // updates afterwards would race its first render.
                for update in replay_notice(total, history.len())
                    .into_iter()
                    .chain(super::replay(&history))
                {
                    if let Err(e) =
                        conn.send_notification(v1::SessionNotification::new(id.clone(), update))
                    {
                        return responder.respond_with_internal_error(format!(
                            "the conversation could not be replayed: {e}"
                        ));
                    }
                }
                responder.respond(v1::LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: v1::PromptRequest, responder, conn| {
                let sid = req.session_id.0.to_string();
                let Prompt { task, note } = match prompt_text(&req.prompt) {
                    Ok(p) => p,
                    Err(e) => return responder.respond_with_internal_error(e),
                };
                // A prompt sent while a turn is running joins it rather than
                // being refused or starting a second one. A direct message can
                // redirect the current turn; BOTROSTER keeps one turn per
                // session, so the message goes to that turn and this request
                // is answered by it when it ends.
                if let Some(joined) = join_turn(&run_sessions, &sid, &task) {
                    return conn.spawn(async move {
                        let mut done = joined.done;
                        let answer = loop {
                            if let Some(answer) = done.borrow_and_update().clone() {
                                break answer;
                            }
                            if done.changed().await.is_err() {
                                // The session went away with the turn still
                                // running. `cancelled` is the accurate answer:
                                // it did not finish.
                                break Ok(v1::StopReason::Cancelled);
                            }
                        };
                        let _ = match answer {
                            Ok(stop) => responder.respond(v1::PromptResponse::new(stop)),
                            Err(why) => responder.respond_with_internal_error(why),
                        };
                        Ok(())
                    });
                }

                let claim = match claim_turn(&run_sessions, &sid) {
                    Ok(claim) => claim,
                    Err(e) => return responder.respond_with_internal_error(e),
                };

                // Only on this path, and only after the turn is claimed.
                //
                // The notice is already inside `task`, so on the join path
                // above the agent echoes it back as part of the redirect and
                // sending it here too would render it twice (once alone, once
                // bundled with the person's own words). A turn that starts
                // here never echoes the prompt that started it, so this is the
                // one place the notice has no other way to reach the screen.
                // Below `claim_turn` for the same reason: a prompt for a
                // session that does not exist should answer with the error
                // and nothing else.
                if let Some(note) = note {
                    let _ = conn.send_notification(v1::SessionNotification::new(
                        req.session_id.clone(),
                        v1::SessionUpdate::UserMessageChunk(v1::ContentChunk::new(
                            v1::ContentBlock::Text(v1::TextContent::new(note)),
                        )),
                    ));
                }

                let cfg = Arc::clone(&run_cfg);
                let bots = Arc::clone(&run_bots);
                let spawn_conn = conn.clone();
                let done_sessions = Arc::clone(&run_sessions);

                // Off the event loop. Awaiting the turn here would stop the
                // connection reading the very approval answer the turn waits
                // for; see the module docs.
                conn.spawn(async move {
                    let out = one_turn(&cfg, &bots, &claim, &sid, &task, spawn_conn).await;
                    let stop = match &out {
                        Ok(v1::PromptResponse { stop_reason, .. }) => *stop_reason,
                        // A turn that failed did not end for any of the
                        // protocol's reasons; whoever joined it is told it was
                        // cancelled rather than that it completed.
                        Err(_) => v1::StopReason::Cancelled,
                    };
                    release_turn(&done_sessions, &sid, stop);
                    let _ = match out {
                        Ok(v1::PromptResponse { stop_reason, .. }) => {
                            responder.respond(v1::PromptResponse::new(stop_reason))
                        }
                        Err(message) => responder.respond_with_internal_error(message),
                    };
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |note: v1::CancelNotification, _conn| {
                // Cooperative, as ACP specifies. This asks; the turn ends at
                // its next boundary (or immediately, if it is waiting on the
                // model) and answers the outstanding `session/prompt` with
                // `cancelled`. The client always gets its response; cancelling
                // only changes which one. An unhandled notification would
                // leave the Bot spending tokens and touching the computer
                // after the person pressed stop.
                let sid = note.session_id.0.to_string();
                if let Some(s) = cancel_sessions.lock().expect("sessions lock").get(&sid) {
                    if let Some(stop) = &s.cancel {
                        let _ = stop.send(true);
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(agent_client_protocol::Stdio::new())
        .await
        .map_err(|e| anyhow::anyhow!("the ACP connection ended: {e}"))
}

/// The Bot for this directory, creating it the first time.
fn existing_or_new(
    bots: &botroster_bots::BotStore,
    name: &str,
    cwd: &Path,
) -> Result<botroster_bots::Bot, String> {
    if let Ok(list) = bots.list(true) {
        if let Some(found) = list.into_iter().find(|b| b.name == name) {
            return Ok(found);
        }
    }
    // No title. Repeating the name would read as "Bug Repro / Bug Repro" in
    // a sidebar and tell `botroster bot ls` nothing; a field repeating its
    // neighbour is worse than an empty one. The flow is create, then edit the
    // profile, and an empty title is the correct starting state.
    bots.create(
        name,
        "",
        &format!("Works on {}. Created by an ACP client.", cwd.display()),
    )
    .map_err(|e| format!("cannot create a Bot named `{name}`: {e}"))
}

/// What a session should answer in, from what the client asked for.
///
/// Precedence, highest first, each with a reason:
///
/// 1. `--bot`: an operator pinned this process and no client may talk past
///    it. It wins over a group too.
/// 2. A named Bot: a roster picking a Bot.
/// 3. A named group: a roster picking a thread. Below the Bot, because a
///    client sending both is asking for something that does not exist and the
///    Bot is the narrower request.
/// 4. The working directory: what an editor means.
///
/// A group must already exist. `session/new` creates a Bot for a directory
/// nobody has used, which is right for a Bot and wrong for a group: groups
/// have members, and inventing an empty one produces a thread with nobody in
/// it to answer.
fn resolve_target(
    bots: &botroster_bots::BotStore,
    cfg: &Config,
    meta: Option<&v1::Meta>,
    cwd: &Path,
) -> Result<Target, String> {
    if cfg.bot.is_none() {
        if let Some(name) = super::requested_bot(meta) {
            return Ok(Target::Bot(existing_or_new(bots, &name, cwd)?.id));
        }
        if let Some(name) = super::requested_group(meta) {
            let g = bots
                .resolve_group(&name)
                .map_err(|e| format!("no group `{name}`: {e}"))?;
            return Ok(Target::Group(g.id));
        }
    }
    let name = cfg.bot.clone().unwrap_or_else(|| bot_name_for(cwd));
    Ok(Target::Bot(existing_or_new(bots, &name, cwd)?.id))
}

/// One prompt turn: run it, stream it, and say how it ended.
async fn one_turn(
    cfg: &Config,
    bots: &botroster_bots::BotStore,
    claim: &Claim,
    session: &str,
    task: &str,
    conn: agent_client_protocol::ConnectionTo<agent_client_protocol::Client>,
) -> Result<v1::PromptResponse, String> {
    let target = &claim.target;
    // Which Bot answers, and which conversation this turn belongs to. For a
    // group both are decided here, per message, because who answers depends
    // on who was `@mentioned` in it.
    let (bot, thread) = match target {
        Target::Bot(id) => (bots.get(id).map_err(|e| e.to_string())?, crate::Thread::Own),
        Target::Group(id) => {
            let g = bots.get_group(id).map_err(|e| e.to_string())?;
            let owner = g.owner_for(task).cloned().ok_or_else(|| {
                format!(
                    "you mentioned somebody who is not in {} — members are {}",
                    g.id,
                    g.members
                        .iter()
                        .map(botroster_bots::BotId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            (
                bots.get(&owner).map_err(|e| e.to_string())?,
                crate::Thread::Group(id),
            )
        }
    };

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (perm_tx, mut perm_rx) = mpsc::unbounded_channel::<ClientAsk>();

    let ev_conn = conn.clone();
    let ev_session = session.to_owned();
    let events = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            if let Some(update) = update_for(&e) {
                let _ = ev_conn.send_notification(v1::SessionNotification::new(
                    v1::SessionId::new(ev_session.clone()),
                    update,
                ));
            }
        }
    });

    let perm_conn = conn.clone();
    let permissions = tokio::spawn(async move {
        while let Some(ask) = perm_rx.recv().await {
            match ask {
                ClientAsk::Approval {
                    session,
                    req,
                    reply,
                } => {
                    let d = ask_client(&perm_conn, &session, &req).await;
                    let _ = reply.send(d);
                }
                ClientAsk::Secret {
                    session,
                    req,
                    reply,
                } => {
                    let v = ask_client_for_secret(&perm_conn, &session, &req).await;
                    let _ = reply.send(v);
                }
            }
        }
    });

    let approver = Arc::new(AcpApprover {
        session: session.to_owned(),
        tx: perm_tx,
    });

    let outcome = crate::run_task(crate::Task {
        hub_url: &cfg.hub,
        server: &cfg.server,
        home: &cfg.home,
        model_opts: &cfg.model_opts,
        bots,
        bot: &bot,
        task,
        approver,
        demo: cfg.demo,
        demo_tools: cfg.demo_tools,
        demo_secret: cfg.demo_secret,
        fallback: "Done.",
        thread,
        max_steps: crate::DEFAULT_MAX_STEPS,
        history: crate::DEFAULT_HISTORY,
        watch: Some(ev_tx),
        cancel: Some(claim.cancel.clone()),
        redirects: Some(claim.redirects.clone()),
    })
    .await;

    let _ = events.await;
    permissions.abort();

    match outcome {
        Ok(o) => match super::turn_end(&o.reason) {
            super::TurnEnd::Stopped { reason, .. } => Ok(v1::PromptResponse::new(reason)),
            // Not a stop reason: the turn did not happen. An error response is
            // the only thing in the protocol that says so.
            super::TurnEnd::Failed { message, transient } => Err(if transient {
                format!("{message} (worth retrying shortly)")
            } else {
                message
            }),
        },
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use botroster_proto::ToolCallId;

    fn started(tool: &str, call: &str) -> AgentEvent {
        AgentEvent::ToolCallStarted {
            step: 1,
            id: botroster_agent::ToolUseId::new("u1"),
            call_id: ToolCallId::new(call),
            tool: tool.to_owned(),
            args: serde_json::json!({}),
        }
    }

    fn kind(u: &v1::SessionUpdate) -> String {
        serde_json::to_value(u).unwrap()["sessionUpdate"]
            .as_str()
            .unwrap_or("?")
            .to_owned()
    }

    /// The live turn cannot reach this: `--demo` replays one scripted reply
    /// and calls no tools, so tool rendering is only exercised here.
    #[test]
    fn a_tool_call_reaches_the_client_named_after_the_tool() {
        let u = update_for(&started("fs.write", "c1")).expect("a tool call is rendered");
        assert_eq!(kind(&u), "tool_call");
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(
            v["title"], "fs.write",
            "the client shows the title to a person, so it is the tool's name"
        );
        assert_eq!(v["toolCallId"], "c1");
        assert_eq!(v["status"], "in_progress");
    }

    #[test]
    fn a_finished_tool_call_says_whether_it_worked() {
        for (ok, want) in [(true, "completed"), (false, "failed")] {
            let e = AgentEvent::ToolCallFinished {
                id: botroster_agent::ToolUseId::new("u1"),
                call_id: ToolCallId::new("c1"),
                ok,
                output: serde_json::json!({}),
                elapsed_ms: 3,
            };
            let u = update_for(&e).expect("a finished call is rendered");
            assert_eq!(kind(&u), "tool_call_update");
            let v = serde_json::to_value(&u).unwrap();
            assert_eq!(
                v["status"], want,
                "a failed call rendered as {}; the person cannot see it went wrong",
                v["status"]
            );
            assert_eq!(
                v["toolCallId"], "c1",
                "the update must match the call it updates"
            );
        }
    }

    /// `None` is a decision. `Thinking` carries a step number and no words,
    /// so rendering it would mean inventing the thought.
    #[test]
    fn events_with_nothing_to_say_are_not_turned_into_empty_bubbles() {
        assert!(update_for(&AgentEvent::Thinking { step: 2 }).is_none());
        assert!(update_for(&AgentEvent::ToolProgress {
            call_id: ToolCallId::new("c1"),
            payload: serde_json::json!({"pct": 10}),
        })
        .is_none());
    }

    fn one_session() -> (Sessions, String) {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let sid = "botroster-test".to_owned();
        sessions.lock().unwrap().insert(
            sid.clone(),
            Session {
                target: Target::Bot(botroster_bots::BotId("b1".to_owned())),
                running: None,
                cancel: None,
            },
        );
        (sessions, sid)
    }

    /// Not a timing test: two prompts racing is exactly the case a sleep-based
    /// test would miss on a fast machine.
    #[test]
    fn a_second_turn_cannot_start_while_the_first_is_running() {
        let (sessions, sid) = one_session();
        let _claim = claim_turn(&sessions, &sid).expect("the first turn claims it");
        let second = claim_turn(&sessions, &sid);
        assert!(
            second.is_err(),
            "two turns claimed one session; the first is now unstoppable, and both answer a conversation neither of them can see"
        );
        assert!(
            second.unwrap_err().contains("session/cancel"),
            "the refusal should say how to proceed, not just that it will not"
        );
    }

    /// `session/load` takes an id from the client, so it can name a session
    /// this process is already running a turn on. Rebinding it there would
    /// drop the live turn's cancel sender (cancellation goes dead) and leave
    /// the session looking idle, so `claim_turn` would start a second turn on
    /// it. Same invariant as the test above, reached through a different
    /// path.
    #[test]
    fn a_conversation_cannot_be_loaded_over_a_running_turn() {
        let (sessions, sid) = one_session();
        let mut stop = claim_turn(&sessions, &sid)
            .expect("the turn claims it")
            .cancel;

        let bot = Target::Bot(botroster_bots::BotId("someone-else".to_owned()));
        let loaded = bind_session(&sessions, &sid, bot);
        assert!(
            loaded.is_err(),
            "a load rebound a session mid-turn; the running turn is now unstoppable"
        );
        assert!(
            loaded.unwrap_err().contains("session/cancel"),
            "the refusal should say how to proceed, not just that it will not"
        );

        // The turn it refused to disturb is still stoppable.
        assert!(
            !*stop.borrow_and_update(),
            "the turn should not already be cancelled"
        );
        claim_turn(&sessions, &sid).expect_err("and the session is still claimed");
    }

    /// An idle session is fair game: rebinding one that is not running a turn
    /// is exactly what a reconnecting client does.
    #[test]
    fn an_idle_session_can_be_rebound_to_a_conversation() {
        let (sessions, sid) = one_session();
        let bot = Target::Bot(botroster_bots::BotId("elsewhere".to_owned()));
        bind_session(&sessions, &sid, bot.clone()).expect("an idle session rebinds");
        let claimed = claim_turn(&sessions, &sid)
            .expect("and is usable afterwards")
            .target;
        assert_eq!(claimed, bot, "the session should point at the loaded Bot");
    }

    // ---- what a client is shown ----

    fn every_kind() -> Vec<AgentEvent> {
        use botroster_agent::model::Usage;
        vec![
            AgentEvent::Started {
                task: "t".into(),
                model: "m".into(),
                tools: vec![],
            },
            AgentEvent::Thinking { step: 1 },
            AgentEvent::AssistantText {
                step: 1,
                text: "hello".into(),
            },
            AgentEvent::ToolCallStarted {
                step: 1,
                id: botroster_agent::ToolUseId::new("u1"),
                call_id: botroster_proto::ToolCallId::new("c1"),
                tool: "fs.write".into(),
                args: serde_json::json!({ "path": "notes.md" }),
            },
            AgentEvent::ToolProgress {
                call_id: botroster_proto::ToolCallId::new("c1"),
                payload: serde_json::json!({ "stage": "running echo botroster-ok" }),
            },
            AgentEvent::ToolCallFinished {
                id: botroster_agent::ToolUseId::new("u1"),
                call_id: botroster_proto::ToolCallId::new("c1"),
                ok: true,
                output: serde_json::json!({ "written": "notes.md" }),
                elapsed_ms: 3,
            },
            AgentEvent::Finished {
                reason: botroster_agent::FinishReason::Completed,
                steps: 1,
                usage: Usage::default(),
                text: "done".into(),
            },
        ]
    }

    /// A transcript shows tool activity, computer use, created files,
    /// questions and approvals. That guarantee erodes one event kind at a
    /// time, silently, behind a catch-all arm.
    ///
    /// `carried` is total over `AgentEvent`, so a new variant stops compiling
    /// until somebody decides. This asserts the decision and the code agree.
    #[test]
    fn every_event_either_reaches_the_client_or_is_dropped_on_purpose() {
        for event in every_kind() {
            let rendered = update_for(&event).is_some();
            match carried(&event) {
                Carried::Rendered => assert!(
                    rendered,
                    "{event:?} is meant to reach a client and does not"
                ),
                Carried::Dropped(why) => assert!(
                    !rendered,
                    "{event:?} reaches a client although it is dropped for: {why}"
                ),
            }
        }
    }

    /// Four of the seven must reach a client, and they are the four a person
    /// would notice missing. Pinned by count as well as by kind so that
    /// "dropped" cannot quietly become the answer for all of them.
    #[test]
    fn the_work_itself_is_what_reaches_the_client() {
        let shown: Vec<_> = every_kind()
            .iter()
            .filter(|e| update_for(e).is_some())
            .map(std::mem::discriminant)
            .collect();
        assert_eq!(
            shown.len(),
            4,
            "expected speech, the call, its progress and its result"
        );
    }

    /// A tool call without its arguments says "fs.write" and nothing about
    /// what. The replayed form of the same call carries them, so leaving them
    /// off here would make one conversation render differently live and on
    /// reopen.
    #[test]
    fn a_tool_call_carries_what_it_was_asked_to_do() {
        let event = AgentEvent::ToolCallStarted {
            step: 1,
            id: botroster_agent::ToolUseId::new("u1"),
            call_id: botroster_proto::ToolCallId::new("c1"),
            tool: "fs.write".into(),
            args: serde_json::json!({ "path": "notes.md" }),
        };
        let Some(v1::SessionUpdate::ToolCall(call)) = update_for(&event) else {
            panic!("a tool call should render");
        };
        assert_eq!(
            call.raw_input.as_ref().and_then(|v| v.get("path")),
            Some(&serde_json::json!("notes.md")),
            "the arguments did not travel with the call"
        );
    }

    /// "Computer use": what the machine is doing while it does it.
    #[test]
    fn progress_reaches_the_client_as_the_stage_it_is_at() {
        let event = AgentEvent::ToolProgress {
            call_id: botroster_proto::ToolCallId::new("c1"),
            payload: serde_json::json!({ "stage": "running echo botroster-ok" }),
        };
        let Some(v1::SessionUpdate::ToolCallUpdate(update)) = update_for(&event) else {
            panic!("progress with a stage should render");
        };
        let content = update.fields.content.expect("a stage to show");
        let v1::ToolCallContent::Content(c) = &content[0] else {
            panic!("the stage should be plain content");
        };
        let v1::ContentBlock::Text(t) = &c.content else {
            panic!("the stage is text");
        };
        assert_eq!(t.text, "running echo botroster-ok");
    }

    /// A payload nobody has agreed the shape of has nothing a client could put
    /// in front of a person. Inventing one would show a shape BOTROSTER made up.
    #[test]
    fn progress_with_nothing_to_say_is_not_sent() {
        let event = AgentEvent::ToolProgress {
            call_id: botroster_proto::ToolCallId::new("c1"),
            payload: serde_json::json!({ "bytes": 4096 }),
        };
        assert!(update_for(&event).is_none());
        assert_eq!(carried(&event), Carried::Dropped("no stage to show"));
    }

    /// A transcript shows created files. A status alone would make that
    /// unreachable by any client.
    #[test]
    fn a_finished_call_carries_what_it_produced() {
        let event = AgentEvent::ToolCallFinished {
            id: botroster_agent::ToolUseId::new("u1"),
            call_id: botroster_proto::ToolCallId::new("c1"),
            ok: true,
            output: serde_json::json!({ "written": "notes.md" }),
            elapsed_ms: 3,
        };
        let Some(v1::SessionUpdate::ToolCallUpdate(update)) = update_for(&event) else {
            panic!("a finished call should render");
        };
        assert_eq!(update.fields.status, Some(v1::ToolCallStatus::Completed));
        assert_eq!(
            update
                .fields
                .raw_output
                .as_ref()
                .and_then(|v| v.get("written")),
            Some(&serde_json::json!("notes.md")),
            "what the tool produced did not travel"
        );
    }

    /// A message that never reached the turn must not be reported as
    /// delivered. Redirects are picked up at the top of a step, so one sent
    /// during the final model call is still queued when the turn ends.
    /// Handing the prompt that sent it the turn's stop reason would tell the
    /// person their instruction landed when the Bot never saw it.
    #[test]
    fn a_message_left_undelivered_is_reported_as_such() {
        let (sessions, sid) = one_session();
        let claim = claim_turn(&sessions, &sid).expect("the turn claims it");

        // A message arrives, and the turn ends without ever reading it.
        let joined = join_turn(&sessions, &sid, "actually check the invoices")
            .expect("a running turn accepts it");
        release_turn(&sessions, &sid, v1::StopReason::EndTurn);

        let answer = joined.done.borrow().clone().expect("the turn ended");
        let why = answer.expect_err("an undelivered message reported success");
        assert!(
            why.contains("never saw it") && why.contains("invoices"),
            "the refusal should say what was lost and that it can be resent: {why}"
        );
        drop(claim);
    }

    /// A joined prompt carries its own notice, so the handler must not send
    /// one as well.
    ///
    /// This is the premise the call site's ordering rests on. The notice lives
    /// inside `task`; `join_turn` hands `task` to the running turn; the agent
    /// emits it back as `Redirected`, which `update_for` renders as a
    /// `UserMessageChunk`. Sending the notice separately and joining would
    /// put it on screen twice (once alone, once inside the person's own
    /// words), which is why the send sits below `claim_turn` rather than
    /// beside `prompt_text`.
    ///
    /// What this does not prove: that the handler refrains from sending it.
    /// Moving the send above [`join_turn`] would not fail this test, because
    /// nothing here can observe the connection. Catching that needs a turn
    /// held open at a known moment (a hub, and an approval answered on cue),
    /// and blocking a client's permission handler to hold it is the deadlock
    /// described in the module docs.
    #[test]
    fn a_redirect_already_carries_what_could_not_be_read() {
        let (sessions, sid) = one_session();
        let claim = claim_turn(&sessions, &sid).expect("the turn claims it");

        let prompt = prompt_text(&[
            v1::ContentBlock::Text(v1::TextContent::new("is this right?".to_owned())),
            an_image(),
        ])
        .expect("the text is readable");
        let note = prompt.note.clone().expect("an image went unread");
        join_turn(&sessions, &sid, &prompt.task).expect("a running turn accepts it");

        // `undelivered` is the same drain the step loop performs, which is
        // what makes this the message the turn would actually receive.
        let delivered = claim.redirects.undelivered();
        assert_eq!(delivered.len(), 1, "one message, not two: {delivered:?}");
        assert!(
            delivered[0].contains(&note),
            "the turn was handed the message without its notice, so nothing \
             would render it: {delivered:?}"
        );
        drop(claim);
    }

    /// When the turn did read it, the sender gets the turn's own ending.
    #[test]
    fn a_message_the_turn_read_is_answered_by_that_turn() {
        let (sessions, sid) = one_session();
        let claim = claim_turn(&sessions, &sid).expect("the turn claims it");
        let joined = join_turn(&sessions, &sid, "and check the invoices").expect("accepted");

        // The agent picks it up at its next step boundary. `undelivered` is
        // the same drain the loop does; reading it here is exactly what the
        // agent's own step would have done.
        let read = claim.redirects.undelivered();
        assert_eq!(read, vec!["and check the invoices".to_owned()]);

        release_turn(&sessions, &sid, v1::StopReason::EndTurn);
        let answer = joined.done.borrow().clone().expect("the turn ended");
        assert_eq!(
            answer.expect("a delivered message should get the turn's ending"),
            v1::StopReason::EndTurn
        );
    }

    /// Nothing is running, so there is nothing to join and the caller should
    /// start a turn instead.
    #[test]
    fn an_idle_session_is_not_joinable() {
        let (sessions, sid) = one_session();
        assert!(join_turn(&sessions, &sid, "hello").is_none());
    }

    #[test]
    fn the_session_is_free_again_once_the_turn_ends() {
        let (sessions, sid) = one_session();
        let claimed = claim_turn(&sessions, &sid).expect("first");
        drop(claimed);
        release_turn(&sessions, &sid, v1::StopReason::EndTurn);
        assert!(
            claim_turn(&sessions, &sid).is_ok(),
            "the session stayed busy after its turn ended, so it takes no more prompts ever"
        );
    }

    /// Cancellation has to reach this turn. Holding a sender from a previous
    /// turn would cancel nothing.
    #[test]
    fn each_turn_gets_the_stop_button_that_belongs_to_it() {
        let (sessions, sid) = one_session();
        let first = claim_turn(&sessions, &sid).expect("first").cancel;
        release_turn(&sessions, &sid, v1::StopReason::EndTurn);
        let second = claim_turn(&sessions, &sid).expect("second").cancel;

        // Cancel through the session, the way the notification handler does.
        {
            let map = sessions.lock().unwrap();
            let s = map.get(&sid).unwrap();
            s.cancel.as_ref().unwrap().send(true).unwrap();
        }
        assert!(*second.borrow(), "the running turn did not see the stop");
        assert!(
            !*first.borrow(),
            "cancelling reached a finished turn's receiver, so the wrong turn was stopped"
        );
    }

    #[test]
    fn a_directory_becomes_a_bot_name_a_person_would_recognise() {
        assert_eq!(bot_name_for(Path::new("/tmp/Payments API")), "payments-api");
        assert_eq!(bot_name_for(Path::new("/srv/ledger")), "ledger");
        // Nothing to borrow a name from, and an empty Bot name is not a name.
        assert_eq!(bot_name_for(Path::new("/")), "workspace");
        // Stability is the point: the same directory twice is the same Bot.
        assert_eq!(
            bot_name_for(Path::new("/a/My Project")),
            bot_name_for(Path::new("/b/My Project")),
            "the same project name must reach the same Bot"
        );
    }

    #[test]
    fn a_prompt_with_no_text_is_refused_rather_than_run_as_an_empty_task() {
        assert!(prompt_text(&[]).is_err(), "an empty prompt is not a task");
        let text = prompt_text(&[v1::ContentBlock::Text(v1::TextContent::new(
            "fix the build".to_owned(),
        ))])
        .expect("text is readable");
        assert_eq!(text.task, "fix the build");
        assert!(
            text.note.is_none(),
            "nothing was unreadable, so nothing is said"
        );
    }

    fn a_link(name: &str, uri: &str) -> v1::ContentBlock {
        v1::ContentBlock::ResourceLink(v1::ResourceLink::new(name.to_owned(), uri.to_owned()))
    }

    fn an_image() -> v1::ContentBlock {
        v1::ContentBlock::Image(v1::ImageContent::new(
            "iVBOR".to_owned(),
            "image/png".to_owned(),
        ))
    }

    /// The schema is not optional about this: "All agents MUST support
    /// resource links in prompts", and no capability declines them. Dropping
    /// the link loses the only part of the message that says what to look
    /// at, so the URI has to reach the task.
    #[test]
    fn a_resource_link_reaches_the_bot_because_the_protocol_requires_it() {
        let task = prompt_text(&[
            v1::ContentBlock::Text(v1::TextContent::new("what changed here?".to_owned())),
            a_link("notes.md", "file:///srv/notes.md"),
        ])
        .expect("text and a link are both readable")
        .task;
        assert!(task.contains("what changed here?"));
        assert!(
            task.contains("file:///srv/notes.md"),
            "the Bot cannot act on a link it was never given: {task}"
        );
        assert!(task.contains("notes.md"), "the link keeps its name: {task}");
    }

    /// Text plus an image must not answer the text and drop the image without
    /// a word; the answer would look like an answer about the screenshot.
    #[test]
    fn an_unreadable_block_is_named_in_the_task_not_silently_dropped() {
        let task = prompt_text(&[
            v1::ContentBlock::Text(v1::TextContent::new("is this right?".to_owned())),
            an_image(),
        ])
        .expect("the text is still readable");
        assert!(
            task.task.contains("is this right?"),
            "the text still gets through"
        );
        assert!(
            task.task.contains("1 image"),
            "the person is told what was not read, and what kind: {task:?}"
        );
        // Told live, not only on coming back to the conversation. `replay`
        // guarantees the two views match, so the notice shown live has to be
        // the notice that replays.
        assert_eq!(
            task.note.as_deref(),
            task.task.lines().last(),
            "what is shown live must be what replays: {task:?}"
        );
    }

    /// Counted and named, so somebody can tell one missed attachment from
    /// four. Order follows the message.
    #[test]
    fn what_was_not_read_is_counted_by_kind() {
        assert_eq!(counted(&["image"]), "1 image");
        assert_eq!(counted(&["image", "image"]), "2 images");
        assert_eq!(
            counted(&["image", "audio clip", "image"]),
            "2 images and 1 audio clip"
        );
    }

    /// Nothing readable at all is still an error, but one that says what the
    /// message actually held: "the prompt was empty" is false and unhelpful
    /// when a screenshot was attached.
    #[test]
    fn a_prompt_of_only_unreadable_blocks_says_what_it_held() {
        let why = prompt_text(&[an_image()]).expect_err("there is no task here");
        assert!(why.contains("1 image"), "{why}");
        assert!(
            prompt_text(&[]).expect_err("still empty").contains("empty"),
            "an actually-empty prompt keeps its own wording"
        );
    }
}

#[cfg(test)]
mod secret_ask_tests {
    use super::*;

    fn answer(option: &str, meta: Option<serde_json::Value>) -> v1::RequestPermissionResponse {
        let res = v1::RequestPermissionResponse::new(v1::RequestPermissionOutcome::Selected(
            v1::SelectedPermissionOutcome::new(v1::PermissionOptionId::new(option.to_owned())),
        ));
        match meta {
            Some(v) => {
                let mut m = serde_json::Map::new();
                m.insert(SECRET_META.to_owned(), v);
                res.meta(m)
            }
            None => res,
        }
    }

    /// Every way an answer can be wrong is a refusal.
    ///
    /// ACP has no free-text prompt, so this rides on `_meta`, which means a
    /// client that has never heard of the extension still sees the two options
    /// and can still click one. None of those clicks may produce a credential.
    /// The dangerous outcome is not a refusal; it is an empty credential
    /// stored under a real name, which then fails against a service later with
    /// nothing to explain why.
    #[test]
    fn only_a_real_value_under_the_agreed_key_is_a_credential() {
        let val = |v: serde_json::Value| Some(serde_json::json!({ "value": v }));
        let cases: Vec<(&str, v1::RequestPermissionResponse)> = vec![
            (
                "an option botroster never offered",
                answer("allow-always", val("x".into())),
            ),
            ("declining", answer("reject-once", val("x".into()))),
            // Picked "provide" and sent nothing: a client that does not
            // implement the extension. Must not read as an empty credential.
            ("provide with no _meta at all", answer(PROVIDE, None)),
            (
                "provide with an empty _meta",
                answer(PROVIDE, Some(serde_json::json!({}))),
            ),
            (
                "a value that is not a string",
                answer(PROVIDE, val(42.into())),
            ),
            (
                "a null value",
                answer(PROVIDE, val(serde_json::Value::Null)),
            ),
            ("an empty string", answer(PROVIDE, val("".into()))),
            ("only whitespace", answer(PROVIDE, val("   \n".into()))),
        ];
        for (what, res) in cases {
            assert_eq!(supplied_value(res), None, "{what} produced a credential");
        }
    }

    /// A cancelled request is a refusal, not an error and not a value.
    #[test]
    fn a_cancelled_request_supplies_nothing() {
        let res = v1::RequestPermissionResponse::new(v1::RequestPermissionOutcome::Cancelled);
        assert_eq!(supplied_value(res), None);
    }

    /// The value arrives whole, minus the newline a text field adds.
    #[test]
    fn a_supplied_credential_arrives_exactly_as_typed() {
        for (sent, want) in [
            ("sk-live-abc", "sk-live-abc"),
            ("sk-live-abc\n", "sk-live-abc"),
            ("sk-live-abc\r\n", "sk-live-abc"),
            // Not tidied up: an inner or leading space is the person's, and
            // silently rewriting a credential breaks it somewhere else.
            ("sk with spaces", "sk with spaces"),
            (" leading-kept", " leading-kept"),
        ] {
            let res = answer(PROVIDE, Some(serde_json::json!({ "value": sent })));
            assert_eq!(
                supplied_value(res).as_deref(),
                Some(want),
                "sending {sent:?} did not arrive as {want:?}"
            );
        }
    }

    /// The credential's name and reason reach the client; nothing else does.
    ///
    /// The request is drawn by clients as a tool call in the conversation, so
    /// what goes in the title and `raw_input` is what a person, and every
    /// other reader of that transcript, sees.
    #[test]
    fn the_request_carries_the_name_and_the_reason() {
        let meta = serde_json::json!({ "name": "linear-token", "why": "to file the issue" });
        let rendered = meta.to_string();
        assert!(rendered.contains("linear-token"));
        assert!(rendered.contains("to file the issue"));
        // The agreed key is namespaced: `_meta` is shared with every other
        // extension a client implements.
        assert!(SECRET_META.starts_with("botroster/"), "{SECRET_META}");
    }
}

#[cfg(test)]
mod replay_bound_tests {
    use super::*;

    /// A load that left messages out says so, in the transcript.
    ///
    /// `session/load` replays one notification per message, so an unbounded
    /// load is linear in the whole history. Bounding it is right; bounding it
    /// silently is not: a transcript that starts partway through with no
    /// explanation reads as the whole conversation, and a reader would
    /// conclude the Bot has forgotten things it has not.
    #[test]
    fn a_bounded_replay_names_what_it_left_out() {
        // Nothing omitted, nothing said. The ordinary conversation is
        // untouched by this.
        assert!(replay_notice(12, 12).is_none());
        assert!(replay_notice(0, 0).is_none());
        // A shorter history than the limit cannot produce a notice.
        assert!(replay_notice(REPLAY_LIMIT - 1, REPLAY_LIMIT - 1).is_none());

        let notice = replay_notice(20_000, REPLAY_LIMIT).expect("a truncated load says so");
        let v1::SessionUpdate::UserMessageChunk(chunk) = notice else {
            panic!("the notice must be a message in the transcript, not a status");
        };
        let v1::ContentBlock::Text(t) = chunk.content else {
            panic!("the notice has to be readable");
        };
        // The count, so it is clear how much is missing rather than that
        // something is.
        assert!(t.text.contains("19500"), "{}", t.text);
        // Where it went, because "not shown" alone reads as "lost".
        assert!(t.text.contains("still on disk"), "{}", t.text);
        assert!(t.text.contains("search"), "{}", t.text);
    }
}
