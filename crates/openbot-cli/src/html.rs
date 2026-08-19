//! A self-contained HTML view of a session.
//!
//! `docs/SPEC.md` §10 calls for a read-only session view: one file, no
//! network, no build step, openable from a file:// URL. A backend developed
//! with no view of itself drifts toward shapes that are awkward to render.
//!
//! It consumes the same `AgentEvent` stream the terminal renderer does. If
//! something is visible in one surface and not the other, the event is
//! missing, not the surface.

use openbot_agent::agent::{AgentEvent, FinishReason};
use serde_json::Value;

pub struct Session<'a> {
    pub task: &'a str,
    pub model: &'a str,
    pub hub: &'a str,
    pub events: &'a [AgentEvent],
}

pub fn render(s: &Session) -> String {
    let mut body = String::new();

    for e in s.events {
        match e {
            AgentEvent::Started { tools, .. } => {
                body.push_str(&format!(
                    r#"<div class="tools">{}</div>"#,
                    tools
                        .iter()
                        .map(|t| format!(r#"<span class="chip">{}</span>"#, esc(t)))
                        .collect::<Vec<_>>()
                        .join("")
                ));
            }
            AgentEvent::Thinking { .. } => {}
            AgentEvent::Redirected { text } => {
                // The person, mid-run. Shown as theirs so the change of
                // direction has a visible cause.
                body.push_str(&format!(r#"<div class="task">{}</div>"#, esc(text)));
            }
            AgentEvent::AssistantText { text, .. } => {
                body.push_str(&format!(r#"<div class="say">{}</div>"#, esc_multi(text)));
            }
            AgentEvent::ToolCallStarted {
                call_id,
                tool,
                args,
                ..
            } => {
                // The row opens here and is closed by the matching finish. A
                // call with no finish stays visibly open, which is the correct
                // rendering of a run that was cut off.
                body.push_str(&format!(
                    r#"<details class="call" id="c-{id}" open><summary><span class="tool">{tool}</span><span class="target">{target}</span><span class="time" data-pending>running…</span></summary><div class="io"><div class="lbl">arguments</div><pre>{args}</pre>"#,
                    id = esc(call_id.as_str()),
                    tool = esc(tool),
                    target = esc(&summarize(args)),
                    args = esc(&pretty(args)),
                ));
            }
            AgentEvent::ToolProgress { payload, .. } => {
                if let Some(stage) = payload.get("stage").and_then(|v| v.as_str()) {
                    body.push_str(&format!(r#"<div class="stage">{}</div>"#, esc(stage)));
                }
            }
            AgentEvent::ToolCallFinished {
                ok,
                output,
                elapsed_ms,
                ..
            } => {
                body.push_str(&format!(
                    r#"<div class="lbl">{label}</div><pre class="{cls}">{out}</pre></div></details><script>closeCall({ok}, "{ms}")</script>"#,
                    label = if *ok { "result" } else { "refused" },
                    cls = if *ok { "ok" } else { "bad" },
                    out = esc(&pretty(output)),
                    ok = ok,
                    ms = fmt_ms(*elapsed_ms),
                ));
            }
            AgentEvent::Finished {
                reason,
                steps,
                text,
                usage,
            } => {
                let (cls, label) = match reason {
                    FinishReason::Completed => ("done", "completed".to_owned()),
                    FinishReason::StepLimit { max_steps } => {
                        ("warn", format!("stopped at the {max_steps}-step budget"))
                    }
                    FinishReason::Truncated => ("warn", "truncated by the provider".into()),
                    FinishReason::ModelFailed { message, .. } => {
                        ("bad", format!("model failed: {}", message))
                    }
                    FinishReason::ComputerBusy { message } => {
                        ("warn", format!("stopped — {message}"))
                    }
                    // Neutral, not a warning: a requested stop is not a fault
                    // of the run.
                    FinishReason::Cancelled { message } => ("", message.clone()),
                    FinishReason::TokenBudget { spent, budget } => (
                        "warn",
                        format!("stopped at the {budget}-token budget after {spent}"),
                    ),
                    FinishReason::NothingApproved { message, tool } => (
                        "warn",
                        format!(
                            "{message}; {}",
                            crate::render::nothing_approved_advice(tool)
                        ),
                    ),
                    FinishReason::Declined => {
                        ("warn", "the model declined this request".to_owned())
                    }
                    FinishReason::HubFailed { message } => {
                        ("bad", format!("hub failed: {}", message))
                    }
                };
                let failed = s
                    .events
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::ToolCallFinished { ok: false, .. }))
                    .count();
                let tally = if failed > 0 {
                    format!("{steps} steps · {failed} tool calls failed")
                } else {
                    format!("{steps} steps")
                };
                // Tokens rather than a price: what a run costs in money depends
                // on a rate this code does not know and cannot keep current.
                let tally = match (usage.input_tokens, usage.output_tokens) {
                    (0, 0) => tally,
                    (i, o) => format!("{tally} · {i} tokens in / {o} out"),
                };
                body.push_str(&format!(
                    r#"<div class="end {cls}"><strong>{label}</strong> <span class="muted">{tally}</span></div>"#,
                    label = esc(&label),
                    tally = esc(&tally),
                ));
                if !text.is_empty() {
                    body.push_str(&format!(
                        r#"<div class="say final">{}</div>"#,
                        esc_multi(text)
                    ));
                }
            }
        }
    }

    TEMPLATE
        .replace("{{TASK}}", &esc(s.task))
        .replace("{{MODEL}}", &esc(s.model))
        .replace("{{HUB}}", &esc(s.hub))
        .replace("{{BODY}}", &body)
}

fn summarize(args: &Value) -> String {
    for key in ["path", "command", "query", "url", "name"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return v.to_owned();
        }
    }
    String::new()
}

fn pretty(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Escape for HTML text and attribute context.
///
/// Tool output is arbitrary bytes from a model and a shell. Rendering it
/// unescaped would make every transcript a stored-XSS delivery mechanism for
/// whoever opens the file.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn esc_multi(s: &str) -> String {
    esc(s).replace('\n', "<br>")
}

const TEMPLATE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{{TASK}} · openbot</title>
<style>
:root{
  --bg:#fbfbfa; --fg:#1a1a19; --muted:#6b6b66; --line:#e6e5e1;
  --accent:#2f6f5e; --warn:#8a6d1f; --bad:#9b3232; --card:#fff; --code:#f4f3f0;
}
@media (prefers-color-scheme:dark){:root:not([data-theme=light]){
  --bg:#131312; --fg:#e9e8e4; --muted:#8e8d87; --line:#2a2a27;
  --accent:#6fbfa5; --warn:#d3b05a; --bad:#e08a8a; --card:#1a1a18; --code:#201f1d;
}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
  font:15px/1.6 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;}
.wrap{max-width:52rem;margin:0 auto;padding:3rem 1.25rem 6rem}
header{border-bottom:1px solid var(--line);padding-bottom:1.25rem;margin-bottom:2rem}
.brand{font-weight:600;letter-spacing:-.01em}
.meta{color:var(--muted);font-size:.82rem;margin-top:.35rem}
h1{font-size:1.35rem;line-height:1.35;margin:1.5rem 0 .25rem;font-weight:600;letter-spacing:-.015em}
.tools{display:flex;flex-wrap:wrap;gap:.35rem;margin:.75rem 0 1.5rem}
.chip{font:11px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--muted);
  border:1px solid var(--line);border-radius:99px;padding:.15rem .5rem}
.say{margin:1.1rem 0;padding-left:.9rem;border-left:2px solid var(--line)}
.say.final{border-left-color:var(--accent)}
.call{margin:.5rem 0;background:var(--card);border:1px solid var(--line);border-radius:8px;
  overflow:hidden}
.call summary{cursor:pointer;list-style:none;display:flex;align-items:center;gap:.6rem;
  padding:.5rem .75rem;font:12px ui-monospace,SFMono-Regular,Menlo,monospace}
.call summary::-webkit-details-marker{display:none}
.tool{color:var(--accent);font-weight:600}
.target{color:var(--muted);flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.time{color:var(--muted);font-variant-numeric:tabular-nums}
.time.bad{color:var(--bad)}
.io{padding:0 .75rem .75rem;border-top:1px solid var(--line)}
.lbl{font-size:10px;text-transform:uppercase;letter-spacing:.08em;color:var(--muted);
  margin:.7rem 0 .3rem}
pre{margin:0;padding:.6rem .7rem;background:var(--code);border-radius:6px;
  font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;
  white-space:pre-wrap;word-break:break-word;overflow-x:auto;max-height:22rem}
pre.bad{color:var(--bad)}
.stage{font:11px ui-monospace,monospace;color:var(--muted);padding:.15rem .75rem}
.end{margin:2rem 0 .5rem;padding-top:1.25rem;border-top:1px solid var(--line)}
.end.done strong{color:var(--accent)} .end.warn strong{color:var(--warn)}
.end.bad strong{color:var(--bad)}
.muted{color:var(--muted)}
</style></head><body><div class="wrap">
<header>
  <div class="brand">openbot</div>
  <div class="meta">{{MODEL}} · {{HUB}}</div>
</header>
<h1>{{TASK}}</h1>
{{BODY}}
</div>
<script>
// Each finished call closes the row opened just above it. Written as a call
// rather than emitted inline so the markup stays readable.
function closeCall(ok, ms){
  const rows=document.querySelectorAll('.call');
  const row=rows[rows.length-1]; if(!row) return;
  const t=row.querySelector('.time');
  t.textContent=ms; t.removeAttribute('data-pending');
  if(!ok) t.classList.add('bad');
  // Collapse successful calls; leave failures open, because a failure is what
  // the reader is looking for.
  if(ok) row.removeAttribute('open');
}
document.querySelectorAll('script').forEach(s=>s.remove());
</script>
</body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use openbot_agent::model::ToolUseId;
    use openbot_proto::ToolCallId;
    use serde_json::json;

    fn ev_call(tool: &str, args: Value) -> AgentEvent {
        AgentEvent::ToolCallStarted {
            step: 1,
            id: ToolUseId::new("t1"),
            call_id: ToolCallId::new("c1"),
            tool: tool.into(),
            args,
        }
    }

    #[test]
    fn hostile_tool_output_cannot_inject_markup() {
        let events = vec![
            ev_call("fs.read", json!({ "path": "x" })),
            AgentEvent::ToolCallFinished {
                id: ToolUseId::new("t1"),
                call_id: ToolCallId::new("c1"),
                ok: true,
                output: json!("<script>alert('pwned')</script>"),
                elapsed_ms: 3,
            },
        ];
        let html = render(&Session {
            task: "t",
            model: "m",
            hub: "h",
            events: &events,
        });
        assert!(!html.contains("<script>alert"), "unescaped script tag");
        assert!(html.contains("&lt;script&gt;alert"));
    }

    #[test]
    fn the_task_is_escaped_in_both_title_and_heading() {
        let html = render(&Session {
            task: r#"</title><img src=x onerror=alert(1)>"#,
            model: "m",
            hub: "h",
            events: &[],
        });
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;/title&gt;"));
    }

    #[test]
    fn a_failed_call_is_marked_and_left_open() {
        let events = vec![
            ev_call("fs.write", json!({ "path": "../nope" })),
            AgentEvent::ToolCallFinished {
                id: ToolUseId::new("t1"),
                call_id: ToolCallId::new("c1"),
                ok: false,
                output: json!("path escapes the workspace"),
                elapsed_ms: 2,
            },
        ];
        let html = render(&Session {
            task: "t",
            model: "m",
            hub: "h",
            events: &events,
        });
        assert!(html.contains("refused"));
        assert!(html.contains("closeCall(false"));
        assert!(html.contains("path escapes the workspace"));
    }

    #[test]
    fn the_summary_names_failures() {
        let events = vec![
            ev_call("fs.write", json!({})),
            AgentEvent::ToolCallFinished {
                id: ToolUseId::new("t1"),
                call_id: ToolCallId::new("c1"),
                ok: false,
                output: json!("denied"),
                elapsed_ms: 1,
            },
            AgentEvent::Finished {
                reason: FinishReason::Completed,
                usage: Default::default(),
                steps: 2,
                text: "all done".into(),
            },
        ];
        let html = render(&Session {
            task: "t",
            model: "m",
            hub: "h",
            events: &events,
        });
        assert!(html.contains("1 tool calls failed"));
    }

    #[test]
    fn the_page_is_self_contained() {
        let html = render(&Session {
            task: "t",
            model: "m",
            hub: "h",
            events: &[],
        });
        // No network at all: a transcript must open from a file:// URL, and
        // must not phone anywhere with someone's session contents.
        for bad in ["http://", "https://", "//cdn", "src=\"//"] {
            assert!(!html.contains(bad), "external reference: {bad}");
        }
    }
}
