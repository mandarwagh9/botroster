//! Terminal rendering of an agent session.
//!
//! A session is a stream of events, and the job here is to make that stream
//! legible while it is still moving. Three rules:
//!
//! 1. The shape carries the meaning. A tool call is a row with a verb, a
//!    target, a duration and a mark; the left gutter should show what happened
//!    without reading a word.
//! 2. Width never decides correctness. Long paths and long output are the
//!    normal case; every field is bounded so a wide value cannot smear the
//!    layout.
//! 3. Degrade cleanly. Without a TTY there is no colour and no redraw: the
//!    same transcript, plain, so piping to a file or a CI log still reads.

use std::io::{IsTerminal, Write};

/// Fixed columns. A gutter is only scannable if the marks land in the same
/// place on every row.
const TOOL_COL: usize = 12;
const TARGET_COL: usize = 38;
const TIME_COL: usize = 7;

use botroster_agent::agent::{AgentEvent, FinishReason};
use botroster_agent::model::{Content, Message, Role};
use botroster_proto::ToolCallId;
use serde_json::Value;

/// The end of a tool-call row: how long it took, whether the call worked, and
/// the exit code if a command failed.
///
/// A separate function so the rendered line itself can be asserted on; the
/// renderer otherwise prints straight to stdout with no seam.
fn result_tail(s: Style, elapsed_ms: u64, ok: bool, output: &Value) -> String {
    let mark = if ok { s.green("✓") } else { s.red("✗") };
    let exit = failed_exit(output)
        .map(|c| format!(" {}", s.yellow(&format!("exit {c}"))))
        .unwrap_or_default();
    format!(
        "{} {mark}{exit}",
        s.dim(&rpad(&fmt_ms(elapsed_ms), TIME_COL))
    )
}

/// The mark and the sentence a run ends on.
///
/// A separate function because this is the line a person actually reads and
/// it must be testable. Every non-success reason is named: "completed" on its
/// own reads as success even when nothing the task asked for happened.
fn summary_of(s: Style, reason: &FinishReason) -> (String, String) {
    match reason {
        FinishReason::Completed => (s.green("✓"), "completed".to_owned()),
        FinishReason::StepLimit { max_steps } => (
            s.yellow("⚠"),
            format!("stopped at the {max_steps}-step budget"),
        ),
        FinishReason::Truncated => (s.yellow("⚠"), "truncated by the provider".into()),
        FinishReason::ModelFailed { message, .. } => {
            (s.red("✗"), format!("model failed: {message}"))
        }
        FinishReason::ComputerBusy { message } => (s.yellow("⏸"), format!("stopped — {message}")),
        // Not a warning. Nothing went wrong: the run was asked to stop, so
        // it is reported like a completed run rather than as a problem.
        FinishReason::Cancelled { message } => (s.dim("■"), message.clone()),
        FinishReason::TokenBudget { spent, budget } => (
            s.yellow("⚠"),
            format!("stopped at the {budget}-token budget after {spent}"),
        ),
        FinishReason::NothingApproved { message, tool } => (
            s.yellow("⚠"),
            format!("{message}; {}", nothing_approved_advice(tool)),
        ),
        // The provider said the model declined. Named, because a refusal
        // and a finished answer would otherwise both read "completed".
        FinishReason::Declined => (s.yellow("⚠"), "the model declined this request".to_owned()),
        FinishReason::HubFailed { message } => (s.red("✗"), format!("hub failed: {message}")),
    }
}

/// The exit code of a command that failed, if this result carries one.
///
/// `None` for a tool that has no such concept and for a command that
/// succeeded, so the ordinary row is unchanged. Read off the result rather
/// than special-cased by tool name: anything that reports an `exit_code` means
/// the same thing by it, and a name check would miss the next tool that does.
fn failed_exit(output: &Value) -> Option<i64> {
    match output.get("exit_code")?.as_i64()? {
        0 => None,
        other => Some(other),
    }
}

/// What to tell somebody whose run stopped because nothing could be approved.
///
/// `--approve auto` is not universal advice. It answers a gated action: the
/// operator accepts the risk out of band and the run proceeds. There is no
/// equivalent for a credential, because `ApprovalHandler::supply` refuses in
/// every unattended mode (there is no value a handler could invent). Offering
/// the flag anyway would send somebody to run a command that cannot work.
pub fn nothing_approved_advice(tool: &str) -> &'static str {
    if tool == "secret.request" {
        // Actionable and true: the value can be put in the store ahead of
        // time, and then the Bot never has to ask.
        "store it first with `botroster secret set <name>`"
    } else {
        "pass --approve auto for an unattended run"
    }
}

/// ANSI, or nothing at all when the output is not a terminal.
#[derive(Clone, Copy)]

pub struct Style {
    on: bool,
}

impl Style {
    /// Styling off, unconditionally. For tests and for rendering into a
    /// buffer that is not the terminal.
    #[allow(dead_code)] // used from the approve module's tests
    pub fn plain() -> Self {
        Self { on: false }
    }

    pub fn detect() -> Self {
        // NO_COLOR is a de-facto standard and costs one line to honour.
        let no_color = std::env::var_os("NO_COLOR").is_some();
        Self {
            on: std::io::stdout().is_terminal() && !no_color,
        }
    }

    fn wrap(self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    pub fn dim(self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn bold(self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn cyan(self, s: &str) -> String {
        self.wrap("36", s)
    }
    pub fn green(self, s: &str) -> String {
        self.wrap("32", s)
    }
    pub fn red(self, s: &str) -> String {
        self.wrap("31", s)
    }
    pub fn yellow(self, s: &str) -> String {
        self.wrap("33", s)
    }
}

/// A tool row that has been opened but not yet completed.
struct OpenRow {
    call_id: ToolCallId,
    tool: String,
    target: String,
}

pub struct Renderer {
    s: Style,
    /// Verbose mode prints full tool arguments and results.
    verbose: bool,
    /// Tool calls that failed or were refused this run.
    ///
    /// A run where every write was denied still ends with the loop reporting
    /// "completed", which is true but uninformative. The summary has to say
    /// what did not happen.
    failed: usize,
    /// Tool calls that succeeded.
    ok: usize,
    /// The row currently mid-print, if any.
    ///
    /// Keyed by `call_id` rather than by arrival order: progress frames reach
    /// the event stream through a separate task, so nothing guarantees a
    /// progress frame for one call arrives before the next call starts.
    /// Matching on the id means a stray frame is ignored instead of being
    /// written into the middle of somebody else's row.
    open: Option<OpenRow>,
}

impl Renderer {
    pub fn new(verbose: bool) -> Self {
        Self {
            s: Style::detect(),
            verbose,
            failed: 0,
            ok: 0,
            open: None,
        }
    }

    /// Print the head of a tool row: gutter, tool, target. No newline; the
    /// timing and mark complete it.
    fn print_head(&self, tool: &str, target: &str) {
        let s = self.s;
        print!(
            "  {} {}{}",
            s.dim("├─"),
            s.cyan(&pad(tool, TOOL_COL)),
            s.dim(&pad(target, TARGET_COL)),
        );
        let _ = std::io::stdout().flush();
    }

    /// Close an open row without timing. Used when something else needs to
    /// print first; it must not corrupt the layout.
    fn abandon_open_row(&mut self) {
        if self.open.take().is_some() {
            println!();
        }
    }

    pub fn header(&self, model: &str, hub: &str, tools: usize) {
        let s = self.s;
        println!();
        println!(
            "  {} {} {} {} {}",
            s.bold("botroster"),
            s.dim("·"),
            s.cyan(model),
            s.dim("·"),
            s.dim(&format!("{tools} tools · {hub}")),
        );
    }

    /// Note that this run began with work handed over by someone else.
    pub fn handoffs(&self, n: usize) {
        println!();
        println!(
            "  {} {}",
            self.s.cyan("↳"),
            self.s.dim(&format!(
                "picked up {n} handoff{}",
                if n == 1 { "" } else { "s" }
            ))
        );
    }

    pub fn task(&self, task: &str) {
        println!();
        // A task can be several lines (a handoff preamble arrives in front of
        // it), so every line after the first is indented to sit under the
        // marker instead of falling back to column zero.
        for (i, line) in task.lines().enumerate() {
            if i == 0 {
                println!("  {} {}", self.s.cyan("▸"), self.s.bold(line));
            } else if line.trim().is_empty() {
                println!("  {}", self.s.dim("│"));
            } else {
                println!("  {} {}", self.s.dim("│"), line);
            }
        }
        println!();
    }

    pub fn event(&mut self, e: &AgentEvent) {
        let s = self.s;
        match e {
            AgentEvent::Started { .. } => {}

            AgentEvent::Thinking { .. } => {
                // Transient: it exists to be overwritten. Without a TTY there
                // is nothing to overwrite it with, so printing it would leave
                // a permanent "thinking" line in every log and pipe.
                if s.on {
                    print!("  {} {}", s.yellow("✻"), s.dim("thinking"));
                    let _ = std::io::stdout().flush();
                }
            }

            // The person interrupted. Marked as theirs, like the task at the
            // top of the run, so a reader scrolling back can see why the Bot
            // changed direction.
            AgentEvent::Redirected { text } => {
                self.abandon_open_row();
                self.clear_line();
                println!();
                println!("  {} {text}", s.bold("▸"));
                println!();
            }

            AgentEvent::AssistantText { text, .. } => {
                // Clear the thinking line before writing over it.
                self.abandon_open_row();
                self.clear_line();
                for line in text.lines() {
                    println!("  {} {}", s.dim("│"), line);
                }
                println!("  {}", s.dim("│"));
            }

            AgentEvent::ToolCallStarted {
                call_id,
                tool,
                args,
                ..
            } => {
                self.clear_line();
                self.abandon_open_row();
                // Fixed columns so the duration and mark land in the same place
                // on every row. The head prints now, the timing completes it,
                // so a slow call is visible while it runs.
                let target = truncate(&summarize_args(args), TARGET_COL);
                self.print_head(tool, &target);
                self.open = Some(OpenRow {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    target,
                });
            }

            AgentEvent::ToolProgress { call_id, payload } => {
                // Only a terminal can be redrawn. Writing a stage into an open
                // row on a pipe would push the mark out of its column with no
                // way to repair it, so on a pipe progress is simply not shown.
                if !s.on {
                    return;
                }
                let Some(open) = self.open.as_ref() else {
                    return;
                };
                if open.call_id != *call_id {
                    return;
                }
                let Some(stage) = payload.get("stage").and_then(|v| v.as_str()) else {
                    return;
                };
                if stage == "finished" {
                    return;
                }
                // Redraw the whole row rather than appending, so the columns
                // survive however many stages arrive.
                let (tool, target) = (open.tool.clone(), open.target.clone());
                self.clear_line();
                self.print_head(&tool, &target);
                print!("{}", s.dim(&format!("… {stage}")));
                let _ = std::io::stdout().flush();
            }

            AgentEvent::ToolCallFinished {
                call_id,
                ok,
                output,
                elapsed_ms,
                ..
            } => {
                if *ok {
                    self.ok += 1;
                } else {
                    self.failed += 1;
                }
                match self.open.take() {
                    // Expected: complete the row opened above.
                    Some(open) if open.call_id == *call_id => {
                        if s.on {
                            // A progress stage may be sitting in the row.
                            self.clear_line();
                            self.print_head(&open.tool, &open.target);
                        }
                    }
                    // Out of order, or nothing open: print a fresh complete row
                    // rather than completing somebody else's.
                    other => {
                        if other.is_some() {
                            println!();
                        }
                        self.print_head("", "");
                    }
                }
                // A command that failed is not a call that failed. `ok` is
                // the tool call's outcome: `shell.exec` ran the command, got
                // an exit code, and returned it, so the call succeeded. The
                // exit code is read off the result and shown separately;
                // otherwise a command exiting non-zero prints a green tick
                // and no output.
                let exit = failed_exit(output);
                println!("{}", result_tail(s, *elapsed_ms, *ok, output));
                if !*ok || exit.is_some() {
                    // A failure is always shown: it is what the model is
                    // about to react to.
                    println!(
                        "  {} {}",
                        s.dim("│"),
                        s.red(&truncate(&render_value(output), 300))
                    );
                } else if self.verbose {
                    println!(
                        "  {} {}",
                        s.dim("│"),
                        s.dim(&truncate(&render_value(output), 300))
                    );
                }
            }

            AgentEvent::Finished {
                reason,
                steps,
                usage,
                text,
            } => {
                self.abandon_open_row();
                self.clear_line();
                println!();
                let (mark, label) = summary_of(s, reason);
                // Name the failures in the summary. "completed" on its own
                // reads as success even when nothing the task asked for
                // actually happened.
                let tally = match (self.ok, self.failed) {
                    (_, 0) => format!("· {}", plural(u64::from(*steps), "step")),
                    (0, f) => format!(
                        "· {} · all {} failed",
                        plural(u64::from(*steps), "step"),
                        plural(f as u64, "tool call")
                    ),
                    (_, f) => format!(
                        "· {} · {} failed",
                        plural(u64::from(*steps), "step"),
                        plural(f as u64, "tool call")
                    ),
                };
                // Tokens, never money: prices differ per model and change
                // without notice, and a printed cost would be believed.
                let tally = match (usage.input_tokens, usage.output_tokens) {
                    (0, 0) => tally,
                    (i, o) => format!("{tally} · {} in / {} out", tokens(i), tokens(o)),
                };
                let mark = if self.failed > 0 && matches!(reason, FinishReason::Completed) {
                    s.yellow("✓")
                } else {
                    mark
                };
                println!("  {} {} {}", mark, label, s.dim(&tally));
                if !text.is_empty() && !matches!(reason, FinishReason::Completed) {
                    println!("  {} {}", s.dim("│"), text);
                }
                println!();
            }
        }
    }

    fn clear_line(&self) {
        if self.s.on {
            print!("\r\x1b[2K");
        }
    }
}

/// Print a line, and stop cleanly if whoever was reading has gone away.
///
/// `println!` panics when stdout is closed, which happens the moment a long
/// listing is piped into `head` or `less` is quit. A panic and a backtrace
/// note is the wrong response to a reader that stopped reading; it looks like
/// the tool crashed.
///
/// The usual fix is restoring the default `SIGPIPE`, which needs `unsafe`;
/// this workspace forbids it, so the write is checked instead.
///
/// Used by the commands that emit an unbounded number of lines (listings and
/// transcripts, the ones people pipe). A one-line confirmation cannot
/// realistically hit this and keeps `println!`.
macro_rules! outln {
    ($($t:tt)*) => {{
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        if writeln!(out, $($t)*).is_err() {
            // Not an error: the reader is done, so this process is too.
            std::process::exit(0);
        }
    }};
}
pub(crate) use outln;

/// Replay a stored conversation, in the same shape a live run prints.
///
/// Tool results are omitted unless asked for: they are the bulk of a
/// transcript and almost never what someone scrolling back wants, which is
/// what the Bot was asked and what it said.
pub fn conversation(messages: &[Message], s: Style, full: bool) {
    for m in messages {
        match m.role {
            Role::User => {
                // A user message is either the task, or tool results being fed
                // back. Only the first gets a heading.
                let text = m.text();
                if !text.trim().is_empty() {
                    outln!(
                        "
  {} {}",
                        s.bold("▸"),
                        s.bold(&text)
                    );
                }
                if full {
                    for c in &m.content {
                        if let Content::ToolResult {
                            content, is_error, ..
                        } = c
                        {
                            let mark = if *is_error { s.red("!") } else { s.dim("·") };
                            for line in content.lines().take(20) {
                                outln!("  {}   {}", mark, s.dim(line));
                            }
                        }
                    }
                }
            }
            Role::Assistant => {
                for c in &m.content {
                    match c {
                        Content::Text { text } if !text.trim().is_empty() => {
                            for line in text.lines() {
                                outln!("  {} {line}", s.dim("│"));
                            }
                        }
                        Content::ToolUse { name, input, .. } => {
                            outln!(
                                "  {} {}  {}",
                                s.dim("├─"),
                                // `pad`, not `rpad`: the live renderer
                                // left-aligns tool names, and a replay should
                                // read the same as the live run.
                                s.cyan(&pad(name, 12)),
                                s.dim(&truncate(&summarize_args(input), 60))
                            );
                        }
                        // The match is total, so a new `Content` variant is
                        // a compile error here rather than a silent drop from
                        // the replay. Results are drawn by the user arm above,
                        // which walks the next message's tool results so they
                        // land under the call that produced them. Blank text
                        // is a model emitting an empty block, and a line of
                        // nothing is worse than no line.
                        Content::ToolResult { .. } | Content::Text { .. } => {}
                    }
                }
            }
        }
    }
}

/// The most useful single field of a tool's arguments (a path, a command),
/// falling back to compact JSON. This is what makes the log scannable.
fn summarize_args(args: &Value) -> String {
    for key in ["path", "command", "query", "url", "name"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return v.to_owned();
        }
    }
    match args {
        Value::Object(m) if m.is_empty() => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn render_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Human-scale duration. Milliseconds below a second, seconds above: a
/// four-digit millisecond count is harder to read than "1.4s".
fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Right-align within `width`, for a numeric column.
fn rpad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return s.to_owned();
    }
    format!("{}{s}", " ".repeat(width - n))
}

/// Left-align within `width`, always leaving at least one trailing space.
///
/// A name that exactly fills its column would otherwise run straight into the
/// next one (`browser.openhttp://…`), and a longer one pushes the row rather
/// than being cut, because a truncated tool name is worse than a ragged edge.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return format!("{s} ");
    }
    format!("{s}{}", " ".repeat(width - n))
}

/// Truncate by characters, never bytes: a byte slice through a multi-byte
/// character panics on the first non-ASCII path.
fn truncate(s: &str, n: usize) -> String {
    let flat = s.replace(['\n', '\r'], " ");
    if flat.chars().count() <= n {
        return flat;
    }
    flat.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn args_summarise_to_their_most_useful_field() {
        assert_eq!(summarize_args(&json!({"path": "a/b.txt"})), "a/b.txt");
        assert_eq!(summarize_args(&json!({"command": "ls -la"})), "ls -la");
        // Order matters: path wins over a generic name.
        assert_eq!(summarize_args(&json!({"name": "x", "path": "p"})), "p");
    }

    #[test]
    fn empty_args_render_as_nothing_not_as_braces() {
        assert_eq!(summarize_args(&json!({})), "");
    }

    #[test]
    fn args_with_no_known_field_fall_back_to_json() {
        assert_eq!(summarize_args(&json!({"depth": 2})), r#"{"depth":2}"#);
    }

    #[test]
    fn truncation_is_char_safe_and_flattens_newlines() {
        let s = "é".repeat(100);
        let out = truncate(&s, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert_eq!(truncate("a\nb", 10), "a b");
    }

    #[test]
    fn durations_read_at_human_scale() {
        assert_eq!(fmt_ms(0), "0ms");
        assert_eq!(fmt_ms(999), "999ms");
        assert_eq!(fmt_ms(1000), "1.0s");
        assert_eq!(fmt_ms(94_300), "94.3s");
    }

    #[test]
    fn numeric_columns_right_align_so_marks_line_up() {
        assert_eq!(rpad("5ms", 7), "    5ms");
        assert_eq!(rpad("1.4s", 7), "   1.4s");
        // Overlong values push the row rather than being silently cut.
        assert_eq!(rpad("12345678", 7), "12345678");
    }

    #[test]
    fn padding_never_truncates_a_long_name() {
        assert_eq!(pad("fs.read", 11), "fs.read    ");
        // Longer than the column: kept whole, with a separator.
        assert_eq!(pad("a.very.long.tool.name", 11), "a.very.long.tool.name ");
    }

    #[test]
    fn a_name_that_exactly_fills_its_column_still_gets_a_gap() {
        // `browser.open` is exactly 12 characters; without the trailing gap
        // the target runs straight into it: "browser.openhttp://...".
        assert_eq!(pad("browser.open", 12), "browser.open ");
        assert!(pad("browser.screenshot", 12).ends_with(' '));
    }

    #[test]
    fn a_multi_line_task_keeps_its_indent() {
        // Rendered into a buffer rather than stdout so the shape can be
        // asserted on.
        let s = Style::plain();
        let task = "While you were away:

- from **researcher**: sources ready

start on the note";
        let rendered: Vec<String> = task
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 {
                    format!("  {} {}", s.cyan("▸"), s.bold(line))
                } else if line.trim().is_empty() {
                    format!("  {}", s.dim("│"))
                } else {
                    format!("  {} {}", s.dim("│"), line)
                }
            })
            .collect();
        assert!(rendered[0].starts_with("  ▸ While you were away"));
        // No continuation line may start at column zero.
        assert!(rendered.iter().all(|l| l.starts_with("  ")), "{rendered:?}");
        assert!(rendered.last().unwrap().contains("start on the note"));
    }

    #[test]
    fn styling_is_a_no_op_when_disabled() {
        let s = Style { on: false };
        assert_eq!(s.green("ok"), "ok");
        assert_eq!(s.bold("ok"), "ok");
    }

    #[test]
    fn styling_wraps_when_enabled() {
        let s = Style { on: true };
        assert_eq!(s.green("ok"), "\x1b[32mok\x1b[0m");
    }
}

/// `1 step`, `5 steps`.
fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// `840`, `12.4k`, `1.2M`: a token count readable at a glance.
fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod usage_tests {
    use super::tokens;

    #[test]
    fn one_step_is_not_one_steps() {
        use super::plural;
        assert_eq!(plural(1, "step"), "1 step");
        assert_eq!(plural(0, "step"), "0 steps");
        assert_eq!(plural(5, "step"), "5 steps");
        assert_eq!(plural(1, "tool call"), "1 tool call");
    }

    #[test]
    fn token_counts_read_at_human_scale() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(840), "840");
        assert_eq!(tokens(12_400), "12.4k");
        assert_eq!(tokens(1_250_000), "1.2M");
    }
}

#[cfg(test)]
mod advice_tests {
    use super::*;

    /// Do not send somebody to run a command that cannot work.
    ///
    /// A run that stops because nobody could approve anything gets a line of
    /// advice. For a gated action `--approve auto` is the right answer: the
    /// operator accepts the risk out of band and it proceeds. For a credential
    /// it is guaranteed wrong: every unattended mode's `supply` returns
    /// `None`, so the flag changes nothing.
    #[test]
    fn a_credential_refusal_is_not_told_to_try_approve_auto() {
        let said = nothing_approved_advice("secret.request");
        assert!(
            !said.contains("--approve auto"),
            "advice that cannot work: {said}"
        );
        // It says something that can: the value goes in ahead of time.
        assert!(said.contains("botroster secret set"), "{said}");

        // The ordinary case keeps the advice that is true for it.
        for tool in ["fs.write", "shell.exec", "bot.send"] {
            assert!(
                nothing_approved_advice(tool).contains("--approve auto"),
                "{tool} lost the advice that works for it"
            );
        }
    }

    /// A declined turn does not read as a completed one.
    ///
    /// The summary is the line a person actually reads. If `refusal` and
    /// `content_filter` were folded into `EndTurn`, a refusal would print as
    /// `✓ completed`: two opposite outcomes, one sentence.
    #[test]
    fn a_declined_turn_is_named_in_the_summary() {
        let (mark, said) = summary_of(Style::plain(), &botroster_agent::FinishReason::Declined);
        assert!(said.contains("declined"), "{said}");
        assert!(
            !said.contains("completed"),
            "a refusal reads as a success: {said}"
        );
        assert_ne!(mark, "✓", "a refusal wears the completed tick");

        // The ordinary ending is untouched.
        let (ok, done) = summary_of(Style::plain(), &botroster_agent::FinishReason::Completed);
        assert_eq!(ok, "✓");
        assert_eq!(done, "completed");
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    /// A command that failed must not read as a success.
    ///
    /// `ok` on a tool result is the call's outcome: `shell.exec` ran the
    /// command, got an exit code, returned it, and the call succeeded. Without
    /// reading the exit code off the result, a command exiting non-zero would
    /// print a green tick and no output.
    #[test]
    fn a_nonzero_exit_is_read_off_the_result() {
        // The ordinary row is unchanged: nothing to say about a success.
        assert_eq!(failed_exit(&serde_json::json!({ "exit_code": 0 })), None);
        // A tool with no such concept says nothing either.
        assert_eq!(
            failed_exit(&serde_json::json!({ "path": "notes.md" })),
            None
        );
        assert_eq!(failed_exit(&serde_json::json!("a string")), None);

        // A failure is named, including the Windows status codes that do not
        // fit a byte and the negatives a signal produces.
        assert_eq!(failed_exit(&serde_json::json!({ "exit_code": 1 })), Some(1));
        assert_eq!(
            failed_exit(&serde_json::json!({ "exit_code": -1073741502i64 })),
            Some(-1073741502)
        );
    }

    /// Asserts on the rendered line, not the reader that feeds it: a test of
    /// the input to a rendering can pass while the rendering is wrong.
    #[test]
    fn the_row_names_a_failed_command_and_says_nothing_extra_about_a_good_one() {
        let plain = Style::plain();
        let good = result_tail(plain, 8, true, &serde_json::json!({ "exit_code": 0 }));
        assert!(good.contains('✓'), "{good}");
        assert!(
            !good.contains("exit"),
            "a successful command grew an annotation: {good}"
        );

        let bad = result_tail(
            plain,
            8,
            // The call succeeded: `shell.exec` ran the command and returned
            // its code. That is why the exit code needs its own annotation.
            true,
            &serde_json::json!({ "exit_code": -1073741502i64, "stdout": "" }),
        );
        assert!(bad.contains('✓'), "the call did succeed: {bad}");
        assert!(
            bad.contains("exit -1073741502"),
            "a command that never ran is indistinguishable from one that worked: {bad}"
        );

        // A tool with no exit code is untouched.
        let other = result_tail(plain, 3, true, &serde_json::json!({ "path": "notes.md" }));
        assert!(!other.contains("exit"), "{other}");
    }
}
