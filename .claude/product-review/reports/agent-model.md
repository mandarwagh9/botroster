# Agent & Model — review

**Reviewed:** in full — `crates/botroster-agent/src/` (`agent.rs`, `hub_client.rs`, `model.rs`,
`transient.rs`, `lib.rs`, `providers/http.rs`, `providers/scripted.rs`, `providers/mod.rs`) and
`crates/botroster-bots/src/lib.rs`. Read in full for anchors and coverage: `tests/agent_loop.rs`,
`tests/vendor.rs`, `tests/continuity.rs`, `tests/handoff_tool.rs`. Read in outline only (test names
and doc headers, not every assertion): `tests/approvals.rs`, `tests/refused.rs`, `tests/secrets.rs`,
`crates/botroster-bots/src/schedule.rs` — none of them changed a finding. `CLAUDE.md`,
`CONTRIBUTING.md`, `docs/SPEC.md` §4–§8. Read out of scope, only far enough to confirm or kill a
finding: `botroster-cli/src/main.rs` (the two run paths and `routine tick`),
`botroster-cli/src/config.rs`, `botrosterd/src/hub.rs` (`tool_call`, `disconnect`),
`botroster-guest/src/tools.rs` (schemas and timeouts). No file was modified.

**Verdict:** Inside a single run this loop is genuinely frontier-grade, and in places better than
what it is copying: cancellation windows reasoned out to the instant, `tool_use`/`tool_result`
pairing enforced by a function signature, a refusal kept distinct from a finished answer, an absent
approver kept distinct from a person saying no. That rigor stops dead at the run boundary — every
defect below lives where state crosses into durable storage or comes back out of it. **The biggest
single gap: the loop protects the conversation it holds in memory and corrupts the one it
persists.** A close second is that there is no retry anywhere in the harness, so a single 429 ends a
run a frontier product would not even have reported.

## Findings

### F-AM1 — A tool server that dies mid-call hangs the run forever, and the only way out discards the run
`P0` · `reach: some users` · `crates/botroster-agent/src/agent.rs:731`, `crates/botroster-agent/src/hub_client.rs:308`

**What is true now.** `HubClient::call` awaits its oneshot with no deadline
(`hub_client.rs:308`), and `drive` awaits `call_tool` bare — it is the one await in the loop that is
*not* wrapped in the cancel `select!` that guards the model turn at `agent.rs:588`. The harness's
only liveness guarantee is that the peer eventually answers or the socket dies. Neither is
guaranteed. `botrosterd`'s `tool.call` path inserts a relay and returns `None`
(`botrosterd/src/hub.rs:1230-1259`); nothing times it out — there are deadlines for `session.bind`
(30s), approvals (120s) and `PreToolUse` hooks, but none for the tool itself. And `Hub::disconnect`
reaps relays only by *origin*: `st.relays.retain(|_, r| r.origin_conn != *id)` and
`st.calls.retain(|_, c| c.origin != *id)` (`hub.rs:381-382`). Both keys are the harness's
connection. So when the *guest* socket drops with a call outstanding, the relay is kept, no error
is ever synthesised, and the harness's `w_rx` never resolves. `pending_relays()` (`hub.rs:275`)
exists to count exactly these and nothing ever drains them.

**Why it matters.** `botroster run` sits on "thinking" indefinitely. The first Ctrl-C prints
"stopping — press Ctrl-C again to quit without saving" and does nothing, because cancellation is
only observed at the top of the loop and the loop is inside `call_tool`. The second Ctrl-C calls
`std::process::exit(130)` (`main.rs:2582`), which skips the `bots.append(&b.id, fresh)` at
`main.rs:2604` — so the entire run's transcript, every file it read and every command it ran, is
gone from the Bot's conversation. A crashed guest is not an exotic event: SPEC §4 devotes a whole
subsection to the guest being `kill -9`'d and to it reconnecting for as long as its process lives.
It does reconnect — as a *new* connection, with the old call still orphaned.

**The durable fix.** The harness must not depend on a remote peer for its own liveness. Give
`HubClient::call` a deadline it owns (a per-method one; a tool call's is necessarily long, and
`shell.exec` already advertises its own `timeout_secs` in its schema, so the harness can derive a
ceiling from the request instead of guessing), and on expiry remove the pending entry and return a
typed `HubError::TimedOut` that `is_transient` classifies. Separately, put `call_tool` inside the
same `select!` as the model turn, gated so a cancel arriving during a call *abandons the wait* but
still writes a `tool_result` for the outstanding `tool_use` — the invariant
`stopping_never_leaves_a_tool_call_hanging` already guards. The hub-side leak
(reaping relays by target as well as origin) is Platform's, but the harness should be correct even
if the hub is not.

**How to prove it.** In `agent_loop.rs`, extend the rig with a tool server that accepts
`tool.call_request` and then drops its socket without replying. Assert the run finishes within a
bounded time with a `HubFailed`/timeout reason and a transcript in which every `tool_use` has a
`tool_result` (reuse `unanswered_calls` at `agent_loop.rs:623`). Today the test hangs until the
suite's own timeout.

---

### F-AM2 — The history window hands the provider a `tool_result` with no `tool_use`, and a routine loses the firing
`P0` · `reach: most users` · `crates/botroster-bots/src/lib.rs:465-471`, `crates/botroster-cli/src/main.rs:2522`, `crates/botroster-cli/src/main.rs:2847`

**What is true now.** `BotStore::history(id, Some(n))` returns `parse_lines(tail_lines(path, n))` —
the last *n* lines of the JSONL log, chosen with no regard for what they are. The CLI seeds every
run with `Some(history)`, default `DEFAULT_HISTORY = 40` (`main.rs:2752`), and hands the result
straight to `Agent::with_history`. A tool-using Bot's log is a repeating
`Assistant{ToolUse}` / `User{ToolResult}` pattern, so a 40-line window frequently *starts* on a
`User{ToolResult}` message whose matching `Assistant{ToolUse}` is line 41 —
excluded. Nothing repairs it: `compact` (`agent.rs:69`) only replaces contents, the Anthropic
translation emits the `tool_result` block unconditionally (`http.rs:260-269`) and the OpenAI one
emits a bare `role:"tool"` message (`http.rs:396-400`). Both vendors 400 the request.
`parse_lines` dropping an unreadable line (`lib.rs:494-512`) is a second, independent way to orphan
a pair.

This is the exact hazard the suite already understands from the other direction. `agent_loop.rs:616-622`:
*"A vendor rejects a request whose tool calls are unanswered, so a transcript that breaks this does
not break the run that produced it; it breaks the next run on that Bot, on another day, with a 400
nobody would trace back to a cancellation."* The mirror case — an orphaned result at the head of the
window — is unguarded.

**Why it matters.** The failure heals itself on the next attempt, and that is what makes it bad
rather than what makes it benign. `fresh` is `[Message::user(task)]` and *is* appended (the previous
log line is an assistant message, so `append`'s dedup at `lib.rs:437` does not fire), so the log
grows by one, the window start advances by one onto the `Assistant{ToolUse}` that owns the orphan,
and the next run is legal. So interactively the user gets one opaque `ModelFailed { transient:
false }` carrying a vendor message about `tool_use_id` that points at nothing they did, retypes the
task, and it works — a failure that cannot be reproduced is harder to diagnose than a stable one,
and nobody will ever file it. **For a routine it costs a firing.** A nightly digest loses a whole
day, recorded as `retryable: false` with a summary nobody can act on, then heals and recurs at
whatever rate the 40-message window lands badly — which, for a Bot doing tool work every night, is
often. That is the run nobody is watching, failing in the way this codebase keeps saying it must
not. (`append`'s dedup can additionally freeze the window in the narrow case where the shift-by-one
lands on a second orphan; that needs `parse_lines` to have dropped a line, and is not the common
path.)

**The durable fix.** A conversation window is not a line count; make that a type-level fact rather
than a convention. `history(limit)` should return a *repaired* window: after taking the tail,
advance the start until the first message is a legal conversation opener (no `ToolResult` in it),
or, better, walk back to include the `Assistant{ToolUse}` that answers it. The strongest version is
to make the repair impossible to skip — have `history` return a `Conversation` newtype whose only
constructor enforces "every `ToolResult` in this slice is preceded by its `ToolUse`", and have
`Agent::with_history` take that type. `compact`'s `&mut [Message]` signature trick is exactly this
idea; the load path deserves the same treatment. The `append` de-duplication should also be
narrowed so it cannot freeze the window: dedupe on "the previous run produced no assistant output",
not on message equality alone.

**How to prove it.** In `continuity.rs`, drive a Bot through enough tool-calling runs that its log
exceeds 40 messages, arrange for line 41-from-the-end to be an `Assistant{ToolUse}`, then run again
against the `vendor.rs` fake and assert the outgoing request contains no `role:"tool"` /
`tool_result` without a preceding call — the same walk `compaction_never_breaks_the_tool_call_pairing`
(`vendor.rs:924`) already performs, applied to the *seeded* half of the conversation instead of the
generated half. The cheapest form needs no vendor at all: a unit test that builds a log of
alternating pairs, calls `history(id, Some(n))` for every `n` from 1 to 40, and asserts that no
returned window begins with a `ToolResult`. Today roughly half of them do.

---

### F-AM3 — Compaction placeholders are written to the Bot's permanent conversation
`P1` · `reach: some users` · `crates/botroster-agent/src/agent.rs:512`, `crates/botroster-agent/src/agent.rs:826`, `crates/botroster-cli/src/main.rs:2604`

**What is true now.** `compact` mutates `messages` in place, overwriting old `ToolResult` contents
with `"[earlier result dropped to fit the context — run the tool again if you still need it]"`.
`finish` then hands that same vector out as `transcript: messages.to_vec()` (`agent.rs:826`), and
both run paths persist the tail of it verbatim: `bots.append(&b.id, fresh)` (`main.rs:2604`) and
`run_task`'s `Thread::Own => bots.append(&bot.id, fresh)` (`main.rs:2891`). A context-fitting
decision, made for one turn of one run, is written to disk as the Bot's memory.

**Why it matters.** SPEC §4 opens with *"context compounds instead of resetting is the whole product
promise."* This is the promise inverted. Once a run crosses 120k characters — roughly fifteen
full-size tool results, well inside the default 24-step budget for a Bot that reads files or runs
commands — every result it gathered before the last three exchanges is permanently replaced by a
placeholder. The next run loads a history in which the Bot appears to have found nothing. There is
no undo: the JSONL log is append-only and the bytes were never written. It compounds with F-AM4:
a 429 halfway through a long run still appends the partial transcript, placeholders included, so a
single rate limit can permanently gut a Bot's memory of work it actually did.

**The durable fix.** Separate the durable transcript from the working context. `drive` should keep
the untouched conversation and compact a *clone* for the request, or `AgentOutcome` should carry a
`transcript` built from the pre-compaction messages while the compacted copy stays local to the
loop. The invariant to encode: what is sent to a provider is a projection of the transcript, never
the transcript itself. If the placeholder is genuinely wanted on disk it should be a distinct
`Content` variant that records what was elided, not a string that overwrites it.

**How to prove it.** In `continuity.rs`, run a Bot with a small `context_budget` and a scripted
provider that produces several large tool results, then read the log back with `history(id, None)`
and assert no stored `ToolResult` contains `"run the tool again"`. Today it does.

---

### F-AM4 — Nothing retries: one 429 or one dropped connection ends the run
`P1` · `reach: most users` · `crates/botroster-agent/src/agent.rs:588-601`, `crates/botroster-agent/src/providers/http.rs:142`

**What is true now.** There is no retry, no backoff and no jitter anywhere in `botroster-agent` — I
grepped the crate for it. `http.rs:142` carefully classifies 408/429/5xx as `Overloaded`, and
`agent.rs:590` reacts to it by returning `model_failed(e)` and ending the run. The classification is
used only to *label* the corpse: `FinishReason::ModelFailed { transient: true }`, which a routine
reads at `main.rs:1432` to decide whether to schedule another attempt ten minutes later. An
interactive run gets nothing at all — the user sees the run stop and retypes the task.

**Why it matters.** 429s and 529s are ordinary weather on every frontier API, and they get *more*
likely as a run goes on, because every turn resends a larger conversation. A twenty-step run that
dies at step eighteen on a rate limit is the single most expensive failure this loop can have, and
the recovery on offer is "run it again", which re-pays for all eighteen steps and re-executes every
side effect. Grok Bot users will not experience this; BOTROSTER users will experience it weekly. It is
also the difference between `transient: true` meaning something and meaning nothing: the flag says
"waiting would fix this" and then nobody waits.

**The durable fix.** Retry belongs in the loop, not in a provider, for the same reason the
`Model` trait doc gives — *"a provider is a translation layer, not a place for behaviour; anything
clever belongs in the agent loop, where it is testable against the scripted provider."* Add a
bounded retry around `model_turn_or_cancel` keyed on `transient::model_failure`: exponential
backoff with jitter, a small attempt cap, honouring `Retry-After` when the provider sends one
(which means `ModelError::Overloaded` needs to carry it rather than stringify it — the same
argument that created the variant). Every wait must sit inside the cancel `select!`, so a stop
button is not made worse by the fix, and every attempt must emit an `AgentEvent` so a retry is
visible rather than a mysterious pause. Only after the cap is exhausted does the run finish with
`ModelFailed { transient: true }`.

**How to prove it.** In `vendor.rs`, serve `[429, 429, 200-with-an-answer]` and assert the run
completes, that the vendor saw three requests, and that the elapsed time is at least the configured
backoff. Then serve unbroken 429s and assert the run gives up after the cap rather than spinning.

---

### F-AM5 — The transient/permanent classification is discarded at two boundaries, so a routine skips a day for a thirty-second outage
`P1` · `reach: some users` · `crates/botroster-agent/src/agent.rs:173-175`, `crates/botroster-agent/src/transient.rs:41`, `crates/botroster-cli/src/main.rs:1432-1440`, `crates/botroster-agent/src/providers/http.rs:154`

**What is true now.** Two places compute the answer and then throw it away.

`FinishReason::ModelFailed` carries a `transient: bool`; `FinishReason::HubFailed`
(`agent.rs:173-175`) carries only a message. But `transient.rs:41` states plainly that
`HubError::Closed` *is* transient — and the module's own header names the motivating scenario:
*"a nightly digest that hits a thirty-second hub restart waits twenty-four hours."* `agent.rs:742`
converts exactly that error into a `HubFailed` with the bit stripped, and `main.rs:1432` therefore
records `retryable: false`. The digest waits twenty-four hours. Note the asymmetry: a hub that is
down *before* the run is retryable (`main.rs:1450`, `is_transient(e)` on the connect error); a hub
that goes down *during* the run — after the run has spent tokens — is not.

Second: `http.rs:154` maps every body-level provider error to `ModelError::Rejected`, unconditionally.
`vendor.rs:1042` proves the case that matters, feeding in Anthropic's
`{"type":"error","error":{"type":"overloaded_error",...}}` delivered with a 200 — which
`provider_error` correctly extracts and then labels permanent. Gateways in front of these APIs do
this routinely, and `--base-url` exists so people can point BOTROSTER at one.

I am aware of `a_hub_failure_is_a_failure_too_and_is_not_assumed_retryable` (`acp/mod.rs:392`) and
am not arguing with it. That test answers "should an ACP client silently re-issue this turn?" —
no. `Run::retryable` answers a different question: "does the day's work still need doing?" —
for `HubError::Closed`, `transient.rs` already says yes. Two consumers, two questions, one bit that
only one of them can see.

**Why it matters.** Routines are the whole of SPEC §8 and the reason `transient.rs` exists. A
control-plane restart — routine, the SPEC says so — silently costs every routine that was mid-run
its entire firing, with a run record that reads like a permanent fault.

**The durable fix.** Make the classification a property of the finish reason rather than of one
variant: give `FinishReason` a `pub fn worth_repeating(&self) -> bool` computed once, at the point
where the typed error is converted, and have `main.rs`'s two `retryable:` sites call it instead of
pattern-matching a subset of variants — that way a new variant cannot silently default to "no".
`HubFailed` gains the same `transient` field, populated from `transient::is_transient`. On the
provider side, `provider_error` should return the error *type* alongside the message so `turn` can
route `overloaded_error` / `rate_limit_error` to `ModelError::Overloaded`, which is what the variant
was created for.

**How to prove it.** A routine test that kills and restarts the hub mid-run and asserts the recorded
`Run` has `retryable: true`. And extend `an_error_delivered_with_a_200_is_still_an_error`
(`vendor.rs:1042`) to assert `matches!(err, ModelError::Overloaded(_))` for the `overloaded_error`
body — today it asserts only that the message survives.

---

### F-AM6 — Compaction is best-effort with a hard floor, and cannot shrink text at all
`P1` · `reach: some users` · `crates/botroster-agent/src/agent.rs:69-101`, `crates/botroster-agent/src/agent.rs:47`

**What is true now.** `compact` has no post-condition. It walks messages older than `KEEP_RECENT`,
replaces `ToolResult` contents, and returns however many it replaced — whether or not the budget was
reached. It touches nothing else: a `Content::Text` block is never shortened, by design
(`assistant_reasoning_is_never_compacted`). Two consequences follow arithmetically. The last
`KEEP_RECENT = 6` messages are untouchable, and each may hold `RESULT_CHAR_LIMIT = 8_000`
characters, so there is a floor of roughly 24–48k characters that compaction cannot go below at
any budget. And a conversation whose bulk is assistant prose or a large pasted user message cannot
be shrunk *at all*. In both cases the loop proceeds and sends the oversized request anyway.

**Why it matters.** The provider answers with a context-length error, which `http.rs:142` classifies
as `Rejected` — non-transient — so it surfaces as `ModelFailed { transient: false }` and a routine
marks the day permanently failed. The comment on `CONVERSATION_CHAR_BUDGET` (`agent.rs:37-40`)
predicts this failure precisely — *"the run dies around step five with a vendor error about context
length, which reads as a model failure rather than a run that went on too long, and lands in a
routine nobody is watching"* — and the mechanism written to prevent it does not guarantee it. Worse,
it is monotone: history only grows, so once a Bot crosses the line it stays across it. That is the
cliff, and it lands hardest on exactly the small local models the provider layer exists to support.

No test can currently observe this. `compaction_never_breaks_the_tool_call_pairing` (`vendor.rs:924`)
sets `context_budget: 2_000`, runs a conversation that is certainly still tens of thousands of
characters after compacting, and passes — because it asserts pairing, and because the fake vendor
has no context limit to violate.

**The durable fix.** Give compaction a contract and let the caller see it fail: return
`Result<usize, TooLarge { got, budget }>` (or a `CompactionOutcome` naming the shortfall) so the
loop can act rather than proceed blindly. Then give it a second lever that can actually reach the
budget — truncating old `Text` blocks with the same "run the tool again"-style marker, or, better,
the summarisation step this loop does not have: fold the dropped span into one assistant-authored
summary message so the run degrades to a shorter memory rather than to an instruction to re-fetch
everything. Re-fetching is a thrash: results dropped to fit are re-run, produce new results, and are
dropped again. And when the budget is still unreachable, finish with a distinct reason
("the conversation no longer fits") rather than letting a vendor say it in its own words.

**How to prove it.** A unit test on `compact` alone: build a conversation of pure `Content::Text`
larger than the budget, call it, and assert the post-condition holds — today `size(messages)` is
still over budget and the return value is `0`, which is indistinguishable from "nothing needed
doing". A second one with `keep_recent: 6` and six 8k results, asserting the same.

---

### F-AM7 — `context_budget` is configuration nothing can set
`P1` · `reach: some users` · `crates/botroster-agent/src/agent.rs:255-261`, `crates/botroster-cli/src/config.rs:47-68`

**What is true now.** `AgentConfig::context_budget` is `pub`, carries a doc comment explaining why
it must be tunable — *"models differ by an order of magnitude in how much they can hold: the default
is wrong for a 32k model in one direction and a million-token one in the other"* — and is set by
exactly one caller in the workspace: `vendor.rs:950`, a test. Both production run paths build
`AgentConfig { max_steps, system, token_budget, ..Default::default() }` (`main.rs:2588`,
`main.rs:2854`), so every run on every model gets 120,000 characters. `ModelSettings`
(`config.rs:47`) has `max_tokens` and `token_budget` but no context field, and there is no
`--context-budget` flag.

**Why it matters.** The product's thesis is that any model works, and this is the one knob that
makes a small model viable. Left at the default, a 32k-context local model is handed ~30k tokens of
conversation plus a system prompt plus a tool catalogue and fails — the exact scenario the constant's
own comment describes. In the other direction a 1M-context model is compacted away from data it
could easily have held, so long tasks degrade for no reason. A field that reads like a control and
is unreachable is worse than no field: it makes the gap look solved.

**The durable fix.** Put it where the other model-shaped limits already live —
`ModelSettings.context_budget`, `botroster config set --context-budget`, a `--context-budget` run flag
through `ModelOverrides`, plumbed into both `AgentConfig` constructions. `botroster config show`
already prints `max_tokens` (`main.rs:1485`); it should print this too, since `status` exists to
report what a run would actually use. If it should instead be derived from the model id, derive it —
but then `context_budget` should not be a public field pretending otherwise.

**How to prove it.** The README/flag test (`botroster-cli/tests/readme.rs`) plus an assertion that a
`--context-budget` value reaches `AgentConfig`. The sharper test: the `config` suite already checks
that flags override the file for `max_tokens` (`config.rs:465-478`) — the same test for this field
fails to compile today.

---

### F-AM8 — A drained inbox is destroyed before the work that consumes it
`P1` · `reach: some users` · `crates/botroster-cli/src/main.rs:2520-2523`, `crates/botroster-bots/src/lib.rs:1305-1343`

**What is true now.** The run path calls `recover_inbox`, then `history`, then
`drain_inbox` (`main.rs:2521-2523`) — before the model has been contacted. `drain_inbox` renames
`inbox.jsonl` to `.taken`, reads it, and deletes it. The handoffs then exist only as a string
prefixed onto the task (`handoff_preamble`, `main.rs:2530-2538`). `recover_inbox` covers a crash
between the rename and the read — a window of microseconds, and covered with real care, including
the re-entrancy argument. The window that matters is the one after: from the drain until
`bots.append(&b.id, fresh)` at `main.rs:2604`, which spans the entire run.

**Why it matters.** Two distinct losses. If the process dies in that window — SIGKILL, a closed
laptop, or the second Ctrl-C at `main.rs:2582`, which the CLI itself offers as "quit without
saving" — the handoffs are gone from the inbox and were never written anywhere else. SPEC §5's
guarantee that a *"receiving Bot wakes, handles, may reply later"* is silently broken, and
`send` refuses a handoff to a non-existent Bot specifically because *"a handoff into the void is
silent data loss"*. Second, and more common: if the run merely *fails* — a rate limit, a step
limit, F-AM1's hang — the handoff text survives inside the appended user message but is no longer
in the inbox, so it is buried as an old conversation line rather than presented as pending work.
Nobody is told the work was never done.

**The durable fix.** The drain must not be an unwitnessed transfer of custody. Take the inbox but
leave the `.taken` file in place until the run has been appended, and delete it only then — the
existing `recover_inbox` merge already handles the restore correctly and is documented as
idempotent, so recovery on the next run costs nothing. Deleting `.taken` at `lib.rs:1341` becomes
a commit step the caller performs, not something the read does on its own. Alternatively give each
handoff a `handled` marker so an unfinished run re-presents it; either way, the property to encode
is that a handoff leaves the inbox only once the run that consumed it is durable.

**How to prove it.** A test that drains an inbox, abandons the run without appending (drop the
outcome), reopens the store, and asserts the handoff is still pending. Today it is gone. The
existing `an_interrupted_drain_is_recovered_rather_than_lost` (`lib.rs:1513`) covers the narrow
window and passes; this one covers the wide one and does not.

---

### F-AM9 — A group thread carries no speaker, so a Bot is seeded with another Bot's replies and tool calls as its own
`P1` · `reach: some users` · `crates/botroster-bots/src/lib.rs:1786-1800`, `crates/botroster-cli/src/main.rs:2847-2848`, `crates/botroster-cli/src/main.rs:2891`

**What is true now.** `append_group` serialises raw `Message` values into the shared log. A `Message`
is `{role, content}` — there is no author field anywhere in `model.rs:96-100`. `group post` picks one
owner via `owner_for` (`lib.rs:1636`), runs `run_task` with `Thread::Group`, seeds from
`group_history` and appends the whole transcript back. So the next post, answered by a *different*
member, loads prior turns in which another Bot's answers appear as `Role::Assistant` and another
Bot's tool calls appear as this Bot's own `tool_use`/`tool_result` pairs. The only attribution
anywhere is the `@mention` inside the human's post text, which names who was *asked*, never who
*answered*.

**Why it matters.** SPEC §5 says the reason to put Bots in a group is that *"the handoff is visible
in one conversation"*. It is visible to a person reading `group log`; it is invisible to the model,
which is the participant that has to act on it. A Bot loaded with a colleague's transcript believes
it ran those commands and reached those conclusions — so it will not re-verify them, will answer
follow-ups about work it never did, and cannot disagree with a peer because it cannot see that a
peer spoke. That dissolves the persona separation the description/message split (`lib.rs:4-11`)
exists to protect: each Bot has its own standing brief and then a shared memory of everyone's
actions. It also compounds F-AM2, since a group window can split a pair belonging to a Bot that is
not even the one running.

**The durable fix.** A shared transcript needs a speaker. Add an optional author to `Message` (or a
`Content::Text` wrapper the group path uses) that is `None` for a single-Bot conversation and the
`BotId` for a group one, defaulted on deserialise so existing logs keep loading. Render it into the
prompt at the provider edge the way `handoff_preamble` (`lib.rs:1375`) already renders a sender —
"**analyst** said: …" — and prefix another member's tool activity so it reads as reported history
rather than as this Bot's own. The shape already exists in `Handoff.from`; the group thread is the
one place it was not carried through.

**How to prove it.** A group test with two members and a scripted provider that records what it was
sent: post twice, mentioning a different member each time, and assert the second member's request
names the first member as the author of the earlier reply. Today the two are indistinguishable in
the request body.

---

### F-AM10 — An empty assistant turn is reported as a completed run carrying the previous run's answer
`P1` · `reach: some users` · `crates/botroster-agent/src/agent.rs:816-821`, `crates/botroster-agent/src/providers/http.rs:455`

**What is true now.** `finish` scans the *whole* message vector backwards for the last assistant
message with non-empty text — and that vector begins with the seeded history (`agent.rs:504`). So
whenever this run produced no text, `AgentOutcome.text` is an answer from a previous run, on a
previous day, presented as this run's result. The trigger is not exotic: `openai::response` reads
the answer with `msg["content"].as_str()` (`http.rs:455`), which yields `None` for any
OpenAI-compatible endpoint that returns content as an array of parts, or that puts the answer in
`reasoning_content` and leaves `content` null. `content` is then empty, `finish_reason` is `"stop"`,
the loop returns `Completed`, and `succeeded()` is `true`.

**Why it matters.** This is a false success, not a cosmetic one. `botroster run` prints a stale
answer with no indication it is stale. A routine records `Run { ok: true, summary: <last week's
answer> }` (`main.rs:1420-1424`) and `last_run` advances, so the day's work is marked done and never
retried. Someone reading the run history sees a healthy routine. The failure mode is silent by
construction, and `--base-url` at a gateway or a local server is a first-class supported setup —
the very configuration the usage-reporting comment (`http.rs:325-329`) singles out as the one that
leaves fields out or sends them in a different shape.

**The durable fix.** Two independent gaps, both worth closing. `finish` must only ever report text
this run produced: pass it the `history_len` boundary and scan no further back, returning empty when
this run said nothing — the same boundary `main.rs` already uses to decide what to persist. And a
turn with no content and no tool calls is not a completed turn: `EndTurn` with an empty content
vector should be its own finish reason ("the provider returned an empty answer"), matching how
`stop_reason=tool_use` with no tool block is already refused loudly at `agent.rs:654-671`. That
symmetry is the point — the loop already knows a provider contradicting itself is not a success.
Separately, `openai::response` should read an array-shaped `content` as well as a string one, since
the alternative is a whole class of endpoints answering silently.

**How to prove it.** In `vendor.rs`, serve a response whose `message.content` is
`[{"type":"text","text":"hello"}]` and assert the text is extracted. Then serve one with
`content: null, finish_reason: "stop"` to an agent seeded with history containing a prior assistant
answer, and assert the outcome does not succeed and its text is empty — today it succeeds carrying
the seeded answer. Note that `ScriptBuilder` cannot express this turn: every one of its methods
emits non-empty content (`scripted.rs:133-140`), so the cheap fixture has no way to produce an empty
answer and the loop's response to one has never been exercised. The test needs `Scripted::new` with
a hand-built `TurnResponse`, the same escape hatch `two_tool_calls_sharing_an_id_end_the_turn`
already uses.

---

### F-AM11 — Unparseable tool arguments become `{}` and are dispatched, and the model never learns why
`P2` · `reach: some users` · `crates/botroster-agent/src/providers/http.rs:465`

**What is true now.** `serde_json::from_str(raw).unwrap_or_else(|_| json!({}))`. A model that emits
invalid JSON in `function.arguments` has its call silently rewritten to an empty object, and the
loop then executes it: `drive` has no idea a parse failed, so `call_tool` goes out with `{}`.

**Why it matters.** Every destructive guest tool has required parameters
(`fs.write` → `["path","contents"]`, `shell.exec` → `["command"]`, `guest/src/tools.rs:406,418`), so
the realistic harm is not a stray write — it is that the model is told the wrong thing. It sees
"missing required field `path`" and concludes it forgot an argument, when in fact its arguments were
present and malformed. It then re-emits the same broken JSON, gets the same misleading error, and
burns steps against a 24-step budget on a mistake it cannot see. Small and local models are where
malformed tool arguments actually happen, and supporting them is the product's stated thesis. The
existing guard, `unparseable_tool_arguments_do_not_take_the_run_down` (`vendor.rs:578`), exercises
`model.turn` in isolation and never reaches the loop, so the dispatch is untested.

**The durable fix.** Represent the failure instead of erasing it. The parse error belongs in the
`Content::ToolUse` (an `input: Result<Value, String>`, or a sibling variant the loop matches on) so
`drive` can skip the hub entirely and write a `ToolResult { is_error: true }` saying the arguments
were not valid JSON, with the offending fragment. That is the same path a failed tool already takes,
so it costs the loop nothing new, and it turns an unfixable confusion into a correctable one. The
Anthropic dialect needs the same treatment for a non-object `input`.

**How to prove it.** An `agent_loop.rs` test with a scripted OpenAI-shaped reply carrying
`"arguments": "{not json"`: assert the hub received no call, and that the tool result fed back names
JSON as the problem. Today the hub is called with `{}`.

---

### F-AM12 — One dialect covers every OpenAI-shaped endpoint, with nowhere to vary a parameter per endpoint
`P2` · `reach: some users` · `crates/botroster-agent/src/providers/http.rs:24-27`, `crates/botroster-agent/src/providers/http.rs:428`

**What is true now.** `Dialect` is a two-value enum, and `Dialect::OpenAiChat` is documented as
covering *"OpenAI, xAI, Groq, Together, Ollama, vLLM, and anything else that ships an
OpenAI-compatible endpoint"* (`http.rs:6-8`). The request body is one fixed shape for all of them,
including `"max_tokens": max_tokens` (`http.rs:428`) and `"stream": false`. There is no per-endpoint
variation point: `HttpModelConfig` carries a dialect, a base URL, a key, a model id and a token cap,
and the body builder is a free function with no room to differ.

**Why it matters.** "OpenAI-compatible" is a family, not a protocol. Members of it disagree about
the output-limit parameter's *name*, about whether `content: null` is acceptable on an assistant
message, about whether a `system` role exists, and about whether `parallel_tool_calls` is
permitted — and the disagreements are hard 400s, which `http.rs:142` classifies as `Rejected`, i.e.
permanent, so the run dies immediately with a vendor message. The structural problem is not any one
of those (they change release to release and I cannot verify which apply to the shipped
configuration from here); it is that when one does apply, there is no place to express it short of
editing `openai::request` and changing behaviour for every other endpoint at once. That is exactly
the leak the crate's own doc comment claims not to have: *"translation happens entirely at this
edge"* is true, but the edge has one shape for a dozen servers.

**The durable fix.** Keep two dialects, but give the OpenAI one a small quirks record on
`HttpModelConfig` — the name of the output-limit field, whether an empty assistant `content` may be
null, whether a `system` role is supported — defaulted to the OpenAI spelling and overridable from
`ModelSettings`. That is a data change, not a code fork, so adding an endpoint is a config entry
rather than a patch to the shared builder. The `stream: false` comment (`http.rs:430-440`) is the
precedent: a whole class of gateway failures removed by one field, discovered by measuring a real
one.

**How to prove it.** A `vendor.rs` test that sets the quirk and asserts the outgoing body names the
configured field rather than `max_tokens`, alongside the existing default-shape assertion in
`an_openai_request_asks_for_a_whole_answer_not_a_stream` (`vendor.rs:536`).

---

## What I could not check

- **Nothing was run against a real provider.** Every dialect claim is read off `http.rs` and the
  fake vendor in `vendor.rs`. In particular I cannot confirm which live endpoints reject
  `max_tokens`, return array-shaped `content`, or 400 on an orphaned `tool_result` — F-AM2 and
  F-AM10 are reasoned from the wire shape the code emits and from the project's own statements
  (`agent.rs:56-61`, `agent_loop.rs:616-622`) that a broken pairing is a 400 from every vendor.
- **I did not run the test suite**, per the brief; `cargo check` was not needed since no finding
  turns on compilation. Claims about what tests do come from reading them.
- **The hub-side relay leak in F-AM1** I read but did not exercise: `botrosterd` is another
  department's scope, and I verified only that `disconnect` reaps by `origin_conn` and that no timer
  touches `relays`. If some path I did not find synthesises a failure for an orphaned relay, F-AM1's
  trigger narrows to a wedged-but-connected guest — the harness-side gap (no timeout, no cancel
  around `call_tool`) is unaffected either way.
- **`schedule.rs`** I read only in outline. The retry/backoff constants and `is_due`/`missed`
  semantics I did read, but they live in `bots/src/lib.rs:2231-2400`, not in `schedule.rs`; the cron
  parser and its DST handling I did not audit. It is timekeeping rather than agent behaviour, and
  its own test list looks unusually thorough for what it is.
- **`approvals.rs`, `refused.rs`, `secrets.rs`** I read as test names and doc headers rather than
  assertion by assertion. They cover the approval gate, handshake refusals and credential
  containment — all of which sit at boundaries owned by other departments — and nothing in their
  outlines contradicts or adds to a finding here.
- **The desktop client and ACP adapter** consume `AgentEvent` and `FinishReason` and are out of
  scope. I checked `acp/mod.rs` only far enough to be fair to the existing `HubFailed` decision in
  F-AM5.
- **Frequency claims are estimates.** F-AM2's "frequently" comes from the shape of a
  tool-using log against a 40-message window, not from measured user data; a Bot that mostly talks
  rather than mostly calls tools will hit it far less often. F-AM3's threshold
  (~15 full-size results) is arithmetic on `RESULT_CHAR_LIMIT` and `CONVERSATION_CHAR_BUDGET`, and
  real runs will reach it slower or faster depending on how large tool outputs actually are.
