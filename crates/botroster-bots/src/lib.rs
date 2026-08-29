//! Bots: named teammates with a job, a standing brief, and a conversation that
//! does not reset.
//!
//! # The description/message split is the memory design
//!
//! A Bot's description holds what stays true ("never send external mail
//! without approval", "always cite the source system"). A message holds one
//! task. The description is replayed into every system prompt; messages
//! accumulate. Collapsing the two turns a teammate back into a chat box:
//! standing rules drift out of the context window, and the Bot loses its own
//! boundaries exactly when the conversation gets long enough to matter.
//!
//! # Storage
//!
//! ```text
//! <root>/bots/<id>/bot.json          profile
//! <root>/bots/<id>/conversation.jsonl  append-only message log
//! ```
//!
//! The log is append-only JSONL rather than a rewritten JSON array: appending
//! is a single write that cannot corrupt what came before, and a truncated
//! final line costs one message rather than the whole history.

#![forbid(unsafe_code)]

pub mod schedule;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use botroster_agent::model::{Content, Message, ToolUseId};
use serde::{Deserialize, Serialize};

/// Maximum number of Bots and groups combined on one account. Matches the
/// observed limit in the product this follows.
pub const MAX_BOTS: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum BotError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no bot `{0}`")]
    NotFound(String),
    #[error("a bot named `{0}` already exists")]
    Duplicate(String),
    #[error("a name must contain at least one letter or digit")]
    BadName,
    /// Bots and group chats share one cap ("an account can have up to 50 Bots
    /// and group chats combined"). The message names both so that a user with
    /// 45 Bots and 5 groups is not told to delete a Bot.
    #[error("this account already has {MAX_BOTS} Bots and group chats")]
    TooMany,
    #[error("corrupt bot data: {0}")]
    Corrupt(String),
    #[error("a bot cannot hand work to itself")]
    SelfHandoff,
    /// Two Bots answer to the same display name. Guessing which one was meant
    /// would hand work to the wrong teammate.
    #[error("`{name}` names more than one bot — use an id: {}", ids.join(", "))]
    Ambiguous { name: String, ids: Vec<String> },
    #[error("a group needs between {MIN_GROUP} and {MAX_GROUP} members, got {0}")]
    BadGroupSize(usize),
    #[error("{0}")]
    BadSchedule(String),
    #[error("this bot already has {MAX_ROUTINES} routines")]
    TooManyRoutines,
    #[error("{0}")]
    BadTrigger(String),
}

pub type Result<T> = std::result::Result<T, BotError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BotId(pub String);

impl BotId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bot {
    pub id: BotId,
    pub name: String,
    /// What this Bot owns, in a few words: "Account health", "Bug repro".
    #[serde(default)]
    pub title: String,
    /// Standing rules and context. Replayed into every system prompt.
    #[serde(default)]
    pub description: String,
    /// Creation order, for stable listing without depending on a clock.
    #[serde(default)]
    pub seq: u64,
    /// Hidden bots keep their work and their history; they only leave the
    /// list.
    #[serde(default)]
    pub hidden: bool,
}

impl Bot {
    /// The system prompt for this Bot: who it is, then its standing brief.
    ///
    /// `base` carries the rules that apply to every Bot; the description is
    /// appended so a Bot's own brief is the last thing the model reads.
    pub fn system_prompt(&self, base: &str) -> String {
        let mut s = String::from(base);
        s.push_str("\n\n---\n\n");
        s.push_str(&format!("You are **{}**", self.name));
        if !self.title.is_empty() {
            s.push_str(&format!(", {}", self.title));
        }
        s.push_str(".\n");
        if !self.description.is_empty() {
            s.push_str("\nYour standing brief, which applies to every task:\n\n");
            s.push_str(&self.description);
            s.push('\n');
        }
        s
    }
}

pub struct BotStore {
    root: PathBuf,
}

impl BotStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("bots"))?;
        Ok(Self { root })
    }

    fn dir(&self, id: &BotId) -> PathBuf {
        self.root.join("bots").join(&id.0)
    }
    fn profile_path(&self, id: &BotId) -> PathBuf {
        self.dir(id).join("bot.json")
    }
    fn log_path(&self, id: &BotId) -> PathBuf {
        self.dir(id).join("conversation.jsonl")
    }
    /// Where the hub writes what a session did.
    ///
    /// `sessions/`, not `runs/`. The hub sees sessions; it does not see turns
    /// or prompts, and a "run" means a turn to anybody reading it. Those
    /// coincide for `botroster run`, which opens one session per invocation,
    /// and do not for the desktop client, which keeps one session across a
    /// whole conversation. Naming the directory after what is actually in it
    /// costs nothing now and saves a rename later.
    fn session_path(&self, id: &BotId, session: &str) -> PathBuf {
        self.dir(id)
            .join("sessions")
            .join(format!("{session}.jsonl"))
    }

    /// Turn a display name into a stable, filesystem-safe id.
    pub fn slug(name: &str) -> Result<BotId> {
        let mut out = String::new();
        let mut last_dash = true; // suppress a leading dash
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        if out.is_empty() {
            return Err(BotError::BadName);
        }
        Ok(BotId(out))
    }

    pub fn create(&self, name: &str, title: &str, description: &str) -> Result<Bot> {
        let id = Self::slug(name)?;
        if self.profile_path(&id).exists() {
            return Err(BotError::Duplicate(name.to_owned()));
        }
        let all = self.list(true)?;
        // Bots and groups share one cap, so both are counted here exactly as
        // `create_group` counts them. Counting Bots alone would enforce the
        // rule from one side only.
        if all.len() + self.groups(true)?.len() >= MAX_BOTS {
            return Err(BotError::TooMany);
        }
        let seq = all.iter().map(|b| b.seq).max().map_or(0, |m| m + 1);

        let bot = Bot {
            id: id.clone(),
            name: name.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            seq,
            hidden: false,
        };
        fs::create_dir_all(self.dir(&id))?;
        self.save(&bot)?;
        Ok(bot)
    }

    pub fn save(&self, bot: &Bot) -> Result<()> {
        fs::create_dir_all(self.dir(&bot.id))?;
        let path = self.profile_path(&bot.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(bot).expect("bot serialises"),
        )?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get(&self, id: &BotId) -> Result<Bot> {
        let s = fs::read_to_string(self.profile_path(id))
            .map_err(|_| BotError::NotFound(id.0.clone()))?;
        serde_json::from_str(&s).map_err(|e| BotError::Corrupt(e.to_string()))
    }

    /// Resolve a name or id to a Bot.
    ///
    /// Three lookups, narrowest first: the string as an id, then its slug,
    /// then the display names. The third exists because a Bot can be renamed
    /// and its id cannot: the id is the durable identity that conversations,
    /// group membership and routines hang off, so [`rename`](Self::rename)
    /// leaves it alone, and a Bot called "Recruiting" may live at
    /// `talent-scout`.
    ///
    /// Ambiguity is an error rather than a guess: two Bots may share a display
    /// name (a store on disk can be hand-edited), and picking whichever the
    /// filesystem listed first would hand work to the wrong teammate.
    pub fn resolve(&self, name_or_id: &str) -> Result<Bot> {
        let wanted = name_or_id.trim();
        let mut hits: Vec<Bot> = Vec::new();
        let found = |hits: &mut Vec<Bot>, b: Bot| {
            if !hits.iter().any(|h| h.id == b.id) {
                hits.push(b);
            }
        };

        // Every way the string could mean a Bot is gathered before deciding;
        // this is not first-match-wins. Returning on the slug would resolve
        // "Talent Scout" to `talent-scout` even when a second, renamed Bot
        // also displays that name.
        let exact = BotId(wanted.to_owned());
        if self.profile_path(&exact).exists() {
            found(&mut hits, self.get(&exact)?);
        }
        // A name with no slug is not an error here: it matches nothing, and
        // `NotFound` is the useful answer.
        if let Ok(slug) = Self::slug(wanted) {
            if self.profile_path(&slug).exists() {
                found(&mut hits, self.get(&slug)?);
            }
        }
        for b in self.list(true)? {
            if b.name.eq_ignore_ascii_case(wanted) {
                found(&mut hits, b);
            }
        }

        match hits.len() {
            0 => Err(BotError::NotFound(name_or_id.to_owned())),
            1 => Ok(hits.remove(0)),
            _ => Err(BotError::Ambiguous {
                name: name_or_id.to_owned(),
                ids: hits.iter().map(|b| b.id.0.clone()).collect(),
            }),
        }
    }

    /// Change what a Bot is called, keeping its id.
    ///
    /// The id does not move. It is the key its conversation, inbox, group
    /// memberships and routines are stored under, so renaming the directory
    /// would either orphan all of them or require rewriting references this
    /// store does not track.
    ///
    /// # Errors
    /// If there is no such Bot, the new name is empty once trimmed, or the
    /// name already belongs to another Bot.
    pub fn rename(&self, name_or_id: &str, new_name: &str) -> Result<Bot> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(BotError::BadName);
        }
        let mut bot = self.resolve(name_or_id)?;
        // `create` refuses a name already taken, so `rename` must too;
        // otherwise two Bots could share a name and `resolve` would refuse
        // both, so `@Ledger` and `bot.send "Ledger"` would reach neither.
        //
        // Compared by slug, as `create` does, so `Ledger ` and `ledger` are
        // the same name. The Bot's own id is excluded: renaming a Bot to what
        // it is already called is a no-op, and a settings form that saves an
        // unchanged name must not fail.
        let taken = Self::slug(new_name)?;
        if taken != bot.id && self.profile_path(&taken).exists() {
            return Err(BotError::Duplicate(new_name.to_owned()));
        }
        bot.name = new_name.to_owned();
        self.save(&bot)?;
        Ok(bot)
    }

    /// Change a Bot's title or description, leaving anything not given alone.
    ///
    /// `None` means "unchanged" rather than "clear it": a settings form that
    /// sends only what was edited must not blank the field beside it. Clearing
    /// is `Some("")`, which is explicit.
    ///
    /// # Errors
    /// If there is no such Bot.
    pub fn describe(
        &self,
        name_or_id: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<Bot> {
        let mut bot = self.resolve(name_or_id)?;
        if let Some(t) = title {
            bot.title = t.trim().to_owned();
        }
        if let Some(d) = description {
            bot.description = d.trim().to_owned();
        }
        self.save(&bot)?;
        Ok(bot)
    }

    pub fn list(&self, include_hidden: bool) -> Result<Vec<Bot>> {
        let dir = self.root.join("bots");
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let id = BotId(e.file_name().to_string_lossy().into_owned());
            // A directory without a readable profile is skipped rather than
            // failing the listing: one damaged bot must not hide the rest.
            if let Ok(b) = self.get(&id) {
                if include_hidden || !b.hidden {
                    out.push(b);
                }
            }
        }
        out.sort_by_key(|b| b.seq);
        Ok(out)
    }

    /// Delete a Bot and its history, and report what went with it.
    ///
    /// Groups live outside the Bot's directory, so the Bot is removed from
    /// every group that lists it. Otherwise the membership list would still
    /// name it and `owner_for` would hand turns to a Bot that no longer
    /// exists.
    ///
    /// A group left with no members is deleted too: it is a thread nothing
    /// can answer, and keeping it means a sidebar entry that fails whenever it
    /// is opened.
    ///
    /// The conversation, inbox and routines live under the Bot's directory
    /// and go with it. The counts are returned so a caller can say what an
    /// irreversible operation destroyed.
    ///
    /// # Errors
    /// If there is no such Bot, or a group cannot be rewritten.
    pub fn delete(&self, id: &BotId) -> Result<Deleted> {
        if !self.dir(id).exists() {
            return Err(BotError::NotFound(id.0.clone()));
        }
        // Counted before the groups are touched: if rewriting one fails,
        // nothing has been destroyed yet and the error is the whole outcome.
        let messages = self.message_count(id)?;
        let routines = self.routines(id)?.len();

        let mut left = Vec::new();
        let mut emptied = Vec::new();
        for mut g in self.groups(true)? {
            if !g.members.iter().any(|m| m == id) {
                continue;
            }
            g.members.retain(|m| m != id);
            // Only an emptied group is deleted. A group of one is a shape
            // `create_group` refuses, but its thread is still a conversation
            // somebody can read, and destroying it as a side effect of
            // deleting a different Bot is worse than a small group. The
            // asymmetry with `create_group` is intentional; held by
            // `a_group_whose_last_member_is_deleted_goes_too`.
            if g.members.is_empty() {
                self.delete_group(&g.id)?;
                emptied.push(g.name);
            } else {
                self.save_group(&g)?;
                left.push(g.name);
            }
        }

        fs::remove_dir_all(self.dir(id))?;
        Ok(Deleted {
            messages,
            routines,
            left,
            emptied,
        })
    }

    /// Copy a Bot's profile as the starting point for another.
    ///
    /// Carries the brief but not the conversation. A copy made to cover a
    /// second region must not inherit the first region's history and answer
    /// with facts about the wrong account.
    pub fn duplicate(&self, id: &BotId, new_name: &str) -> Result<Bot> {
        let src = self.get(id)?;
        self.create(new_name, &src.title, &src.description)
    }

    // ── the record of what a session did ──────────────────────────────

    /// Append one already-serialised line to a session's record.
    ///
    /// Takes a line rather than a typed step because the type belongs to the
    /// hub, which owns what a record means; this crate owns where a Bot's
    /// files live and nothing else. The dependency runs that way round —
    /// `botrosterd` knows about Bots, and this crate must not learn about the
    /// hub to store a string for it.
    ///
    /// Append-only, through the same helper `conversation.jsonl` uses, for the
    /// same reason: a single append-mode `write_all` is atomic on the
    /// platforms this ships to, so two writers interleave whole lines rather
    /// than splicing one into another.
    ///
    /// # Errors
    /// If the directory cannot be created or the file cannot be appended to.
    pub fn append_session(&self, id: &BotId, session: &str, line: &str) -> Result<()> {
        let path = self.session_path(id, session);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        append_lines(&path, [line.to_owned()])
    }

    /// Every line of one session's record, oldest first.
    ///
    /// Returns an empty vector for a session that was never recorded, rather
    /// than an error: a Bot that has done nothing is the ordinary state of a
    /// new one, and a caller listing records should not have to tell "no such
    /// session" from "no such Bot".
    ///
    /// # Errors
    /// If the file exists and cannot be read.
    pub fn session_record(&self, id: &BotId, session: &str) -> Result<Vec<String>> {
        let path = self.session_path(id, session);
        if !path.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_to_string(&path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Which sessions this Bot has a record for, oldest first by name.
    ///
    /// # Errors
    /// If the directory exists and cannot be listed.
    pub fn sessions(&self, id: &BotId) -> Result<Vec<String>> {
        let dir = self.dir(id).join("sessions");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out: Vec<String> = fs::read_dir(&dir)?
            .filter_map(std::result::Result::ok)
            .filter_map(|e| {
                let p = e.path();
                (p.extension()? == "jsonl")
                    .then(|| p.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .collect();
        out.sort();
        Ok(out)
    }

    // ── conversation ──────────────────────────────────────────────────

    /// Append messages to a Bot's history.
    pub fn append(&self, id: &BotId, messages: &[Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        // A retried run restates its task, and the attempt that failed left
        // that task in the log with nothing answering it. Appending the
        // restatement would give the model the same instruction twice in a
        // row, and more across a long outage.
        //
        // Two identical user messages with nothing between them is not
        // something a conversation can produce: either the model answered,
        // and the answer sits between them, or the attempt died before it
        // could. Collapsing them cannot lose a real exchange, and costs one
        // line read off the end of the log.
        let messages = match self.history(id, Some(1))?.first() {
            Some(last) if last.role == botroster_agent::Role::User && *last == messages[0] => {
                &messages[1..]
            }
            _ => messages,
        };
        if messages.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.dir(id))?;
        append_lines(
            &self.log_path(id),
            messages
                .iter()
                .map(|m| serde_json::to_string(m).expect("message serialises")),
        )
    }

    /// Read a Bot's history, oldest first.
    ///
    /// `limit` keeps only the most recent N messages: history grows without
    /// bound and a context window does not.
    ///
    /// Reads from the end of the file when a limit is given. Every run replays
    /// a Bot's recent messages, and parsing the whole log to find the last
    /// forty would make a long-lived Bot pay for its entire history on every
    /// task. The cost belongs to what is asked for, not to how long the Bot
    /// has existed.
    pub fn history(&self, id: &BotId, limit: Option<usize>) -> Result<Vec<Message>> {
        let path = self.log_path(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        if let Some(n) = limit {
            return Ok(Self::repair_window(Self::parse_lines(&tail_lines(
                &path, n,
            )?)));
        }
        let f = BufReader::new(fs::File::open(&path)?);
        let mut out = Vec::new();
        for line in f.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(&line) {
                Ok(m) => out.push(m),
                // A partial final line is what a crash mid-append leaves. Lose
                // that message rather than the whole conversation.
                Err(e) => tracing::warn!(error = %e, "skipping an unreadable history line"),
            }
        }
        // The unlimited path is not cut mid-pair, but it does skip lines it
        // cannot parse, and a skipped line orphans whatever answered it just as
        // a window boundary would.
        Ok(Self::repair_window(out))
    }

    /// Drop tool blocks the window cannot account for.
    ///
    /// A conversation window is not a line count, and treating it as one is how
    /// this broke. A tool-using Bot's log is a repeating pair — an assistant
    /// message asking for a tool, then a user message carrying the result — and
    /// taking the last N *lines* of that cuts between the two whenever N lands
    /// wrong. The window then opens on a `tool_result` whose `tool_use` is one
    /// line further back, outside the request. Anthropic emits the block
    /// unconditionally and the OpenAI dialect emits a bare `role:"tool"`
    /// message, so both vendors answer 400.
    ///
    /// With one tool call per turn that is every third window size, and the
    /// shipped `DEFAULT_HISTORY` of 40 was one of them.
    ///
    /// What made it expensive is that it heals. `fresh` is appended, the window
    /// start advances by one onto the `tool_use` that owns the orphan, and the
    /// next run is legal — so a person retypes the task and it works, and never
    /// files anything. A routine gets no second try: it loses the firing,
    /// records `retryable: false` and a vendor message about a `tool_use_id`
    /// nobody can act on, and recurs whenever the window lands badly again.
    /// `agent_loop.rs` already says this about the mirror case, unanswered calls
    /// at the *end* of a transcript: it "breaks the next run on that Bot, on
    /// another day, with a 400 nobody would trace back". Both ends are repaired
    /// here.
    ///
    /// The repair drops blocks rather than truncating to the first legal
    /// message, which keeps the most context: an orphan can also appear in the
    /// middle, because both read paths skip a line that will not parse, and
    /// truncating on a mid-window orphan would throw away everything before it.
    /// A message left holding nothing is dropped, since an empty message is its
    /// own vendor error.
    ///
    /// It lives inside `history` rather than beside it so that no caller can
    /// forget it. That is the same reasoning as `compact`'s `&mut [Message]`
    /// signature: make the shape a property of the only function that produces
    /// it.
    fn repair_window(messages: Vec<Message>) -> Vec<Message> {
        use std::collections::HashSet;

        fn keep_non_empty(messages: Vec<Message>, mut f: impl FnMut(&mut Message)) -> Vec<Message> {
            let mut out = Vec::with_capacity(messages.len());
            for mut m in messages {
                f(&mut m);
                // An empty message is its own vendor error, so a message left
                // holding nothing goes rather than being sent hollow.
                if !m.content.is_empty() {
                    out.push(m);
                }
            }
            out
        }

        // Two passes, in this order, and not one pass over two precomputed
        // sets. The first version of this collected `asked` and `answered` up
        // front and filtered once, which keeps a call whose only answer is a
        // result the same filter is dropping - leaving exactly the unanswered
        // call the second half exists to prevent. Dropping a result can orphan
        // a call, so the calls have to be judged against what actually
        // survived. The reverse cannot happen: a call is only dropped when
        // nothing answered it, so no surviving result refers to it, and two
        // passes reach a fixed point rather than needing a loop.

        // Forward: a result is legal only if its call is already behind it in
        // this window. Membership is not enough - the call has to come first,
        // or the model is answering a question it has not been asked.
        let mut seen: HashSet<ToolUseId> = HashSet::new();
        let messages = keep_non_empty(messages, |m| {
            m.content.retain(|c| match c {
                Content::ToolResult { id, .. } => seen.contains(id),
                Content::ToolUse { .. } | Content::Text { .. } => true,
            });
            for c in &m.content {
                if let Content::ToolUse { id, .. } = c {
                    seen.insert(id.clone());
                }
            }
        });

        // Then: a call nobody answered is the failure `agent_loop.rs` guards on
        // the write side, arriving from the read side instead.
        let mut answered: HashSet<ToolUseId> = HashSet::new();
        for m in &messages {
            for c in &m.content {
                if let Content::ToolResult { id, .. } = c {
                    answered.insert(id.clone());
                }
            }
        }
        keep_non_empty(messages, |m| {
            m.content.retain(|c| match c {
                Content::ToolUse { id, .. } => answered.contains(id),
                Content::ToolResult { .. } | Content::Text { .. } => true,
            });
        })
    }

    /// Parse whole lines into messages, skipping any that will not read.
    ///
    /// A partial final line is what a crash mid-append leaves behind. Lose that
    /// one message rather than the whole conversation.
    fn parse_lines(lines: &[String]) -> Vec<Message> {
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str::<Message>(l) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping an unreadable history line");
                    None
                }
            })
            .collect()
    }

    /// How many messages a Bot has.
    ///
    /// Counts lines without parsing them. This is called for every Bot in
    /// `bot ls`, and parsing would turn a listing into deserialising every
    /// message on the account, including every tool result.
    pub fn message_count(&self, id: &BotId) -> Result<usize> {
        let path = self.log_path(id);
        if !path.exists() {
            return Ok(0);
        }
        let mut f = BufReader::new(fs::File::open(&path)?);
        let mut n = 0usize;
        let mut buf = Vec::with_capacity(64 * 1024);
        loop {
            buf.clear();
            let read = std::io::BufRead::read_until(&mut f, b'\n', &mut buf)?;
            if read == 0 {
                break;
            }
            // A trailing newline does not start a message, and a crash
            // mid-append can leave a line with no newline at all; both are
            // handled by counting non-empty lines rather than newlines.
            if buf.iter().any(|b| !b.is_ascii_whitespace()) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Forget a Bot's conversation while keeping the Bot.
    pub fn clear_history(&self, id: &BotId) -> Result<()> {
        let p = self.log_path(id);
        if p.exists() {
            fs::remove_file(p)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use botroster_agent::model::{Content, Role};

    fn store() -> (tempfile::TempDir, BotStore) {
        let d = tempfile::tempdir().unwrap();
        let s = BotStore::open(d.path()).unwrap();
        (d, s)
    }

    fn call(id: &str) -> Message {
        Message::assistant(vec![Content::ToolUse {
            id: ToolUseId::new(id),
            name: "fs.read".into(),
            input: serde_json::json!({}),
        }])
    }

    fn result(id: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::ToolResult {
                id: ToolUseId::new(id),
                content: "ok".into(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn a_window_that_is_already_legal_is_returned_untouched() {
        // The load-bearing test of the four. Every other assertion about
        // `repair_window` is satisfied by a function that returns nothing at
        // all, and a repair that quietly ate the conversation would look like a
        // fixed bug and be a much worse one: the Bot would answer every task
        // having forgotten the last forty messages, which is the property this
        // crate exists to provide.
        let window = vec![
            Message::user("do the thing"),
            call("t1"),
            result("t1"),
            Message::assistant(vec![Content::text("done")]),
        ];
        assert_eq!(
            BotStore::repair_window(window.clone()),
            window,
            "a well-formed window must survive the repair unchanged"
        );
    }

    #[test]
    fn a_result_whose_call_was_cut_off_the_front_is_dropped() {
        // The shipped bug: with one tool call per turn this is every third
        // window size, and DEFAULT_HISTORY of 40 was one of them.
        let repaired = BotStore::repair_window(vec![
            result("t0"),
            Message::user("next task"),
            call("t1"),
            result("t1"),
        ]);
        assert_eq!(
            repaired,
            vec![Message::user("next task"), call("t1"), result("t1")],
            "the orphan goes and the intact pair behind it stays"
        );
    }

    #[test]
    fn a_call_left_unanswered_at_the_end_is_dropped_too() {
        // The mirror case, which `agent_loop.rs` guards on the write side: a
        // vendor rejects a request whose tool calls are unanswered. A run
        // cancelled mid-call, or a line that would not parse, can leave one
        // here on the read side where nothing was checking.
        let repaired = BotStore::repair_window(vec![
            Message::user("task"),
            call("t1"),
            result("t1"),
            call("t2"),
        ]);
        assert_eq!(
            repaired,
            vec![Message::user("task"), call("t1"), result("t1")],
            "the dangling call goes; everything answered stays"
        );
    }

    #[test]
    fn an_orphan_in_the_middle_costs_only_itself() {
        // Both read paths skip a line they cannot parse, so an orphan can
        // appear anywhere, not only at the cut. Truncating to the first legal
        // message would be the easy repair and would throw away every message
        // before the orphan - which is most of the context, to fix one block.
        let repaired = BotStore::repair_window(vec![
            Message::user("first"),
            call("t1"),
            result("t1"),
            result("t2"),
            Message::user("later"),
        ]);
        assert_eq!(
            repaired,
            vec![
                Message::user("first"),
                call("t1"),
                result("t1"),
                Message::user("later"),
            ],
            "history before a mid-window orphan must be kept"
        );
    }

    #[test]
    fn a_result_may_not_answer_a_call_that_comes_after_it() {
        // Presence is not enough; the call has to be earlier in the window.
        // Checking membership in the whole window rather than in the part
        // already seen would accept this, and a vendor would not.
        let repaired = BotStore::repair_window(vec![result("t1"), call("t1")]);
        assert!(
            repaired.is_empty(),
            "the result precedes its call and the call is then unanswered, so both go: {repaired:?}"
        );
    }

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![Content::text(text)],
        }
    }

    /// The id a client puts in a message has to reach a Bot.
    ///
    /// `bot.send`'s `to` goes through `resolve`, so an `@mention` picked from
    /// a menu is only useful if this accepts the form the menu inserted. The
    /// composer in `botroster-app/ui/main.js` inserts the id, which is also the
    /// form [`Group::owner_for`] requires; both paths must accept it or the
    /// same text means two different things in two conversations.
    #[test]
    fn a_bot_resolves_by_the_id_a_client_would_have_inserted() {
        let (_d, s) = store();
        let made = s.create("Talent Scout", "", "").unwrap();
        assert_eq!(
            made.id.as_str(),
            "talent-scout",
            "a two-word name should slug, or the two forms would not differ"
        );

        assert_eq!(
            s.resolve("talent-scout").unwrap().id,
            made.id,
            "the id a menu inserts did not reach the Bot"
        );
        // The display name must also work: a person types it and the model
        // may repeat it.
        assert_eq!(s.resolve("Talent Scout").unwrap().id, made.id);
        assert!(
            s.resolve("talent").is_err(),
            "a truncated mention must not land on some other Bot"
        );
    }

    /// Deleting a Bot must not leave a group pointing at it.
    ///
    /// Groups live outside the Bot's directory. If the membership list still
    /// named a deleted Bot, `owner_for` would hand every post to it and the
    /// group would fail on every turn.
    #[test]
    fn deleting_a_bot_takes_it_out_of_the_groups_that_hold_it() {
        let (_d, s) = store();
        let a = s.create("Talent Scout", "", "").unwrap();
        let b = s.create("Writer", "", "").unwrap();
        let g = s
            .create_group("Launch", &[a.id.clone(), b.id.clone()])
            .unwrap();

        let gone = s.delete(&a.id).unwrap();
        assert_eq!(gone.left, vec!["Launch".to_owned()]);
        assert!(gone.emptied.is_empty(), "the group still has a member");

        let after = s.get_group(&g.id).expect("the group survived");
        assert_eq!(
            after.members,
            vec![b.id.clone()],
            "the deleted Bot is still a member: {after:?}"
        );
        // Somebody has to answer.
        assert_eq!(after.owner_for("anything?"), Some(&b.id));
    }

    /// A group with nobody in it is a thread nothing can answer, and a sidebar
    /// entry that fails whenever it is opened.
    ///
    /// Reached only by deletion: `create_group` requires two to six members.
    /// A group of one is kept: that member answers everything and the thread
    /// is still readable, and destroying a conversation because somebody left
    /// is worse than a small group.
    #[test]
    fn a_group_whose_last_member_is_deleted_goes_too() {
        let (_d, s) = store();
        let only = s.create("Talent Scout", "", "").unwrap();
        let other = s.create("Writer", "", "").unwrap();
        let g = s
            .create_group("Launch", &[only.id.clone(), other.id.clone()])
            .unwrap();

        let down_to_one = s.delete(&other.id).unwrap();
        assert_eq!(down_to_one.left, vec!["Launch".to_owned()]);
        assert!(
            s.get_group(&g.id).is_ok(),
            "a group of one is still a conversation somebody can read"
        );

        let gone = s.delete(&only.id).unwrap();
        assert_eq!(gone.emptied, vec!["Launch".to_owned()]);
        assert!(gone.left.is_empty(), "it was emptied, not left");
        assert!(
            s.get_group(&g.id).is_err(),
            "an empty group was kept, and opening it fails every time"
        );
    }

    /// Deleting is irreversible, so it reports what it destroyed, including
    /// any routine that will not run again.
    #[test]
    fn a_deletion_reports_the_work_it_destroyed() {
        let (_d, s) = store();
        let bot = s.create("Talent Scout", "", "").unwrap();
        s.append(&bot.id, &[msg(Role::User, "shortlist?")]).unwrap();

        let gone = s.delete(&bot.id).unwrap();
        assert_eq!(gone.messages, 1, "the conversation was not counted");
        assert_eq!(gone.routines, 0);
    }

    /// A rename changes what a Bot is called, not its id.
    ///
    /// The id is the key its conversation, inbox, groups and routines are
    /// stored under. Moving it would orphan every one of them.
    #[test]
    fn renaming_a_bot_keeps_its_id_and_its_conversation() {
        let (_d, s) = store();
        let made = s
            .create("Talent Scout", "recruiting", "finds people")
            .unwrap();
        s.append(&made.id, &[msg(Role::User, "who did we shortlist?")])
            .unwrap();

        let after = s.rename("talent-scout", "Recruiting").unwrap();
        assert_eq!(after.id, made.id, "the id moved, so its work is orphaned");
        assert_eq!(after.name, "Recruiting");
        assert_eq!(
            s.history(&made.id, Some(10)).unwrap().len(),
            1,
            "the conversation did not survive the rename"
        );

        // And it answers to the new name, which is the point of renaming.
        assert_eq!(s.resolve("Recruiting").unwrap().id, made.id);
        // The id still works too: it is what groups and routines hold.
        assert_eq!(s.resolve("talent-scout").unwrap().id, made.id);
    }

    /// Two Bots sharing a display name must be refused, not guessed between.
    ///
    /// The API does not produce this state (`create` and `rename` both refuse
    /// a taken name), so it is built here by writing the profile directly: a
    /// store on disk can be hand-edited or restored from a backup. Handing
    /// work to whichever Bot the filesystem listed first is the silent wrong
    /// answer this store refuses everywhere else.
    #[test]
    fn a_name_that_means_two_bots_is_refused_rather_than_guessed() {
        let (_d, s) = store();
        let a = s.create("Talent Scout", "", "").unwrap();
        let mut b = s.create("Payments API", "", "").unwrap();
        b.name = "Talent Scout".to_owned();
        s.save(&b).unwrap();

        let err = s
            .resolve("Talent Scout")
            .expect_err("two Bots answer to that name");
        let said = err.to_string();
        assert!(
            said.contains(a.id.as_str()) && said.contains(b.id.as_str()),
            "the refusal has to name both, or there is no way to pick: {said}"
        );

        // Each is still reachable the unambiguous way.
        assert_eq!(s.resolve(a.id.as_str()).unwrap().id, a.id);
        assert_eq!(s.resolve(b.id.as_str()).unwrap().id, b.id);
    }

    /// A settings form sends the field it edited. If `None` cleared the other
    /// one, editing a title would silently wipe a description somebody wrote.
    #[test]
    fn describing_a_bot_leaves_the_field_it_was_not_given_alone() {
        let (_d, s) = store();
        let made = s
            .create("Ledger", "bookkeeping", "closes the month")
            .unwrap();

        let after = s.describe(&made.id.0, Some("accounting"), None).unwrap();
        assert_eq!(after.title, "accounting");
        assert_eq!(
            after.description, "closes the month",
            "the description was cleared by an edit that never mentioned it"
        );

        // Clearing is possible, but only by asking for it.
        let cleared = s.describe(&made.id.0, None, Some("")).unwrap();
        assert_eq!(cleared.title, "accounting");
        assert!(cleared.description.is_empty());

        // Persisted, not just returned.
        assert_eq!(s.get(&made.id).unwrap().title, "accounting");
    }

    #[test]
    fn a_retried_task_is_not_asked_twice() {
        // A run that fails leaves its task in the log with nothing answering
        // it; the retry restates the same task. Stored as-is, the model would
        // read the same instruction twice in a row, and more across a long
        // outage.
        let (_d, s) = store();
        let b = s.create("Writer", "", "").unwrap().id;
        let task = msg(Role::User, "write the notes");

        // Attempt one: dies before the model answers.
        s.append(&b, std::slice::from_ref(&task)).unwrap();
        // Attempt two: same task, restated.
        s.append(&b, std::slice::from_ref(&task)).unwrap();

        let h = s.history(&b, None).unwrap();
        assert_eq!(h.len(), 1, "the task was stored once per attempt: {h:?}");
    }

    #[test]
    fn asking_the_same_thing_after_an_answer_is_a_real_second_ask() {
        // The narrow rule matters: only a repeat with *nothing between* is a
        // restatement. Once the Bot has answered, asking again is somebody
        // asking again, and dropping it would lose a real exchange.
        let (_d, s) = store();
        let b = s.create("Writer", "", "").unwrap().id;
        let task = msg(Role::User, "what is the invoice number?");

        s.append(&b, std::slice::from_ref(&task)).unwrap();
        s.append(&b, &[msg(Role::Assistant, "8891")]).unwrap();
        s.append(&b, std::slice::from_ref(&task)).unwrap();

        let h = s.history(&b, None).unwrap();
        assert_eq!(h.len(), 3, "a genuine second question was swallowed: {h:?}");
    }

    #[test]
    fn a_retry_keeps_the_work_the_failed_attempt_did() {
        // Only the duplicate question is dropped, not the attempt. Whatever
        // the failed run got through stays, which lets the retry carry on
        // instead of starting over.
        let (_d, s) = store();
        let b = s.create("Writer", "", "").unwrap().id;
        let task = msg(Role::User, "write the notes");

        s.append(
            &b,
            &[
                task.clone(),
                msg(Role::Assistant, "wrote part one"),
                msg(Role::User, "write the notes"),
            ],
        )
        .unwrap();
        s.append(&b, &[task.clone(), msg(Role::Assistant, "done")])
            .unwrap();

        let h = s.history(&b, None).unwrap();
        let texts: Vec<String> = h.iter().map(|m| m.text()).collect();
        assert_eq!(
            texts,
            [
                "write the notes",
                "wrote part one",
                "write the notes",
                "done"
            ],
            "the retry lost the partial work or kept the duplicate: {texts:?}"
        );
    }

    #[test]
    fn names_become_stable_readable_slugs() {
        assert_eq!(
            BotStore::slug("Account Health").unwrap().0,
            "account-health"
        );
        assert_eq!(BotStore::slug("  Bug   Repro  ").unwrap().0, "bug-repro");
        assert_eq!(BotStore::slug("Piper").unwrap().0, "piper");
        assert_eq!(BotStore::slug("R2-D2").unwrap().0, "r2-d2");
        // Punctuation collapses rather than producing a run of dashes.
        assert_eq!(
            BotStore::slug("Sales!!! Outbound").unwrap().0,
            "sales-outbound"
        );
        assert!(BotStore::slug("!!!").is_err());
        assert!(BotStore::slug("").is_err());
    }

    #[test]
    fn a_slug_cannot_escape_the_store() {
        let (d, s) = store();
        let b = s.create("../../etc/passwd", "", "").unwrap();
        assert!(s.dir(&b.id).starts_with(d.path()));
        assert_eq!(b.id.0, "etc-passwd");
    }

    #[test]
    fn a_bot_round_trips_and_is_found_by_name_or_id() {
        let (_d, s) = store();
        s.create("Account Health", "Renewal risk", "Never email a customer.")
            .unwrap();

        let by_name = s.resolve("Account Health").unwrap();
        let by_id = s.resolve("account-health").unwrap();
        assert_eq!(by_name, by_id);
        assert_eq!(by_name.description, "Never email a customer.");
    }

    #[test]
    fn creating_the_same_name_twice_is_refused() {
        let (_d, s) = store();
        s.create("Piper", "", "").unwrap();
        assert!(matches!(
            s.create("Piper", "", ""),
            Err(BotError::Duplicate(_))
        ));
        // And the differently-spelled but identically-slugged name too.
        assert!(matches!(
            s.create("piper", "", ""),
            Err(BotError::Duplicate(_))
        ));
    }

    #[test]
    fn the_roster_is_capped() {
        let (_d, s) = store();
        for i in 0..MAX_BOTS {
            s.create(&format!("bot {i}"), "", "").unwrap();
        }
        assert!(matches!(
            s.create("one too many", "", ""),
            Err(BotError::TooMany)
        ));
    }

    #[test]
    fn history_survives_reopening_the_store() {
        let (d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        s.append(
            &b.id,
            &[msg(Role::User, "hello"), msg(Role::Assistant, "hi")],
        )
        .unwrap();
        drop(s);

        // A new process, the same disk: this is the whole promise.
        let again = BotStore::open(d.path()).unwrap();
        let h = again.history(&b.id, None).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].text(), "hello");
        assert_eq!(h[1].role, Role::Assistant);
    }

    /// Two writers must not splice one message into the middle of another.
    ///
    /// `writeln!(f, "{line}")` is two writes (`write_fmt` passes the
    /// formatter's pieces to `write_all` separately), so the text and its
    /// newline reach the file apart, and a writer appending in between lands
    /// inside the first line. Both messages are then unparseable and skipped
    /// by the reader.
    ///
    /// This happens in practice: a routine fires on a schedule while somebody
    /// is talking to the same Bot in the window, two processes appending to
    /// one `conversation.jsonl` with nothing in this crate locking.
    ///
    /// Threads rather than processes, because the hazard is the write pattern
    /// rather than the process boundary: an append-mode handle behaves the
    /// same way either side of it.
    #[test]
    fn appends_from_two_writers_do_not_splice() {
        const WRITERS: usize = 8;
        const EACH: usize = 300;

        let (d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        let root = d.path().to_path_buf();
        let id = b.id.clone();

        std::thread::scope(|scope| {
            for w in 0..WRITERS {
                let root = root.clone();
                let id = id.clone();
                scope.spawn(move || {
                    let mine = BotStore::open(&root).unwrap();
                    for i in 0..EACH {
                        // Lengths vary so a splice lands in different places;
                        // a fixed length can leave the seam on a boundary that
                        // happens to survive.
                        let text = format!("w{w}-{i}-{}", "x".repeat(40 + (i % 400)));
                        mine.append(&id, &[msg(Role::User, &text)]).unwrap();
                    }
                });
            }
        });

        let raw = std::fs::read_to_string(s.log_path(&id)).unwrap();
        let spliced: Vec<&str> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<Message>(l).is_err())
            .collect();
        assert!(
            spliced.is_empty(),
            "{} of {} lines were spliced together by concurrent appends; first: {:?}",
            spliced.len(),
            raw.lines().count(),
            spliced.first().map(|l| &l[..l.len().min(120)])
        );
        assert_eq!(
            s.history(&id, None).unwrap().len(),
            WRITERS * EACH,
            "messages went missing even though every line parsed"
        );
    }

    #[test]
    fn appending_is_incremental_not_a_rewrite() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        for i in 0..20 {
            s.append(&b.id, &[msg(Role::User, &format!("m{i}"))])
                .unwrap();
        }
        assert_eq!(s.message_count(&b.id).unwrap(), 20);
        let h = s.history(&b.id, None).unwrap();
        assert_eq!(h[0].text(), "m0");
        assert_eq!(h[19].text(), "m19");
    }

    #[test]
    fn a_truncated_final_line_costs_one_message_not_the_history() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        s.append(
            &b.id,
            &[msg(Role::User, "first"), msg(Role::User, "second")],
        )
        .unwrap();
        // Simulate a crash mid-append.
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(s.log_path(&b.id))
            .unwrap();
        write!(f, "{{\"role\":\"user\",\"cont").unwrap();
        drop(f);

        let h = s.history(&b.id, None).unwrap();
        assert_eq!(
            h.len(),
            2,
            "a partial line took the whole conversation with it"
        );
        assert_eq!(h[1].text(), "second");
    }

    /// The same crash, on the limited path every run takes.
    ///
    /// The test above reads the whole log. Every run reads the recent end of
    /// it through `history(id, Some(n))`, which seeks backwards through
    /// `tail_lines` rather than parsing from the start, a different code path
    /// with its own way to lose a conversation.
    #[test]
    fn a_truncated_final_line_costs_one_message_on_the_limited_path_too() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        for i in 0..5 {
            s.append(&b.id, &[msg(Role::User, &format!("m{i}"))])
                .unwrap();
        }
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(s.log_path(&b.id))
            .unwrap();
        write!(f, "{{\"role\":\"user\",\"cont").unwrap();
        drop(f);

        let h = s.history(&b.id, Some(3)).unwrap();
        assert!(
            h.iter().any(|m| m.text() == "m4"),
            "the last real message was lost behind the partial one: {h:?}"
        );
        assert!(
            h.iter().all(|m| m.text().starts_with('m')),
            "a fragment was parsed as a message: {h:?}"
        );
    }

    /// A log longer than the read window.
    ///
    /// `tail_lines` starts with 64KB and doubles until it has enough lines.
    /// The window almost never lands on a line boundary, so the first line it
    /// sees is usually a fragment and is dropped. Dropping a whole message
    /// every time would look the same unless the count is exact, so this asks
    /// for the last N of a long history and checks that every one is present
    /// and in order.
    #[test]
    fn a_history_larger_than_the_read_window_still_ends_correctly() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        // Well past 64KB: each message carries a kilobyte of padding.
        let padding = "x".repeat(1024);
        for i in 0..120 {
            s.append(&b.id, &[msg(Role::User, &format!("m{i} {padding}"))])
                .unwrap();
        }

        let h = s.history(&b.id, Some(40)).unwrap();
        assert_eq!(h.len(), 40, "the window did not grow to hold the ask");
        for (n, m) in h.iter().enumerate() {
            let expected = format!("m{}", 80 + n);
            assert!(
                m.text().starts_with(&expected),
                "message {n} of the tail is {:?}, expected {expected}",
                &m.text()[..expected.len().min(m.text().len())]
            );
        }
    }

    /// Every line the tail returns is a whole one.
    ///
    /// The window `tail_lines` reads almost never lands on a line boundary, so
    /// its first line is usually a fragment and is dropped. That drop is only
    /// observable when the answer is exactly one window's worth of lines; any
    /// smaller ask takes from the end and never reaches the fragment. The
    /// boundary is therefore derived from the log rather than hand-picked, so
    /// the ask that can expose it is computed rather than guessed.
    #[test]
    fn the_tail_never_returns_half_a_line() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        let padding = "x".repeat(1024);
        for i in 0..120 {
            s.append(&b.id, &[msg(Role::User, &format!("m{i} {padding}"))])
                .unwrap();
        }
        let path = s.log_path(&b.id);

        let bytes = fs::read(&path).unwrap();
        let from = bytes.len().saturating_sub(TAIL_WINDOW as usize);
        assert!(
            from > 0,
            "the log must exceed one window for this to mean anything"
        );
        let in_window = String::from_utf8_lossy(&bytes[from..]).lines().count();

        for want in [1, 8, in_window - 1, in_window, in_window + 1, 200] {
            for line in tail_lines(&path, want).unwrap() {
                assert!(
                    serde_json::from_str::<Message>(&line).is_ok(),
                    "asking for {want} of the {in_window} in the window returned a partial line: {:?}",
                    &line[..line.len().min(60)]
                );
            }
        }
    }

    #[test]
    fn history_can_be_limited_to_the_most_recent() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        for i in 0..10 {
            s.append(&b.id, &[msg(Role::User, &format!("m{i}"))])
                .unwrap();
        }
        let h = s.history(&b.id, Some(3)).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].text(), "m7", "the wrong end of the history was kept");
        assert_eq!(h[2].text(), "m9");
    }

    #[test]
    fn duplicating_carries_the_brief_but_not_the_conversation() {
        let (_d, s) = store();
        let a = s
            .create(
                "Account Health EU",
                "Renewals",
                "Escalate below 60% health.",
            )
            .unwrap();
        s.append(&a.id, &[msg(Role::User, "EU-only facts")])
            .unwrap();

        let b = s.duplicate(&a.id, "Account Health US").unwrap();
        assert_eq!(b.description, a.description);
        assert_eq!(b.title, a.title);
        assert_eq!(
            s.message_count(&b.id).unwrap(),
            0,
            "the copy inherited the original's history and will answer with the wrong region's facts"
        );
        // The original is untouched.
        assert_eq!(s.message_count(&a.id).unwrap(), 1);
    }

    #[test]
    fn hiding_keeps_everything_and_only_leaves_the_list() {
        let (_d, s) = store();
        let mut b = s.create("Piper", "", "").unwrap();
        s.append(&b.id, &[msg(Role::User, "work")]).unwrap();

        b.hidden = true;
        s.save(&b).unwrap();

        assert!(s.list(false).unwrap().is_empty());
        assert_eq!(s.list(true).unwrap().len(), 1);
        assert_eq!(s.message_count(&b.id).unwrap(), 1, "hiding destroyed work");
    }

    #[test]
    fn deleting_removes_the_bot_and_its_history() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "").unwrap();
        s.append(&b.id, &[msg(Role::User, "x")]).unwrap();
        s.delete(&b.id).unwrap();
        assert!(matches!(s.get(&b.id), Err(BotError::NotFound(_))));
        assert!(matches!(s.delete(&b.id), Err(BotError::NotFound(_))));
    }

    #[test]
    fn the_system_prompt_carries_identity_and_the_standing_brief() {
        let (_d, s) = store();
        let b = s
            .create(
                "Piper",
                "Product performance",
                "Never change production settings.",
            )
            .unwrap();
        let p = b.system_prompt("BASE RULES");

        assert!(p.starts_with("BASE RULES"));
        assert!(p.contains("You are **Piper**, Product performance."));
        assert!(p.contains("Never change production settings."));
        // The brief comes last, so it is the closest thing to the task.
        let brief_at = p.find("Never change").unwrap();
        let base_at = p.find("BASE RULES").unwrap();
        assert!(brief_at > base_at);
    }

    #[test]
    fn a_bot_with_no_brief_still_gets_a_clean_prompt() {
        let (_d, s) = store();
        let b = s.create("Scout", "", "").unwrap();
        let p = b.system_prompt("BASE");
        assert!(p.contains("You are **Scout**."));
        assert!(!p.contains("standing brief"));
    }

    #[test]
    fn listing_survives_one_damaged_bot() {
        let (_d, s) = store();
        s.create("Good One", "", "").unwrap();
        let broken = BotId("broken".into());
        fs::create_dir_all(s.dir(&broken)).unwrap();
        fs::write(s.profile_path(&broken), "{ not json").unwrap();

        let all = s.list(true).unwrap();
        assert_eq!(all.len(), 1, "a corrupt bot hid the healthy ones");
        assert_eq!(all[0].name, "Good One");
    }

    #[test]
    fn clearing_history_keeps_the_bot() {
        let (_d, s) = store();
        let b = s.create("Piper", "", "brief").unwrap();
        s.append(&b.id, &[msg(Role::User, "x")]).unwrap();
        s.clear_history(&b.id).unwrap();
        assert_eq!(s.message_count(&b.id).unwrap(), 0);
        assert_eq!(s.get(&b.id).unwrap().description, "brief");
        // Idempotent.
        s.clear_history(&b.id).unwrap();
    }
}

// ── handoff ───────────────────────────────────────────────────────────

/// A message from one Bot to another, waiting to be picked up.
///
/// Delivery is asynchronous. A synchronous call would make the sender wait on
/// the receiver's whole run, and a receiver that is busy, or itself waiting on
/// an approval, would stall the sender indefinitely. An inbox decouples them:
/// the sender finishes, the receiver picks the work up when it next runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    /// Who sent it. `None` for a message from a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<BotId>,
    pub text: String,
    /// Delivery order, so a drained inbox reads in the order it was written.
    pub seq: u64,
}

impl BotStore {
    fn inbox_path(&self, id: &BotId) -> PathBuf {
        self.dir(id).join("inbox.jsonl")
    }

    /// Hand work to another Bot.
    ///
    /// Fails if the recipient does not exist: a handoff into the void is
    /// silent data loss, and the sender should be told so it can say so.
    pub fn send(&self, from: Option<&BotId>, to: &BotId, text: &str) -> Result<Handoff> {
        if !self.profile_path(to).exists() {
            return Err(BotError::NotFound(to.0.clone()));
        }
        if let Some(f) = from {
            if f == to {
                return Err(BotError::SelfHandoff);
            }
        }
        let seq = self.inbox(to)?.last().map_or(0, |h| h.seq + 1);
        let h = Handoff {
            from: from.cloned(),
            text: text.to_owned(),
            seq,
        };
        append_lines(
            &self.inbox_path(to),
            [serde_json::to_string(&h).expect("handoff serialises")],
        )?;
        Ok(h)
    }

    /// Everything waiting for a Bot, oldest first. Non-destructive.
    pub fn inbox(&self, id: &BotId) -> Result<Vec<Handoff>> {
        let p = self.inbox_path(id);
        if !p.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for line in BufReader::new(fs::File::open(&p)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Handoff>(&line) {
                Ok(h) => out.push(h),
                Err(e) => tracing::warn!(error = %e, "skipping an unreadable inbox line"),
            }
        }
        Ok(out)
    }

    /// Take everything waiting, clearing the inbox.
    ///
    /// The read and the clear are one step: draining in two would drop
    /// anything that arrived between them.
    pub fn drain_inbox(&self, id: &BotId) -> Result<Vec<Handoff>> {
        let p = self.inbox_path(id);
        if !p.exists() {
            return Ok(Vec::new());
        }
        // Rename first, then read: a message delivered after this instant
        // lands in a fresh file rather than being read and then deleted.
        let taken = p.with_extension("jsonl.taken");
        let _ = fs::remove_file(&taken);
        fs::rename(&p, &taken)?;

        let mut out = Vec::new();
        for line in BufReader::new(fs::File::open(&taken)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(h) = serde_json::from_str::<Handoff>(&line) {
                out.push(h);
            }
        }
        let _ = fs::remove_file(&taken);
        Ok(out)
    }

    /// Recover a drain interrupted between the rename and the read.
    ///
    /// Without this, a crash at that instant loses every message in flight.
    ///
    /// Recovery must itself survive being interrupted. Writing the merge and
    /// deleting `taken` are two steps; a machine that stops between them
    /// leaves an inbox that already holds the recovered messages and a `taken`
    /// file that says to put them back, and a naive second run would deliver
    /// the same handoff twice.
    ///
    /// `seq` cannot detect this: it is assigned from whatever is in the inbox
    /// at the time, so it restarts at 0 after every drain and two unrelated
    /// handoffs can share one. The shape of the merge settles it instead: the
    /// `taken` bytes are written first, so a completed merge leaves an inbox
    /// that begins with exactly those bytes, and a second run is a no-op.
    pub fn recover_inbox(&self, id: &BotId) -> Result<()> {
        let p = self.inbox_path(id);
        let taken = p.with_extension("jsonl.taken");
        if !taken.exists() {
            return Ok(());
        }
        let recovered = fs::read_to_string(&taken)?;
        let current = if p.exists() {
            fs::read_to_string(&p)?
        } else {
            String::new()
        };
        if !current.starts_with(&recovered) {
            // Through a temporary and a rename, like every other durable
            // write in this file. A partial write here is not one lost
            // message but a truncated inbox, produced by the code that exists
            // to save it.
            //
            // No test covers the atomicity: nothing in this suite stops a
            // process mid-write, so a plain `fs::write(&p, ..)` would leave
            // every test green. Do not remove the temporary as ceremony.
            let tmp = p.with_extension("jsonl.recovering");
            fs::write(&tmp, format!("{recovered}{current}"))?;
            fs::rename(&tmp, &p)?;
        }
        fs::remove_file(&taken)?;
        Ok(())
    }

    /// Render waiting handoffs as a preamble for the receiving Bot's task.
    pub fn handoff_preamble(handoffs: &[Handoff]) -> String {
        if handoffs.is_empty() {
            return String::new();
        }
        let mut s = String::from("While you were away, these arrived:\n\n");
        for h in handoffs {
            match &h.from {
                Some(f) => s.push_str(&format!("- from **{f}**: {}\n", h.text)),
                None => s.push_str(&format!("- {}\n", h.text)),
            }
        }
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BotStore) {
        let d = tempfile::tempdir().unwrap();
        let s = BotStore::open(d.path()).unwrap();
        (d, s)
    }

    #[test]
    fn a_handoff_waits_until_the_recipient_next_runs() {
        let (_d, s) = store();
        let a = s.create("Researcher", "", "").unwrap();
        let b = s.create("Writer", "", "").unwrap();

        s.send(Some(&a.id), &b.id, "sources are in /workspace/refs")
            .unwrap();
        assert_eq!(s.inbox(&b.id).unwrap().len(), 1);
        assert!(
            s.inbox(&a.id).unwrap().is_empty(),
            "the sender's own inbox changed"
        );

        let drained = s.drain_inbox(&b.id).unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].from.as_ref().unwrap(), &a.id);
        assert!(drained[0].text.contains("/workspace/refs"));
        assert!(
            s.inbox(&b.id).unwrap().is_empty(),
            "a drained handoff came back"
        );
    }

    #[test]
    fn a_handoff_to_a_bot_that_does_not_exist_is_refused() {
        let (_d, s) = store();
        let a = s.create("Researcher", "", "").unwrap();
        // Silently dropping this would lose work and tell nobody.
        assert!(matches!(
            s.send(Some(&a.id), &BotId("ghost".into()), "hello"),
            Err(BotError::NotFound(_))
        ));
    }

    #[test]
    fn a_bot_cannot_hand_off_to_itself() {
        let (_d, s) = store();
        let a = s.create("Researcher", "", "").unwrap();
        // A self-handoff is a loop that burns tokens until the step budget.
        assert!(matches!(
            s.send(Some(&a.id), &a.id, "do it again"),
            Err(BotError::SelfHandoff)
        ));
    }

    #[test]
    fn handoffs_keep_their_order() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        for i in 0..5 {
            s.send(Some(&a.id), &b.id, &format!("step {i}")).unwrap();
        }
        let d = s.drain_inbox(&b.id).unwrap();
        assert_eq!(d.len(), 5);
        assert_eq!(d[0].text, "step 0");
        assert_eq!(d[4].text, "step 4");
        assert!(d.windows(2).all(|w| w[0].seq < w[1].seq));
    }

    #[test]
    fn a_message_arriving_mid_drain_is_not_lost() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "first").unwrap();

        let drained = s.drain_inbox(&b.id).unwrap();
        s.send(Some(&a.id), &b.id, "second").unwrap();

        assert_eq!(drained.len(), 1);
        assert_eq!(s.inbox(&b.id).unwrap()[0].text, "second");
    }

    /// Recovering twice delivers once.
    ///
    /// Recovery writes the inbox back and then deletes the `taken` file. A
    /// machine stopping between those two steps leaves both files, and a
    /// second run must not deliver the same handoff again: a Bot told the
    /// same thing twice acts on it twice.
    ///
    /// The state after that interruption is built exactly: the inbox written
    /// back, `taken` still present. A recovery that does nothing at all is
    /// ruled out by `recovery_puts_taken_messages_before_newer_ones`; the
    /// pair together is the property.
    #[test]
    fn recovering_an_inbox_twice_delivers_it_once() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "important").unwrap();
        let p = s.inbox_path(&b.id);
        let taken = p.with_extension("jsonl.taken");
        fs::rename(&p, &taken).unwrap();

        fs::write(&p, fs::read_to_string(&taken).unwrap()).unwrap();
        assert!(taken.exists(), "the state being tested needs both files");

        s.recover_inbox(&b.id).unwrap();
        let back = s.inbox(&b.id).unwrap();
        assert_eq!(
            back.len(),
            1,
            "an interrupted recovery delivered the same handoff again: {:?}",
            back.iter().map(|h| (h.seq, &h.text)).collect::<Vec<_>>()
        );
        assert_eq!(back[0].text, "important");
        assert!(!taken.exists(), "the recovery marker was left behind");
    }

    #[test]
    fn an_interrupted_drain_is_recovered_rather_than_lost() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "important").unwrap();

        // Simulate a crash between the rename and the read.
        let p = s.inbox_path(&b.id);
        fs::rename(&p, p.with_extension("jsonl.taken")).unwrap();
        assert!(s.inbox(&b.id).unwrap().is_empty());

        s.recover_inbox(&b.id).unwrap();
        let back = s.inbox(&b.id).unwrap();
        assert_eq!(back.len(), 1, "an in-flight handoff was lost to a crash");
        assert_eq!(back[0].text, "important");
    }

    #[test]
    fn recovery_puts_taken_messages_before_newer_ones() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "older").unwrap();
        let p = s.inbox_path(&b.id);
        fs::rename(&p, p.with_extension("jsonl.taken")).unwrap();
        s.send(Some(&a.id), &b.id, "newer").unwrap();

        s.recover_inbox(&b.id).unwrap();
        let back = s.inbox(&b.id).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].text, "older", "recovery reordered the inbox");
        assert_eq!(back[1].text, "newer");
    }

    #[test]
    fn a_person_can_hand_work_to_a_bot_too() {
        let (_d, s) = store();
        let b = s.create("Writer", "", "").unwrap();
        s.send(None, &b.id, "please draft the launch note").unwrap();
        let d = s.drain_inbox(&b.id).unwrap();
        assert!(d[0].from.is_none());
    }

    #[test]
    fn the_preamble_names_the_sender_so_the_model_knows_who_asked() {
        let (_d, s) = store();
        let a = s.create("Researcher", "", "").unwrap();
        let b = s.create("Writer", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "sources ready").unwrap();
        s.send(None, &b.id, "and hurry").unwrap();

        let text = BotStore::handoff_preamble(&s.drain_inbox(&b.id).unwrap());
        assert!(
            text.contains("from **researcher**: sources ready"),
            "got: {text}"
        );
        assert!(text.contains("- and hurry"));
        assert!(BotStore::handoff_preamble(&[]).is_empty());
    }

    #[test]
    fn a_handoff_to_a_deleted_bot_is_refused() {
        let (_d, s) = store();
        let a = s.create("A", "", "").unwrap();
        let b = s.create("B", "", "").unwrap();
        s.send(Some(&a.id), &b.id, "x").unwrap();
        s.delete(&b.id).unwrap();
        assert!(matches!(
            s.send(Some(&a.id), &b.id, "y"),
            Err(BotError::NotFound(_))
        ));
    }
}

// ── groups ────────────────────────────────────────────────────────────

/// Smallest useful group. Two Bots and a person is a conversation; one Bot is
/// just a Bot.
pub const MIN_GROUP: usize = 2;
/// Beyond this nobody can follow who owns what. The product this follows
/// stops at the same number.
pub const MAX_GROUP: usize = 6;

/// What deleting a Bot destroyed.
///
/// Returned rather than logged so the caller can report it: which
/// conversation and routines went with the Bot, and which groups changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deleted {
    /// Messages in the conversation that was destroyed.
    pub messages: usize,
    /// Routines that will not run again.
    pub routines: usize,
    /// Groups it was taken out of, by name.
    pub left: Vec<String>,
    /// Groups deleted because it was the last member, by name.
    pub emptied: Vec<String>,
}

/// Several Bots on one shared thread.
///
/// The transcript is shared; the Bots are not. A group turn runs one owner,
/// chosen by an `@mention` or defaulting to the first member. Fanning a
/// message out to everyone produces duplicate work and a thread nobody can
/// read; one owner per stage keeps the handoffs legible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: BotId,
    pub name: String,
    pub members: Vec<BotId>,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub hidden: bool,
}

impl Group {
    /// Who should answer this message.
    ///
    /// An `@mention` names the owner. Without one the first member answers
    /// (the coordinator, in the pattern the docs describe). `None` when a
    /// mention names somebody outside the group, so the caller can say so
    /// rather than silently handing the work to the wrong Bot.
    pub fn owner_for(&self, message: &str) -> Option<&BotId> {
        match mentions(message).first() {
            Some(m) => self.members.iter().find(|b| b.as_str() == m),
            None => self.members.first(),
        }
    }
}

/// Extract `@name` mentions, lowercased to match Bot ids.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        if c != '@' {
            continue;
        }
        // An `@` inside a word is an email address, not a mention.
        if text[..i]
            .chars()
            .next_back()
            .is_some_and(|p| p.is_alphanumeric())
        {
            continue;
        }
        let name: String = text[i + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name.to_ascii_lowercase());
        }
    }
    out
}

impl BotStore {
    fn group_dir(&self, id: &BotId) -> PathBuf {
        self.root.join("groups").join(&id.0)
    }
    fn group_profile(&self, id: &BotId) -> PathBuf {
        self.group_dir(id).join("group.json")
    }
    fn group_log(&self, id: &BotId) -> PathBuf {
        self.group_dir(id).join("conversation.jsonl")
    }

    pub fn create_group(&self, name: &str, members: &[BotId]) -> Result<Group> {
        let id = Self::slug(name)?;
        if self.group_profile(&id).exists() {
            return Err(BotError::Duplicate(name.to_owned()));
        }
        let mut deduped: Vec<BotId> = Vec::new();
        for m in members {
            if !deduped.contains(m) {
                deduped.push(m.clone());
            }
        }
        if deduped.len() < MIN_GROUP || deduped.len() > MAX_GROUP {
            return Err(BotError::BadGroupSize(deduped.len()));
        }
        // Every member must exist, or the group has a hole that only shows up
        // when somebody is mentioned.
        for m in &deduped {
            if !self.profile_path(m).exists() {
                return Err(BotError::NotFound(m.0.clone()));
            }
        }
        // Groups share the roster cap with Bots.
        if self.list(true)?.len() + self.groups(true)?.len() >= MAX_BOTS {
            return Err(BotError::TooMany);
        }
        let seq = self
            .groups(true)?
            .iter()
            .map(|g| g.seq)
            .max()
            .map_or(0, |m| m + 1);

        let g = Group {
            id: id.clone(),
            name: name.to_owned(),
            members: deduped,
            seq,
            hidden: false,
        };
        fs::create_dir_all(self.group_dir(&id))?;
        self.save_group(&g)?;
        Ok(g)
    }

    pub fn save_group(&self, g: &Group) -> Result<()> {
        fs::create_dir_all(self.group_dir(&g.id))?;
        let path = self.group_profile(&g.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(g).expect("group serialises"),
        )?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get_group(&self, id: &BotId) -> Result<Group> {
        let s = fs::read_to_string(self.group_profile(id))
            .map_err(|_| BotError::NotFound(id.0.clone()))?;
        serde_json::from_str(&s).map_err(|e| BotError::Corrupt(e.to_string()))
    }

    pub fn resolve_group(&self, name_or_id: &str) -> Result<Group> {
        let id = BotId(name_or_id.to_owned());
        if self.group_profile(&id).exists() {
            return self.get_group(&id);
        }
        let slug = Self::slug(name_or_id)?;
        if self.group_profile(&slug).exists() {
            return self.get_group(&slug);
        }
        Err(BotError::NotFound(name_or_id.to_owned()))
    }

    pub fn groups(&self, include_hidden: bool) -> Result<Vec<Group>> {
        let dir = self.root.join("groups");
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let id = BotId(e.file_name().to_string_lossy().into_owned());
            if let Ok(g) = self.get_group(&id) {
                if include_hidden || !g.hidden {
                    out.push(g);
                }
            }
        }
        out.sort_by_key(|g| g.seq);
        Ok(out)
    }

    pub fn delete_group(&self, id: &BotId) -> Result<()> {
        if !self.group_dir(id).exists() {
            return Err(BotError::NotFound(id.0.clone()));
        }
        fs::remove_dir_all(self.group_dir(id))?;
        Ok(())
    }

    /// Append to a group's shared transcript.
    pub fn append_group(&self, id: &BotId, messages: &[Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.group_dir(id))?;
        append_lines(
            &self.group_log(id),
            messages
                .iter()
                .map(|m| serde_json::to_string(m).expect("message serialises")),
        )
    }

    /// The shared transcript, oldest first.
    ///
    /// Reads from the end when a limit is given, for the reason
    /// [`history`](Self::history) does: a group thread grows without bound
    /// and a context window does not.
    pub fn group_history(&self, id: &BotId, limit: Option<usize>) -> Result<Vec<Message>> {
        let path = self.group_log(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        if let Some(n) = limit {
            return Ok(Self::parse_lines(&tail_lines(&path, n)?));
        }
        let mut out = Vec::new();
        for line in BufReader::new(fs::File::open(&path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(&line) {
                Ok(m) => out.push(m),
                Err(e) => tracing::warn!(error = %e, "skipping an unreadable group line"),
            }
        }
        Ok(out)
    }

    /// How many messages a group thread holds.
    ///
    /// Counts newlines rather than parsing. `botroster group ls` calls this once
    /// per group, so deserialising every message would make a sidebar refresh
    /// parse every message of every group.
    ///
    /// Counting newlines is also exact after a crash: a partial line has no
    /// newline yet, so it is not counted, which matches what parsing gives by
    /// skipping it.
    pub fn group_message_count(&self, id: &BotId) -> Result<usize> {
        let path = self.group_log(id);
        if !path.exists() {
            return Ok(0);
        }
        let mut f = BufReader::new(fs::File::open(&path)?);
        let mut n = 0usize;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let read = std::io::Read::read(&mut f, &mut buf)?;
            if read == 0 {
                return Ok(n);
            }
            n += bytecount(&buf[..read]);
        }
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use botroster_agent::model::{Content, Role};

    fn store() -> (tempfile::TempDir, BotStore) {
        let d = tempfile::tempdir().unwrap();
        let s = BotStore::open(d.path()).unwrap();
        (d, s)
    }

    fn three(s: &BotStore) -> Vec<BotId> {
        ["Coordinator", "Researcher", "Writer"]
            .iter()
            .map(|n| s.create(n, "", "").unwrap().id)
            .collect()
    }

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![Content::text(text)],
        }
    }

    #[test]
    fn mentions_are_found_but_email_addresses_are_not() {
        assert_eq!(mentions("@writer draft it"), vec!["writer"]);
        assert_eq!(
            mentions("hey @Writer and @researcher"),
            vec!["writer", "researcher"]
        );
        // An address is not a mention, and a bare @ is not either.
        assert!(mentions("mail ops@example.com about it").is_empty());
        assert!(mentions("cost is 5 @ each").is_empty());
        assert_eq!(mentions("@account-health please"), vec!["account-health"]);
    }

    /// A group thread is read from the end, like a Bot's, and still in order.
    #[test]
    fn a_group_thread_is_limited_to_its_most_recent() {
        let (_d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();
        for i in 0..50 {
            s.append_group(&g.id, &[msg(Role::User, &format!("m{i}"))])
                .unwrap();
        }

        let h = s.group_history(&g.id, Some(5)).unwrap();
        assert_eq!(h.len(), 5);
        for (n, m) in h.iter().enumerate() {
            assert_eq!(m.text(), format!("m{}", 45 + n), "out of order: {h:?}");
        }
        assert_eq!(s.group_history(&g.id, None).unwrap().len(), 50);
    }

    /// The count must not need to parse the thread, and must still be right
    /// after a crash.
    ///
    /// `botroster group ls` calls it once per group, so parsing would deserialise
    /// every message of every group on each listing. Counting newlines is
    /// also exact after a partial append, because a half-written line has no
    /// newline yet.
    #[test]
    fn a_group_count_is_exact_including_after_a_crash() {
        let (_d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();
        assert_eq!(s.group_message_count(&g.id).unwrap(), 0, "a new group");

        for i in 0..7 {
            s.append_group(&g.id, &[msg(Role::User, &format!("m{i}"))])
                .unwrap();
        }
        assert_eq!(s.group_message_count(&g.id).unwrap(), 7);

        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(s.group_log(&g.id))
            .unwrap();
        write!(f, "{{\"role\":\"user\",\"cont").unwrap();
        drop(f);

        assert_eq!(
            s.group_message_count(&g.id).unwrap(),
            7,
            "a partial line was counted as a message"
        );
        // The limited read returns one fewer message here by design:
        // `tail_lines` takes the last N lines, one of which is the fragment,
        // and `parse_lines` drops it. A Bot's history behaves identically.
        // What must hold is that nothing half-written is returned and the
        // newest real message is.
        let tail = s.group_history(&g.id, Some(3)).unwrap();
        assert!(
            tail.iter().any(|m| m.text() == "m6"),
            "the newest message was lost behind the partial one: {tail:?}"
        );
        assert!(
            tail.iter().all(|m| m.text().starts_with('m')),
            "a fragment was returned as a message: {tail:?}"
        );
    }

    #[test]
    fn a_mention_picks_the_owner_and_no_mention_picks_the_coordinator() {
        let (_d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();

        assert_eq!(g.owner_for("@writer draft it").unwrap().as_str(), "writer");
        // No mention: the first member owns it, which is the coordinator
        // pattern the docs describe.
        assert_eq!(
            g.owner_for("what is the status?").unwrap().as_str(),
            "coordinator"
        );
    }

    /// A mention has to be the id, and a two-word display name is not one.
    ///
    /// [`mentions`] takes characters while they are `[A-Za-z0-9_-]`, so a
    /// space ends the name. Any client offering an `@` menu has to insert the
    /// id rather than the display name; `@Talent Scout` resolves to `talent`
    /// and the turn is refused for naming somebody outside the group.
    /// `botroster-app/ui/main.js` inserts `el.dataset.mention` for this reason.
    #[test]
    fn a_display_name_with_a_space_does_not_resolve_but_the_id_does() {
        // Built by hand rather than through the store, because the ids are
        // the point: a Bot shown as "Talent Scout" is `talent-scout` on disk,
        // and that is the pair where the two forms differ. A fixture whose
        // ids are single words (`writer`, `researcher`) resolves either way
        // and proves nothing.
        let g = Group {
            id: BotId("launch".into()),
            name: "Launch".into(),
            members: vec![BotId("talent-scout".into()), BotId("payments-api".into())],
            seq: 0,
            hidden: false,
        };

        assert_eq!(
            g.owner_for("@talent-scout draft it").map(BotId::as_str),
            Some("talent-scout"),
            "the id is the form that works, hyphen and all"
        );

        // The same Bot, written the way a sidebar shows it.
        assert_eq!(
            mentions("@Talent Scout draft it"),
            vec!["talent".to_owned()],
            "a space ends the mention, so only the first word arrives"
        );
        assert_eq!(
            g.owner_for("@Talent Scout draft it").map(BotId::as_str),
            None,
            "the display name reached the resolver as `talent` and named nobody"
        );
    }

    #[test]
    fn mentioning_someone_outside_the_group_is_not_silently_redirected() {
        let (_d, s) = store();
        let ids = three(&s);
        s.create("Outsider", "", "").unwrap();
        let g = s.create_group("Launch", &ids).unwrap();
        // Quietly falling back to the coordinator would hand the work to the
        // wrong Bot and look like it succeeded.
        assert!(g.owner_for("@outsider do it").is_none());
    }

    #[test]
    fn a_group_needs_at_least_two_and_at_most_six() {
        let (_d, s) = store();
        let ids = three(&s);
        assert!(matches!(
            s.create_group("Solo", &ids[..1]),
            Err(BotError::BadGroupSize(1))
        ));

        let mut many = ids.clone();
        for i in 0..5 {
            many.push(s.create(&format!("Extra {i}"), "", "").unwrap().id);
        }
        assert!(matches!(
            s.create_group("Crowd", &many),
            Err(BotError::BadGroupSize(8))
        ));
        assert!(s.create_group("Launch", &ids).is_ok());
    }

    #[test]
    fn duplicate_members_collapse_rather_than_inflating_the_group() {
        let (_d, s) = store();
        let ids = three(&s);
        let dupes = vec![ids[0].clone(), ids[1].clone(), ids[0].clone()];
        let g = s.create_group("Launch", &dupes).unwrap();
        assert_eq!(g.members.len(), 2);
    }

    #[test]
    fn a_group_with_a_member_that_does_not_exist_is_refused() {
        let (_d, s) = store();
        let ids = three(&s);
        let mut bad = ids.clone();
        bad.push(BotId("ghost".into()));
        // A hole in the roster only shows up when somebody is mentioned, by
        // which point the work has already gone somewhere wrong.
        assert!(matches!(
            s.create_group("Launch", &bad),
            Err(BotError::NotFound(_))
        ));
    }

    #[test]
    fn the_transcript_is_shared_and_the_private_ones_are_not_touched() {
        let (_d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();

        s.append_group(&g.id, &[msg(Role::User, "kick off")])
            .unwrap();
        s.append_group(&g.id, &[msg(Role::Assistant, "on it")])
            .unwrap();

        assert_eq!(s.group_message_count(&g.id).unwrap(), 2);
        // A group turn must not leak into a Bot's own conversation, or its
        // private history fills with other people's threads.
        for id in &ids {
            assert_eq!(s.message_count(id).unwrap(), 0);
        }
    }

    #[test]
    fn a_group_survives_reopening_the_store() {
        let (d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();
        s.append_group(&g.id, &[msg(Role::User, "kick off")])
            .unwrap();
        drop(s);

        let again = BotStore::open(d.path()).unwrap();
        let back = again.resolve_group("Launch").unwrap();
        assert_eq!(back.members.len(), 3);
        assert_eq!(again.group_message_count(&back.id).unwrap(), 1);
    }

    #[test]
    fn groups_and_bots_share_one_roster_cap() {
        let (_d, s) = store();
        let ids = three(&s);
        // Three bots exist; fill the rest of the roster with groups.
        for i in 0..(MAX_BOTS - 3) {
            s.create_group(&format!("group {i}"), &ids).unwrap();
        }
        assert!(matches!(
            s.create_group("one too many", &ids),
            Err(BotError::TooMany)
        ));
    }

    #[test]
    fn deleting_a_group_leaves_its_members_alone() {
        let (_d, s) = store();
        let ids = three(&s);
        let g = s.create_group("Launch", &ids).unwrap();
        s.delete_group(&g.id).unwrap();

        assert!(matches!(s.get_group(&g.id), Err(BotError::NotFound(_))));
        for id in &ids {
            assert!(s.get(id).is_ok(), "deleting a group deleted a Bot");
        }
    }

    #[test]
    fn a_group_and_a_bot_can_share_a_name_without_colliding() {
        let (_d, s) = store();
        let ids = three(&s);
        s.create("Launch", "", "").unwrap();
        // Groups live in their own namespace; a name clash must not make one
        // resolve to the other.
        let g = s.create_group("Launch", &ids).unwrap();
        assert_eq!(s.resolve("Launch").unwrap().name, "Launch");
        assert_eq!(s.resolve_group("Launch").unwrap().id, g.id);
    }
}

// ── routines ──────────────────────────────────────────────────────────

/// Newlines in a chunk. Split out only so the byte literal is written once.
fn bytecount(chunk: &[u8]) -> usize {
    chunk.iter().filter(|b| **b == 10).count()
}

/// How much of the end of a log is read on the first attempt.
///
/// Named because `the_tail_never_returns_half_a_line` derives from it the one
/// `want` that puts the window's first line into the answer, the only case
/// where dropping that line is observable.
const TAIL_WINDOW: u64 = 64 * 1024;

/// Append whole lines to a file in a single write.
///
/// `writeln!(f, "{line}")` is two writes: `Write::write_fmt` hands the
/// formatter's pieces to `write_all` one at a time, so the text and its
/// newline reach the file separately. A second process appending between them
/// splices its message into the middle of the first, and both lines are lost,
/// because the reader skips what will not parse. Two writers appending
/// concurrently interleave partial lines under ordinary load; held by
/// `appends_from_two_writers_do_not_splice`.
///
/// Two writers is routine: a routine fires on a schedule while somebody is
/// talking to the same Bot in the window, and they are separate processes
/// appending to one `conversation.jsonl`. Nothing in this crate locks; a
/// single append-mode `write_all` is already atomic on the platforms this
/// ships to, and a cross-process lock would be a much larger promise.
fn append_lines(path: &Path, lines: impl IntoIterator<Item = String>) -> Result<()> {
    let mut buf = String::new();
    for line in lines {
        buf.push_str(&line);
        buf.push('\n');
    }
    if buf.is_empty() {
        return Ok(());
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(buf.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Read the last `want` lines of a file without parsing the rest.
///
/// Doubles a window backwards from the end until it holds enough complete
/// lines. Memory is proportional to what is returned, not to how long the file
/// has been growing; a conversation log is append-only and may span years.
///
/// The first line of a window is dropped unless the window reaches the start
/// of the file: it is almost certainly a fragment, and a fragment parses as a
/// skipped message rather than an error, which would silently lose a real one.
fn tail_lines(path: &Path, want: usize) -> Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(Vec::new());
    }

    // Enough for a handful of ordinary messages; doubles when it is not.
    let mut window: u64 = TAIL_WINDOW;
    loop {
        let from = len.saturating_sub(window);
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::with_capacity((len - from) as usize);
        Read::take(Read::by_ref(&mut f), len - from).read_to_end(&mut buf)?;

        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        if from > 0 && !lines.is_empty() {
            lines.remove(0);
        }

        if lines.len() >= want || from == 0 {
            let start = lines.len().saturating_sub(want);
            return Ok(lines[start..].iter().map(|l| (*l).to_owned()).collect());
        }
        window = window.saturating_mul(2);
    }
}

/// How many delivered event ids to remember for de-duplication.
///
/// Providers retry within minutes or hours; this is far past that, and bounds
/// what a webhook endpoint that has been running for years has to look at.
const EVENTS_REMEMBERED: usize = 10_000;

/// Matches the observed per-Bot limit.
pub const MAX_ROUTINES: usize = 50;
/// How many run records to keep. Enough to see a pattern, few enough that the
/// file stays readable.
pub const MAX_RUNS_KEPT: usize = 20;

/// How long to wait before retrying a routine that found the computer busy.
const RETRY_COOLOFF_MINUTES: i64 = 10;
/// How many times the wait between retries doubles: 10 min, 20, 40, up to
/// about 5 hours.
///
/// Capped so a routine that has been failing all week still tries a few times
/// a day: long enough to stop hammering, short enough that the morning after
/// an outage ends the digest is already there.
const RETRY_BACKOFF_STEPS: usize = 5;

/// One recorded run of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub at: chrono::DateTime<chrono::Utc>,
    pub ok: bool,
    /// What happened, in one line.
    pub summary: String,
    #[serde(default)]
    pub steps: u32,
    /// Tokens this run spent, in and out.
    ///
    /// Recorded per run because a routine can run unattended for months, and
    /// the first question when the bill arrives is which one has been doing
    /// the work.
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// The attempt did not get to do the work, and should be tried again.
    ///
    /// Set when a person had taken the computer. Without this the routine
    /// would be marked as having run and the firing would silently never
    /// happen.
    #[serde(default)]
    pub retryable: bool,
    /// Somebody ran this on purpose, rather than the schedule firing it.
    ///
    /// A rehearsal and a real firing leave the same trace otherwise, and the
    /// history is the only place anyone can check whether a routine has
    /// actually been running: three green rows that were all somebody pressing
    /// a button say the opposite of what they appear to say.
    ///
    /// `serde(default)` because every run recorded before this field existed
    /// was a scheduled one, which is exactly what `false` means.
    #[serde(default)]
    pub manual: bool,
}

/// Recurring work owned by one Bot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    pub bot: BotId,
    pub name: String,
    /// What the Bot is asked to do on every run. Written once and replayed
    /// forever, so it has to carry its own context.
    pub instructions: String,
    #[serde(flatten)]
    pub trigger: Trigger,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// When it last fired. `None` until the first run.
    #[serde(default)]
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub runs: Vec<Run>,
    /// A run is owed, and may be attempted again from this time.
    ///
    /// Set when an attempt could not proceed because a person had the
    /// computer. It carries two meanings: this routine still owes a run, and
    /// not before this time. Without the first, a deferred 9am firing would
    /// wait until tomorrow, because a routine that has never completed is only
    /// due within a minute of its firing time. Without the second, every tick
    /// would spend a model turn discovering the computer is still busy.
    #[serde(default)]
    pub retry_after: Option<chrono::DateTime<chrono::Utc>>,
}

fn yes() -> bool {
    true
}

impl Routine {
    /// The schedule, if this is a scheduled routine.
    pub fn cron(&self) -> Option<&schedule::Cron> {
        match &self.trigger {
            Trigger::Schedule { cron, .. } => Some(cron),
            Trigger::Event { .. } => None,
        }
    }

    pub fn tz(&self) -> Result<chrono_tz::Tz> {
        let name = match &self.trigger {
            Trigger::Schedule { timezone, .. } => timezone.as_str(),
            Trigger::Event { .. } => "",
        };
        schedule::timezone(name).map_err(|e| BotError::BadSchedule(e.to_string()))
    }

    /// When this routine next fires after `now`.
    ///
    /// `None` for an event trigger: it fires when something happens, and
    /// pretending to know when would be a lie in a status listing.
    pub fn next_after(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let Some(cron) = self.cron() else {
            return Ok(None);
        };
        cron.next_after(now, self.tz()?)
            .map_err(|e| BotError::BadSchedule(e.to_string()))
    }

    /// Whether this routine is owed a run at `now`.
    ///
    /// Computed from the last run rather than by matching the current minute.
    /// A scheduler that was asleep (laptop shut, process restarted) must still
    /// notice it missed a firing and run once. Matching the minute would
    /// silently skip every firing the process happened not to be awake for.
    pub fn is_due(&self, now: chrono::DateTime<chrono::Utc>) -> Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        // An event routine is never "due": it waits for something to happen.
        // Without this it would look permanently overdue and fire on every
        // tick, which is the runaway an event trigger is meant to avoid.
        if self.cron().is_none() {
            return Ok(false);
        }
        // A firing that could not be served is still owed, whatever the
        // schedule says next. Holding off alone would not be enough: a routine
        // that has never completed is due only within a minute of its firing
        // time, so a deferred 9am run would quietly wait until 9am tomorrow.
        if let Some(t) = self.retry_after {
            return Ok(now >= t);
        }
        match self.last_run {
            Some(t) => Ok(matches!(self.next_after(t)?, Some(next) if next <= now)),
            // Never run: due once its first firing has passed, not from the
            // beginning of time.
            None => Ok(matches!(
                self.next_after(now - chrono::Duration::minutes(1))?,
                Some(next) if next <= now
            )),
        }
    }

    /// How many firings were missed since the last run.
    ///
    /// Reported rather than replayed: running a daily digest eleven times
    /// because a laptop was closed for a fortnight is worse than running it
    /// once and saying so.
    pub fn missed(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        if self.cron().is_none() {
            return Ok(0);
        }
        let Some(mut t) = self.last_run else {
            return Ok(0);
        };
        let mut n = 0usize;
        while let Some(next) = self.next_after(t)? {
            if next > now || n > 1000 {
                break;
            }
            n += 1;
            t = next;
        }
        Ok(n.saturating_sub(1))
    }
}

impl BotStore {
    fn routine_dir(&self, bot: &BotId) -> PathBuf {
        self.dir(bot).join("routines")
    }
    fn routine_path(&self, bot: &BotId, id: &str) -> PathBuf {
        self.routine_dir(bot).join(format!("{id}.json"))
    }

    /// Create a scheduled routine.
    pub fn create_routine(
        &self,
        bot: &BotId,
        name: &str,
        instructions: &str,
        cron_expr: &str,
        timezone: &str,
    ) -> Result<Routine> {
        let cron =
            schedule::Cron::parse(cron_expr).map_err(|e| BotError::BadSchedule(e.to_string()))?;
        // Validate the zone now: a routine that cannot compute its own next
        // run is one that silently never fires.
        schedule::timezone(timezone).map_err(|e| BotError::BadSchedule(e.to_string()))?;
        self.create_triggered(
            bot,
            name,
            instructions,
            Trigger::Schedule {
                cron,
                timezone: timezone.to_owned(),
            },
        )
    }

    /// Create a routine with any trigger.
    pub fn create_triggered(
        &self,
        bot: &BotId,
        name: &str,
        instructions: &str,
        trigger: Trigger,
    ) -> Result<Routine> {
        if !self.profile_path(bot).exists() {
            return Err(BotError::NotFound(bot.0.clone()));
        }
        if let Trigger::Event { source, matches } = &trigger {
            if source.trim().is_empty() {
                return Err(BotError::BadTrigger(
                    "an event trigger needs a source".into(),
                ));
            }
            // A source with no conditions fires on every event from that
            // source; refused at creation rather than discovered in a bill.
            if matches.is_empty() {
                return Err(BotError::BadTrigger(format!(
                    "an event trigger needs at least one condition — `{source}` alone \
                     would fire on every event from that source"
                )));
            }
        }

        let id = Self::slug(name)?.0;
        if self.routine_path(bot, &id).exists() {
            return Err(BotError::Duplicate(name.to_owned()));
        }
        if self.routines(bot)?.len() >= MAX_ROUTINES {
            return Err(BotError::TooManyRoutines);
        }

        let r = Routine {
            id,
            bot: bot.clone(),
            name: name.to_owned(),
            instructions: instructions.to_owned(),
            trigger,
            enabled: true,
            last_run: None,
            runs: Vec::new(),
            retry_after: None,
        };
        fs::create_dir_all(self.routine_dir(bot))?;
        self.save_routine(&r)?;
        Ok(r)
    }

    pub fn save_routine(&self, r: &Routine) -> Result<()> {
        fs::create_dir_all(self.routine_dir(&r.bot))?;
        let path = self.routine_path(&r.bot, &r.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(r).expect("routine serialises"),
        )?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get_routine(&self, bot: &BotId, id: &str) -> Result<Routine> {
        let s = fs::read_to_string(self.routine_path(bot, id))
            .map_err(|_| BotError::NotFound(id.to_owned()))?;
        serde_json::from_str(&s).map_err(|e| BotError::Corrupt(e.to_string()))
    }

    pub fn routines(&self, bot: &BotId) -> Result<Vec<Routine>> {
        let dir = self.routine_dir(bot);
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if let Ok(r) = self.get_routine(bot, id) {
                out.push(r);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Every routine across every Bot.
    pub fn all_routines(&self) -> Result<Vec<Routine>> {
        let mut out = Vec::new();
        for b in self.list(true)? {
            out.extend(self.routines(&b.id)?);
        }
        Ok(out)
    }

    /// Record that a person looked at this account just now.
    ///
    /// Written only when a command runs at a terminal: cron has no terminal,
    /// so a machine ticking away on its own never counts as somebody watching.
    /// That distinction is the whole mechanism; see [`Self::idle_since`].
    ///
    /// Written through a temporary and a rename, like every other durable
    /// write in this file. A torn marker reads back as `None`, `None` means
    /// "nobody has ever looked", and that means do not pause; a crash during
    /// a plain write would switch off the protection in SPEC §8 and leave it
    /// off until somebody looks again.
    pub fn mark_seen(&self, at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let p = self.watched_path();
        let tmp = p.with_extension("writing");
        fs::write(&tmp, at.to_rfc3339())?;
        fs::rename(&tmp, &p)?;
        Ok(())
    }

    /// Not `seen_path`: that already means event de-duplication in this file.
    fn watched_path(&self) -> PathBuf {
        self.root.join("last-seen")
    }

    /// How long since a person last looked, if it is known.
    ///
    /// `None` on an account nobody has ever looked at from a terminal. That is
    /// not the same as a long absence and must not be treated as one: a fresh
    /// deployment driven entirely by cron would otherwise pause itself on its
    /// first tick.
    ///
    /// A marker that exists but cannot be read is also `None`, with a warning.
    /// Pausing every routine an account has on the strength of one
    /// unparseable line would be a large action on thin evidence. The write
    /// in [`Self::mark_seen`] is atomic, so reaching this needs a disk error
    /// or a hand-edited file rather than a crash.
    pub fn idle_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let raw = fs::read_to_string(self.watched_path()).ok()?;
        match chrono::DateTime::parse_from_rfc3339(raw.trim()) {
            Ok(t) => Some(t.with_timezone(&chrono::Utc)),
            Err(e) => {
                tracing::warn!(
                    path = %self.watched_path().display(),
                    error = %e,
                    "the last-seen marker is unreadable; routines will not pause for inactivity"
                );
                None
            }
        }
    }

    /// Routines to pause because nobody has looked in a long time.
    ///
    /// The failure this prevents is named in SPEC §8: an agent that keeps
    /// spending money while nobody is watching. There is no automatic resume;
    /// a person decides to start it again.
    pub fn idle_routines(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        after: chrono::Duration,
    ) -> Result<Vec<Routine>> {
        let Some(seen) = self.idle_since() else {
            return Ok(Vec::new());
        };
        if now - seen < after {
            return Ok(Vec::new());
        }
        Ok(self
            .all_routines()?
            .into_iter()
            .filter(|r| r.enabled)
            .collect())
    }

    pub fn due(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<Routine>> {
        let mut out = Vec::new();
        for r in self.all_routines()? {
            if r.is_due(now)? {
                out.push(r);
            }
        }
        Ok(out)
    }

    pub fn delete_routine(&self, bot: &BotId, id: &str) -> Result<()> {
        let p = self.routine_path(bot, id);
        if !p.exists() {
            return Err(BotError::NotFound(id.to_owned()));
        }
        fs::remove_file(p)?;
        Ok(())
    }

    /// Routines that should run because `event` arrived.
    ///
    /// Redelivery is rejected by event id: every webhook provider retries, and
    /// an agent doing consequential work twice is a real incident.
    pub fn triggered_by(&self, event: &Event) -> Result<Vec<Routine>> {
        if let Some(id) = &event.id {
            if self.event_seen(id)? {
                return Ok(Vec::new());
            }
        }
        let mut out = Vec::new();
        for r in self.all_routines()? {
            if r.enabled && r.trigger.fires_for(event) {
                out.push(r);
            }
        }
        Ok(out)
    }

    fn seen_path(&self) -> PathBuf {
        self.root.join("events-seen.jsonl")
    }

    /// Whether this event id has been delivered before.
    ///
    /// Looks only at the most recent [`EVENTS_REMEMBERED`] ids. Every webhook
    /// provider retries within minutes or hours, so a window of that size is
    /// far past any real retry, and scanning the whole file would make each
    /// delivery cost the entire history of deliveries.
    ///
    /// The trade-off: an id older than the window is treated as new. That
    /// would be a redelivery from tens of thousands of events ago, which no
    /// provider does.
    pub fn event_seen(&self, id: &str) -> Result<bool> {
        let p = self.seen_path();
        if !p.exists() {
            return Ok(false);
        }
        Ok(tail_lines(&p, EVENTS_REMEMBERED)?
            .iter()
            .any(|l| l.trim() == id))
    }

    /// Remember an event id so a retry is ignored.
    ///
    /// Trims the file when it grows well past the window rather than on every
    /// write: rewriting a file per event would cost more than the scan this
    /// replaced.
    pub fn remember_event(&self, id: &str) -> Result<()> {
        let p = self.seen_path();
        append_lines(&p, [id.to_owned()])?;

        // Cheap check: an id line is short, so file size is a good proxy for
        // count and costs one stat rather than a read.
        let big =
            fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > (EVENTS_REMEMBERED as u64) * 2 * 64;
        if big {
            let keep = tail_lines(&p, EVENTS_REMEMBERED)?;
            let tmp = p.with_extension("jsonl.tmp");
            fs::write(&tmp, keep.join("\n") + "\n")?;
            fs::rename(&tmp, &p)?;
        }
        Ok(())
    }

    /// Record a run, keeping only the most recent [`MAX_RUNS_KEPT`].
    ///
    /// A retryable attempt is recorded but does not advance the clock: the
    /// work has not happened, so the routine stays due and runs once the
    /// computer is free again, after a cool-off.
    pub fn record_run(&self, r: &mut Routine, run: Run) -> Result<()> {
        if run.retryable {
            // Back off as it keeps failing. The first retry is soon, because
            // most of these are a hub restarting and are over in seconds. A
            // real outage would otherwise be retried every ten minutes until
            // the idle policy noticed a fortnight later: two thousand
            // attempts, none more likely to work than the first.
            let consecutive = r
                .runs
                .iter()
                .rev()
                .take_while(|p| p.retryable)
                .count()
                .min(RETRY_BACKOFF_STEPS) as u32;
            let wait = RETRY_COOLOFF_MINUTES * 2i64.pow(consecutive);
            r.retry_after = Some(run.at + chrono::Duration::minutes(wait));
        } else {
            r.last_run = Some(run.at);
            r.retry_after = None;
        }
        r.runs.push(run);
        self.trim_runs(r);
        self.save_routine(r)
    }

    /// Record a run somebody asked for, without touching the schedule.
    ///
    /// This is the whole difference between a rehearsal and a firing, and it
    /// is not cosmetic. [`Routine::is_due`] is computed from `last_run` —
    /// deliberately, so a scheduler that was asleep still notices it missed a
    /// firing — so recording a test run the ordinary way would set `last_run`
    /// to now and the next real firing would be computed from the rehearsal.
    /// Pressing "test run" at 08:55 would silently cancel the 09:00 run, and
    /// the routine would look like it had run, because in a sense it had.
    ///
    /// `retry_after` is left alone for the same reason. A rehearsal that hits
    /// a held computer must not push out a real firing that was already owed,
    /// and must not clear a backoff that a string of real failures earned.
    ///
    /// The history is still written, because the point of a rehearsal is
    /// seeing what it did, and it is marked [`Run::manual`] so the record does
    /// not read as evidence the schedule is working.
    pub fn record_manual_run(&self, r: &mut Routine, run: Run) -> Result<()> {
        r.runs.push(Run {
            manual: true,
            ..run
        });
        self.trim_runs(r);
        self.save_routine(r)
    }

    /// Keep the history bounded, oldest first out.
    fn trim_runs(&self, r: &mut Routine) {
        if r.runs.len() > MAX_RUNS_KEPT {
            let drop = r.runs.len() - MAX_RUNS_KEPT;
            r.runs.drain(..drop);
        }
    }
}

#[cfg(test)]
mod routine_tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    /// A failure the runner marked as retryable.
    fn held(at: DateTime<Utc>) -> Run {
        Run {
            at,
            ok: false,
            summary: "connect: connection refused".into(),
            steps: 0,
            tokens_in: 0,
            tokens_out: 0,
            retryable: true,
            manual: false,
        }
    }

    /// A rehearsal must not cancel the firing it was rehearsing.
    ///
    /// `is_due` is computed from `last_run`, deliberately, so a scheduler that
    /// was asleep still notices it missed a firing. Recording a test run the
    /// ordinary way therefore sets `last_run` to now and the next real firing
    /// is computed from the rehearsal: pressing "test run" at 08:55 silently
    /// cancels the 09:00 run, and the routine looks like it ran, because in a
    /// sense it did.
    ///
    /// This is the assertion that keeps the two recorders apart, and it is why
    /// there are two.
    #[test]
    fn a_test_run_does_not_cancel_the_next_real_one() {
        let (_d, s, bot) = store();
        let mut r = s
            .create_routine(&bot, "nightly", "digest", "0 9 * * *", "UTC")
            .unwrap();

        // Half a minute past nine, with this morning's firing owed and not yet
        // served. Not five past: a routine that has never run is due only
        // within a minute of its firing time, which `is_due` documents and the
        // first draft of this test did not read.
        let nine_oh_five: DateTime<Utc> = "2026-08-25T09:00:30Z".parse().unwrap();
        assert!(
            r.is_due(nine_oh_five).unwrap(),
            "the routine was not owed a run, so nothing below could cancel one"
        );

        s.record_manual_run(
            &mut r,
            Run {
                at: nine_oh_five,
                ok: true,
                summary: "rehearsed".into(),
                steps: 1,
                tokens_in: 1,
                tokens_out: 1,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();

        assert!(
            r.is_due(nine_oh_five).unwrap(),
            "a rehearsal marked the routine as having run; the nine o'clock firing will never happen and the history will say it did"
        );
        assert_eq!(
            r.last_run, None,
            "the rehearsal moved the schedule, which is the one thing it must not touch"
        );

        // And the ordinary recorder still does what it is for, or this test is
        // asserting that nothing works.
        s.record_run(
            &mut r,
            Run {
                at: nine_oh_five,
                ok: true,
                summary: "fired".into(),
                steps: 1,
                tokens_in: 1,
                tokens_out: 1,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();
        assert!(
            !r.is_due(nine_oh_five).unwrap(),
            "a real firing did not settle the schedule either, so nothing here works"
        );
    }

    /// A rehearsal is recorded, and says it was one.
    ///
    /// The point of a test run is seeing what it did, so it goes in the
    /// history. But three green rows that were all somebody pressing a button
    /// say the opposite of what they appear to say, and the history is the
    /// only place anyone can check whether a routine is actually running.
    #[test]
    fn a_test_run_is_kept_and_says_it_was_a_test() {
        let (_d, s, bot) = store();
        let mut r = s
            .create_routine(&bot, "nightly", "digest", "0 9 * * *", "UTC")
            .unwrap();
        let at = Utc::now();
        let run = |summary: &str| Run {
            at,
            ok: true,
            summary: summary.into(),
            steps: 1,
            tokens_in: 1,
            tokens_out: 1,
            retryable: false,
            manual: false,
        };

        s.record_manual_run(&mut r, run("rehearsed")).unwrap();
        s.record_run(&mut r, run("fired")).unwrap();

        let marks: Vec<bool> = r.runs.iter().map(|p| p.manual).collect();
        assert_eq!(
            marks,
            vec![true, false],
            "the history cannot tell a rehearsal from a firing"
        );

        // Round-trips, or the mark is only true until the next launch.
        let back = s.get_routine(&bot, &r.id).unwrap();
        assert_eq!(
            back.runs.iter().map(|p| p.manual).collect::<Vec<_>>(),
            vec![true, false],
            "the mark did not survive being written to disk and read back"
        );
    }

    #[test]
    fn retries_back_off_instead_of_hammering_a_dead_hub() {
        // The first retry is soon, because most of these are a hub restarting
        // and are over in seconds. Without a backoff a genuine outage is
        // retried every ten minutes until the idle policy notices a fortnight
        // later: two thousand attempts, none more likely to work than the
        // first.
        let (_d, s, bot) = store();
        let mut r = s
            .create_routine(&bot, "nightly", "digest", "0 9 * * *", "UTC")
            .unwrap();

        let start = Utc::now();
        let mut waits = Vec::new();
        for i in 0..6 {
            let at = start + Duration::hours(i);
            s.record_run(&mut r, held(at)).unwrap();
            waits.push((r.retry_after.unwrap() - at).num_minutes());
        }

        assert_eq!(waits[0], 10, "the first retry should be soon");
        assert!(
            waits.windows(2).all(|w| w[1] >= w[0]),
            "the wait shrank as it kept failing: {waits:?}"
        );
        assert!(
            *waits.last().unwrap() <= 60 * 6,
            "backed off past a working day: {waits:?}"
        );
        // Still trying a few times a day, so the morning after an outage ends
        // the digest is already there.
        assert!(
            *waits.last().unwrap() >= 60,
            "gave up too readily: {waits:?}"
        );
    }

    #[test]
    fn one_success_clears_the_backoff() {
        let (_d, s, bot) = store();
        let mut r = s
            .create_routine(&bot, "nightly", "digest", "0 9 * * *", "UTC")
            .unwrap();
        let start = Utc::now();
        for i in 0..4 {
            s.record_run(&mut r, held(start + Duration::hours(i)))
                .unwrap();
        }
        s.record_run(
            &mut r,
            Run {
                at: start + Duration::hours(5),
                ok: true,
                summary: "done".into(),
                steps: 2,
                tokens_in: 1,
                tokens_out: 1,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();
        assert!(r.retry_after.is_none(), "a success left a retry pending");

        // And the next failure starts from the short wait again, rather than
        // resuming a backoff earned days ago.
        let at = start + Duration::hours(6);
        s.record_run(&mut r, held(at)).unwrap();
        assert_eq!((r.retry_after.unwrap() - at).num_minutes(), 10);
    }

    fn store() -> (tempfile::TempDir, BotStore, BotId) {
        let d = tempfile::tempdir().unwrap();
        let s = BotStore::open(d.path()).unwrap();
        let b = s.create("Piper", "", "").unwrap().id;
        (d, s, b)
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_routine_round_trips_with_its_schedule() {
        let (d, s, b) = store();
        s.create_routine(
            &b,
            "Morning digest",
            "summarise the inbox",
            "0 9 * * *",
            "UTC",
        )
        .unwrap();
        drop(s);

        let again = BotStore::open(d.path()).unwrap();
        let r = again.get_routine(&b, "morning-digest").unwrap();
        assert_eq!(r.instructions, "summarise the inbox");
        assert!(r.enabled);
        // The schedule reparses from its stored expression.
        assert_eq!(
            r.next_after(at("2026-08-12T08:00:00Z")).unwrap(),
            Some(at("2026-08-12T09:00:00Z"))
        );
    }

    #[test]
    fn a_bad_schedule_is_refused_at_creation_not_at_the_first_run() {
        let (_d, s, b) = store();
        // Discovering this when the routine silently never fires is far worse.
        assert!(matches!(
            s.create_routine(&b, "Bad", "x", "not a cron", "UTC"),
            Err(BotError::BadSchedule(_))
        ));
        assert!(matches!(
            s.create_routine(&b, "Bad tz", "x", "0 9 * * *", "Mars/Olympus"),
            Err(BotError::BadSchedule(_))
        ));
        assert!(s.routines(&b).unwrap().is_empty());
    }

    #[test]
    fn a_routine_for_a_bot_that_does_not_exist_is_refused() {
        let (_d, s, _b) = store();
        assert!(matches!(
            s.create_routine(&BotId("ghost".into()), "R", "x", "0 9 * * *", "UTC"),
            Err(BotError::NotFound(_))
        ));
    }

    #[test]
    fn a_never_run_routine_becomes_due_only_once_its_time_has_passed() {
        let (_d, s, b) = store();
        let r = s
            .create_routine(&b, "Digest", "x", "0 9 * * *", "UTC")
            .unwrap();
        assert!(!r.is_due(at("2026-08-12T08:59:00Z")).unwrap());
        assert!(r.is_due(at("2026-08-12T09:00:00Z")).unwrap());
    }

    #[test]
    fn a_routine_missed_while_the_process_slept_still_runs_once() {
        let (_d, s, b) = store();
        let mut r = s
            .create_routine(&b, "Digest", "x", "0 9 * * *", "UTC")
            .unwrap();
        s.record_run(
            &mut r,
            Run {
                at: at("2026-08-01T09:00:00Z"),
                ok: true,
                summary: "ok".into(),
                steps: 1,
                tokens_in: 0,
                tokens_out: 0,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();

        // Eleven days later the laptop opens. Matching the current minute
        // would find nothing and silently skip every missed morning.
        let now = at("2026-08-12T14:23:00Z");
        assert!(r.is_due(now).unwrap(), "a missed routine was skipped");
        // ...but it runs once, and says how many it missed rather than
        // replaying eleven digests.
        assert_eq!(r.missed(now).unwrap(), 10);
    }

    #[test]
    fn a_paused_routine_is_never_due() {
        let (_d, s, b) = store();
        let mut r = s
            .create_routine(&b, "Digest", "x", "0 9 * * *", "UTC")
            .unwrap();
        r.enabled = false;
        assert!(!r.is_due(at("2026-08-12T09:00:00Z")).unwrap());
    }

    #[test]
    fn run_history_is_capped_at_the_documented_limit() {
        let (_d, s, b) = store();
        let mut r = s
            .create_routine(&b, "Digest", "x", "0 9 * * *", "UTC")
            .unwrap();
        for i in 0..(MAX_RUNS_KEPT + 5) {
            s.record_run(
                &mut r,
                Run {
                    at: at("2026-08-12T09:00:00Z") + Duration::days(i as i64),
                    ok: true,
                    summary: format!("run {i}"),
                    steps: 1,
                    tokens_in: 0,
                    tokens_out: 0,
                    retryable: false,
                    manual: false,
                },
            )
            .unwrap();
        }
        assert_eq!(r.runs.len(), MAX_RUNS_KEPT);
        // The oldest go, not the newest.
        assert_eq!(r.runs[0].summary, "run 5");
        assert_eq!(
            r.runs[MAX_RUNS_KEPT - 1].summary,
            format!("run {}", MAX_RUNS_KEPT + 4)
        );
    }

    #[test]
    fn routines_pause_when_nobody_has_looked_for_a_long_time() {
        // SPEC §8: an agent that keeps working, and spending, while nobody is
        // watching is a bug.
        let (_d, s, b) = store();
        let r = s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        let now = at("2026-08-12T09:00:00Z");

        // Nobody has ever looked. That is *not* a long absence: a fresh
        // deployment driven entirely by cron must not pause itself on its
        // first tick.
        assert!(s.idle_since().is_none());
        assert!(s.idle_routines(now, Duration::days(14)).unwrap().is_empty());

        // Someone looked a fortnight ago.
        s.mark_seen(now - Duration::days(15)).unwrap();
        let idle = s.idle_routines(now, Duration::days(14)).unwrap();
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].id, r.id);

        // Someone looked this morning.
        s.mark_seen(now - Duration::hours(2)).unwrap();
        assert!(s.idle_routines(now, Duration::days(14)).unwrap().is_empty());
    }

    /// An unreadable marker does not pause anything. This is a design
    /// decision.
    ///
    /// `idle_since` answers `None` for two different facts: nobody has ever
    /// looked, and the marker cannot be read. Both mean "do not pause", so an
    /// unreadable marker switches off the protection SPEC §8 exists for.
    ///
    /// The write is atomic, so a crash cannot produce this; what is left is a
    /// disk error or a hand-edited file, and the answer to those is still
    /// "keep running": pausing every routine an account has on the strength
    /// of one unparseable line is a large action on thin evidence.
    /// `idle_since` warns rather than letting it pass for a fresh deployment.
    ///
    /// Held here because it is a decision about spending money, and one that
    /// is easy to flip while tidying an `Option`.
    #[test]
    fn an_unreadable_marker_keeps_routines_running_and_says_so() {
        let (_d, s, b) = store();
        s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        let now = at("2026-08-12T09:00:00Z");

        // Long past the threshold, with a marker that reads.
        s.mark_seen(now - Duration::days(400)).unwrap();
        assert_eq!(
            s.idle_routines(now, Duration::days(14)).unwrap().len(),
            1,
            "the routine was not paused even with a readable marker, so the rest of \
             this test would prove nothing"
        );

        let p = s.root.join("last-seen");
        let full = fs::read_to_string(&p).unwrap();
        fs::write(&p, &full[..full.len() / 2]).unwrap();

        assert!(s.idle_since().is_none());
        assert!(
            s.idle_routines(now, Duration::days(14)).unwrap().is_empty(),
            "an unreadable marker now pauses routines, which changes what this \
             account spends and must be an explicit decision"
        );
    }

    /// A successful `mark_seen` leaves the marker readable and no temporary
    /// behind.
    ///
    /// This does not test the atomicity: nothing in this suite stops a
    /// process mid-write, so a plain `fs::write` would leave every test green.
    #[test]
    fn marking_seen_leaves_no_half_written_marker_behind() {
        let (_d, s, _b) = store();
        let now = at("2026-08-12T09:00:00Z");
        s.mark_seen(now).unwrap();
        assert_eq!(s.idle_since(), Some(now));
        assert!(
            !s.root.join("last-seen.writing").exists(),
            "the temporary outlived the write that made it"
        );
    }

    #[test]
    fn an_already_paused_routine_is_not_paused_again() {
        // Otherwise every tick would re-announce the same thing forever.
        let (_d, s, b) = store();
        let mut r = s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        let now = at("2026-08-12T09:00:00Z");
        s.mark_seen(now - Duration::days(30)).unwrap();

        r.enabled = false;
        s.save_routine(&r).unwrap();
        assert!(s.idle_routines(now, Duration::days(14)).unwrap().is_empty());
    }

    #[test]
    fn the_watched_marker_survives_a_reload() {
        // A tick is a fresh process; an in-memory marker would be useless.
        let (_d, s, _b) = store();
        let when = at("2026-08-01T10:00:00Z");
        s.mark_seen(when).unwrap();
        assert_eq!(s.idle_since(), Some(when));
    }

    #[test]
    fn a_run_blocked_by_a_person_stays_due_instead_of_being_eaten() {
        // If a person is driving the computer at firing time, the run must
        // not be silently consumed. A blocked attempt is recorded, but the
        // work has not been done, so the routine is still owed.
        let (_d, s, b) = store();
        let mut r = s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        let when = at("2026-08-12T09:00:00Z");

        s.record_run(
            &mut r,
            Run {
                at: when,
                ok: false,
                summary: "a person has taken over this computer".into(),
                steps: 1,
                tokens_in: 0,
                tokens_out: 0,
                retryable: true,
                manual: false,
            },
        )
        .unwrap();

        assert!(r.last_run.is_none(), "a blocked attempt consumed the run");
        assert_eq!(r.runs.len(), 1, "the attempt was not recorded at all");

        // Not immediately, though: each retry costs a model turn to discover
        // the computer is still busy.
        assert!(!r.is_due(when + Duration::minutes(1)).unwrap());
        assert!(r.is_due(when + Duration::minutes(11)).unwrap());

        // Once it runs, the clock advances and the hold clears.
        s.record_run(
            &mut r,
            Run {
                at: when + Duration::minutes(11),
                ok: true,
                summary: "done".into(),
                steps: 4,
                tokens_in: 0,
                tokens_out: 0,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();
        assert!(r.last_run.is_some());
        assert!(r.retry_after.is_none());
        assert!(!r.is_due(when + Duration::minutes(12)).unwrap());
    }

    #[test]
    fn a_deferred_routine_survives_a_reload() {
        let (_d, s, b) = store();
        let mut r = s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        let when = at("2026-08-12T09:00:00Z");
        s.record_run(
            &mut r,
            Run {
                at: when,
                ok: false,
                summary: "busy".into(),
                steps: 1,
                tokens_in: 0,
                tokens_out: 0,
                retryable: true,
                manual: false,
            },
        )
        .unwrap();

        // The hold has to be on disk: a tick is usually a fresh process.
        let reloaded = s
            .routines(&b)
            .unwrap()
            .into_iter()
            .find(|x| x.id == r.id)
            .expect("the routine vanished");
        assert_eq!(reloaded.retry_after, r.retry_after);
        assert!(reloaded.last_run.is_none());
        assert!(!reloaded.is_due(when + Duration::minutes(1)).unwrap());
    }

    #[test]
    fn recording_a_run_persists_and_advances_the_clock() {
        let (_d, s, b) = store();
        let mut r = s.create_routine(&b, "D", "x", "0 9 * * *", "UTC").unwrap();
        s.record_run(
            &mut r,
            Run {
                at: at("2026-08-12T09:00:00Z"),
                ok: false,
                summary: "the source was unreachable".into(),
                steps: 3,
                tokens_in: 0,
                tokens_out: 0,
                retryable: false,
                manual: false,
            },
        )
        .unwrap();

        let back = s.get_routine(&b, &r.id).unwrap();
        assert_eq!(back.last_run, Some(at("2026-08-12T09:00:00Z")));
        assert_eq!(back.runs.len(), 1);
        assert!(!back.runs[0].ok, "a failed run was recorded as a success");
        // And it is not immediately due again.
        assert!(!back.is_due(at("2026-08-12T09:01:00Z")).unwrap());
    }

    #[test]
    fn due_finds_routines_across_every_bot() {
        let (_d, s, b) = store();
        let other = s.create("Scout", "", "").unwrap().id;
        s.create_routine(&b, "Morning", "x", "0 9 * * *", "UTC")
            .unwrap();
        s.create_routine(&other, "Evening", "x", "0 18 * * *", "UTC")
            .unwrap();

        let due = s.due(at("2026-08-12T09:00:00Z")).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Morning");

        let due = s.due(at("2026-08-12T18:00:00Z")).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Evening");
    }

    #[test]
    fn the_per_bot_routine_cap_is_enforced() {
        let (_d, s, b) = store();
        for i in 0..MAX_ROUTINES {
            s.create_routine(&b, &format!("r {i}"), "x", "0 9 * * *", "UTC")
                .unwrap();
        }
        assert!(matches!(
            s.create_routine(&b, "one more", "x", "0 9 * * *", "UTC"),
            Err(BotError::TooManyRoutines)
        ));
    }

    #[test]
    fn deleting_a_routine_leaves_the_bot_and_its_siblings() {
        let (_d, s, b) = store();
        s.create_routine(&b, "A", "x", "0 9 * * *", "UTC").unwrap();
        s.create_routine(&b, "B", "x", "0 9 * * *", "UTC").unwrap();
        s.delete_routine(&b, "a").unwrap();

        assert_eq!(s.routines(&b).unwrap().len(), 1);
        assert!(s.get(&b).is_ok());
        assert!(matches!(
            s.delete_routine(&b, "a"),
            Err(BotError::NotFound(_))
        ));
    }

    #[test]
    fn a_timezone_is_respected_when_deciding_what_is_due() {
        let (_d, s, b) = store();
        let r = s
            .create_routine(&b, "IST digest", "x", "0 9 * * *", "Asia/Kolkata")
            .unwrap();
        // 09:00 IST is 03:30 UTC, not 09:00 UTC.
        assert!(!r.is_due(at("2026-08-12T09:00:00Z")).unwrap() || r.last_run.is_some());
        assert!(r.is_due(at("2026-08-12T03:30:00Z")).unwrap());
    }
}

// ── event triggers ────────────────────────────────────────────────────

/// Something that happened somewhere else: a Slack message, a GitHub
/// notification, a webhook from anything at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Where it came from, e.g. `github`, `slack`.
    pub source: String,
    /// Sender-supplied identity, used to reject a redelivery.
    ///
    /// Every webhook provider retries. Without this, a provider that does not
    /// get its 200 fast enough runs the routine twice, and an agent doing
    /// consequential work twice is a real incident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub payload: serde_json::Value,
}

/// One condition an event must satisfy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    /// Dotted path into the payload, e.g. `issue.title` or `channel`.
    pub path: String,
    #[serde(flatten)]
    pub test: Test,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Test {
    Equals(String),
    Contains(String),
}

impl Match {
    /// Whether `payload` satisfies this condition.
    ///
    /// A path that is not present never matches. Treating a missing field as a
    /// match would turn `issue.title contains "repro"` into "every event from
    /// this source", which is exactly the broad listener that produces noise.
    pub fn test(&self, payload: &serde_json::Value) -> bool {
        let Some(v) = dig(payload, &self.path) else {
            return false;
        };
        let s = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match &self.test {
            Test::Equals(want) => s.eq_ignore_ascii_case(want),
            Test::Contains(want) => s.to_lowercase().contains(&want.to_lowercase()),
        }
    }
}

/// Follow a dotted path into a JSON value.
fn dig<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// What makes a routine run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "trigger", rename_all = "snake_case")]
pub enum Trigger {
    Schedule {
        cron: schedule::Cron,
        /// IANA name. Empty means UTC.
        #[serde(default)]
        timezone: String,
    },
    /// Fires when a matching event arrives.
    ///
    /// At least one condition is required. A source alone is an "every new
    /// message" listener: it burns usage, floods the thread, and hands the
    /// agent input nobody meant it to act on. Refusing it at creation is
    /// cheaper than discovering it in a bill.
    Event { source: String, matches: Vec<Match> },
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Trigger::Schedule { cron, timezone } => {
                let base = schedule::describe(&cron.expr);
                if timezone.is_empty() {
                    base
                } else {
                    format!("{base} ({timezone})")
                }
            }
            Trigger::Event { source, matches } => {
                let conds: Vec<String> = matches
                    .iter()
                    .map(|m| match &m.test {
                        Test::Equals(v) => format!("{} is {v:?}", m.path),
                        Test::Contains(v) => format!("{} contains {v:?}", m.path),
                    })
                    .collect();
                format!("on a {source} event where {}", conds.join(" and "))
            }
        }
    }

    /// Whether this trigger fires for `event`. Schedules never do.
    pub fn fires_for(&self, event: &Event) -> bool {
        match self {
            Trigger::Schedule { .. } => false,
            Trigger::Event { source, matches } => {
                if !source.eq_ignore_ascii_case(&event.source) {
                    return false;
                }
                // Every condition must hold. `matches` is never empty; that
                // is enforced at creation.
                !matches.is_empty() && matches.iter().all(|m| m.test(&event.payload))
            }
        }
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, BotStore, BotId) {
        let d = tempfile::tempdir().unwrap();
        let s = BotStore::open(d.path()).unwrap();
        let b = s.create("Repro", "", "").unwrap().id;
        (d, s, b)
    }

    fn trig(source: &str, path: &str, contains: &str) -> Trigger {
        Trigger::Event {
            source: source.into(),
            matches: vec![Match {
                path: path.into(),
                test: Test::Contains(contains.into()),
            }],
        }
    }

    fn ev(source: &str, payload: serde_json::Value) -> Event {
        Event {
            source: source.into(),
            id: None,
            payload,
        }
    }

    #[test]
    fn a_source_alone_is_refused_because_it_fires_on_everything() {
        let (_d, s, b) = store();
        // A source alone fires on every event. Refusing it at creation is
        // cheaper than discovering it in a bill.
        let e = s.create_triggered(
            &b,
            "Too broad",
            "x",
            Trigger::Event {
                source: "slack".into(),
                matches: vec![],
            },
        );
        assert!(matches!(e, Err(BotError::BadTrigger(_))));
        assert!(s.routines(&b).unwrap().is_empty());
    }

    #[test]
    fn a_matching_event_fires_the_routine() {
        let (_d, s, b) = store();
        s.create_triggered(
            &b,
            "Repro",
            "reproduce it",
            trig("slack", "text", "needs repro"),
        )
        .unwrap();
        let hit = s
            .triggered_by(&ev(
                "slack",
                json!({ "text": "ticket 42 needs repro please" }),
            ))
            .unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "Repro");
    }

    #[test]
    fn a_non_matching_event_fires_nothing() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "Repro", "x", trig("slack", "text", "needs repro"))
            .unwrap();
        // Right source, wrong content.
        assert!(s
            .triggered_by(&ev("slack", json!({ "text": "lunch plans" })))
            .unwrap()
            .is_empty());
        // Right content, wrong source.
        assert!(s
            .triggered_by(&ev("github", json!({ "text": "needs repro" })))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_missing_field_never_matches() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "Repro", "x", trig("slack", "text", "repro"))
            .unwrap();
        // Treating an absent field as a match would silently widen the rule
        // into every event from this source.
        assert!(s
            .triggered_by(&ev("slack", json!({ "other": "repro" })))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn every_condition_must_hold() {
        let (_d, s, b) = store();
        s.create_triggered(
            &b,
            "Narrow",
            "x",
            Trigger::Event {
                source: "slack".into(),
                matches: vec![
                    Match {
                        path: "channel".into(),
                        test: Test::Equals("escalations".into()),
                    },
                    Match {
                        path: "text".into(),
                        test: Test::Contains("needs repro".into()),
                    },
                ],
            },
        )
        .unwrap();

        let both = ev(
            "slack",
            json!({"channel": "escalations", "text": "this needs repro"}),
        );
        assert_eq!(s.triggered_by(&both).unwrap().len(), 1);

        let one = ev(
            "slack",
            json!({"channel": "random", "text": "this needs repro"}),
        );
        assert!(
            s.triggered_by(&one).unwrap().is_empty(),
            "an AND rule matched on only one condition"
        );
    }

    #[test]
    fn nested_paths_are_followed() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "R", "x", trig("github", "issue.title", "crash"))
            .unwrap();
        let hit = s
            .triggered_by(&ev(
                "github",
                json!({"issue": {"title": "crash on startup"}}),
            ))
            .unwrap();
        assert_eq!(hit.len(), 1);
    }

    #[test]
    fn a_redelivered_event_does_not_run_twice() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "R", "x", trig("github", "action", "opened"))
            .unwrap();
        let e = Event {
            source: "github".into(),
            id: Some("delivery-abc123".into()),
            payload: json!({ "action": "opened" }),
        };

        assert_eq!(s.triggered_by(&e).unwrap().len(), 1);
        s.remember_event(e.id.as_ref().unwrap()).unwrap();
        // Every webhook provider retries. Doing consequential work twice is a
        // real incident, not a nuisance.
        assert!(
            s.triggered_by(&e).unwrap().is_empty(),
            "a retried webhook ran the routine again"
        );
    }

    #[test]
    fn an_event_without_an_id_is_always_delivered() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "R", "x", trig("cli", "kind", "manual"))
            .unwrap();
        let e = ev("cli", json!({ "kind": "manual" }));
        assert_eq!(s.triggered_by(&e).unwrap().len(), 1);
        assert_eq!(s.triggered_by(&e).unwrap().len(), 1);
    }

    #[test]
    fn a_paused_event_routine_does_not_fire() {
        let (_d, s, b) = store();
        let mut r = s
            .create_triggered(&b, "R", "x", trig("slack", "text", "repro"))
            .unwrap();
        r.enabled = false;
        s.save_routine(&r).unwrap();
        assert!(s
            .triggered_by(&ev("slack", json!({"text": "needs repro"})))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_event_routine_is_never_due_on_a_clock_tick() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "R", "x", trig("slack", "text", "repro"))
            .unwrap();
        // Otherwise it looks permanently overdue and fires on every tick,
        // which is the runaway an event trigger exists to avoid.
        let now = chrono::Utc::now();
        assert!(s.due(now).unwrap().is_empty());
        let r = s.get_routine(&b, "r").unwrap();
        assert_eq!(r.next_after(now).unwrap(), None);
        assert_eq!(r.missed(now).unwrap(), 0);
    }

    #[test]
    fn a_schedule_never_fires_for_an_event() {
        let (_d, s, b) = store();
        s.create_routine(&b, "Daily", "x", "0 9 * * *", "UTC")
            .unwrap();
        assert!(s
            .triggered_by(&ev("slack", json!({"text": "anything"})))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_trigger_survives_a_round_trip_and_reads_back_in_words() {
        let (d, s, b) = store();
        s.create_triggered(
            &b,
            "Repro",
            "x",
            Trigger::Event {
                source: "slack".into(),
                matches: vec![Match {
                    path: "channel".into(),
                    test: Test::Equals("escalations".into()),
                }],
            },
        )
        .unwrap();
        drop(s);

        let again = BotStore::open(d.path()).unwrap();
        let r = again.get_routine(&b, "repro").unwrap();
        assert!(r
            .trigger
            .describe()
            .contains("on a slack event where channel is"));
        assert!(r
            .trigger
            .fires_for(&ev("slack", json!({"channel": "escalations"}))));
    }

    #[test]
    fn matching_is_case_insensitive_because_humans_are() {
        let (_d, s, b) = store();
        s.create_triggered(&b, "R", "x", trig("Slack", "text", "Needs Repro"))
            .unwrap();
        assert_eq!(
            s.triggered_by(&ev("slack", json!({"text": "this NEEDS REPRO now"})))
                .unwrap()
                .len(),
            1
        );
    }

    /// The cap counts Bots and groups together, from both sides.
    ///
    /// The rule is "an account can have up to 50 Bots and group chats
    /// combined". Both `create` and `create_group` must enforce it, and the
    /// error must name both things it counts, or a user with groups is
    /// misdirected about what to delete.
    #[test]
    fn the_roster_cap_names_both_things_it_counts() {
        let (_d, s, first) = store();
        // A group needs two members, so make the second explicitly and fill
        // the rest to one short of the cap.
        let second = s.create("Second", "", "").unwrap().id;
        for i in 0..(MAX_BOTS - 3) {
            s.create(&format!("Bot {i}"), "", "").unwrap();
        }
        // One slot left, and a group takes it.
        s.create_group("Team", &[first.clone(), second.clone()])
            .unwrap();

        // Now both are refused, with a message that describes the real rule.
        let said = s.create("One More", "", "").unwrap_err().to_string();
        assert!(said.contains("Bots and group chats"), "{said}");
        let said = s
            .create_group("Another", &[first, second])
            .unwrap_err()
            .to_string();
        assert!(
            said.contains("Bots and group chats"),
            "a group refused for a full roster blamed bots alone: {said}"
        );
    }

    /// A rename cannot reach a state a creation forbids.
    ///
    /// `create` refuses a name already taken; `rename` must too. Otherwise
    /// two Bots could both be called `Ledger`, `resolve` would refuse both by
    /// name, and `@Ledger` and `bot.send "Ledger"` would reach neither.
    #[test]
    fn a_rename_cannot_take_a_name_that_is_already_someone_elses() {
        let (_d, s, _first) = store();
        s.create("Ledger", "", "").unwrap();
        let accounts = s.create("Accounts", "", "").unwrap();

        let err = s.rename("Accounts", "Ledger").unwrap_err().to_string();
        assert!(err.contains("Ledger"), "{err}");

        // Untouched, and still reachable by its own name.
        assert_eq!(s.resolve("Accounts").unwrap().id, accounts.id);
        // And the name still means exactly one Bot.
        assert_eq!(s.resolve("Ledger").unwrap().name, "Ledger");
    }

    /// Renaming a Bot to what it is already called is a no-op, not a clash.
    ///
    /// A settings form sends every field, so saving an unedited name must not
    /// fail. The check compares slugs, so a difference in case or spacing is
    /// still the same name.
    #[test]
    fn a_bot_may_keep_its_own_name() {
        let (_d, s, _first) = store();
        let b = s.create("Ledger", "", "").unwrap();
        assert_eq!(s.rename("Ledger", "Ledger").unwrap().id, b.id);
        // Recasing is the ordinary reason to rename to "the same" name.
        assert_eq!(s.rename("Ledger", "LEDGER").unwrap().name, "LEDGER");
    }
}
