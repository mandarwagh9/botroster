# Design & Desktop Client — review

**Checked against BACKLOG B01–B19:** read in full; no finding below re-reports one. Four are
adjacent and deliberately distinct, and the distinction is stated inside each finding:

| Mine | Adjacent backlog item | The distinction |
|---|---|---|
| F-DC1 (the whole token layer is not DIRECTION's) | B06 (two coats share a hue) | B06 files one deviation inside the shipped palette. F-DC1 is that the shipped palette is not the pinned palette at all, and that this — LOOP.md's launch decision #1 — has no backlog row. |
| F-DC3 (per-step duration is measured and discarded) | B14 (an abandoned step says it may or may not have run) | B14 designs the *unknown* outcome. F-DC3 is that the *known* outcome never carries a time, on the one surface in the product that does not show one. |
| F-DC6 / F-DC14 (status is never announced; Stop failure says "search failed") | B15 (`setStatus(String(err))` in nine places — `done`) | B15 swept nine sites and gave each a short WHAT. It did not make the pill audible, and one of the nine got the wrong WHAT. |
| F-DC8 (nothing detects the runtime dying) | B17 (the "No computer" banner names the transport) | B17 is the copy on a banner that does appear. F-DC8 is the case where no banner appears at all, because nothing is watching. |

Also checked and deliberately not re-raised: B01 roster status, B02/B08 the approval dialog's
emphasis and its modality, B03 the computer behind a button, B04 the waiting-on-you count, B05 the
empty pane, B07 the erroring routine, B09 Escape on approvals, B10 the transport pill's prominence,
B19 the connect actions below the fold, and the J8 "return after two days" gap recorded under B13.

**Verdict:** No — not yet, and the reason is not craft. Sentence for sentence this is the best-written
desktop client I have reviewed: the approval dialog, the deletion cost, the abandoned step and the
four-part connect error are better than what most funded teams ship, and the code says *why* next to
every one of them. What is missing is everything that happens in the fourth dimension — the client
was designed as a sequence of still frames, and it shows the moment a Bot works for longer than a
screenshot: the log yanks you to the bottom while you are reading, the model call is a silent void
with no elapsed time, eight of eleven surfaces go from blank to populated with nothing in between,
and a runtime that dies leaves the word "connected" in the corner indefinitely. The mental model is
legible where it matters most — a Bot is a teammate with one job, a thread is its memory, and both
land in one sentence on the empty pane — but the product's own differentiator is taught backwards:
the window says each Bot has "a computer it works on" where the architecture, and an explicit
instruction in the spec, say they all share one. On top of that the pinned design system in
`DIRECTION.md`, adopted as launch decision #1, was never implemented, so the build ships a purple
accent DIRECTION bans and a pill radius it forbids. Fix time, liveness and the token layer and this
is genuinely competitive; leave them and it reads as a beautiful prototype.

---

## State coverage matrix

`DESIGNED` — somebody decided what this looks like. `DEFAULT` — the browser or a leftover state
decides. `UNDESIGNED` — the state is reachable and nothing renders for it. `—` — not reachable.

| Surface | empty | loading | partial | error | offline / dead runtime | permission-denied | too-much-content | mid-stream / mid-run |
|---|---|---|---|---|---|---|---|---|
| Connect panel | DESIGNED (fields prefilled, `main.js:2605-2621`) | DEFAULT (`connectBtn.disabled`, `main.js:1251`, and nothing else while a subprocess spawns and handshakes) | — | **DESIGNED** (four-part, `index.html:139-147`) | — | — | PARTIAL (scrolls; B19) | — |
| Roster | DESIGNED (`index.html:173`) | **UNDESIGNED** (`main.js:730-741`) | **UNDESIGNED** (`refreshGroups` swallows a failure with `catch { return; }`, `main.js:623-629`, and is called unawaited at `:734` — half the sidebar vanishes silently) | DEFAULT (raw `String(err)`, `main.js:738`) | **UNDESIGNED** (the last good answer keeps rendering) | — | DEFAULT (`overflow-y:auto`) | **UNDESIGNED** (B01) |
| Run log | **UNDESIGNED** (a Bot with no history gets an empty box, `main.js:760`) | DEFAULT (`setStatus("opening…")`) | **DESIGNED** (B14, `main.js:434-449`) | DESIGNED (`#problem`, `index.html:231`) | **UNDESIGNED** | **DESIGNED** — a hub `deny` lands as a refusal result row and reads as final (MEMORY reachability note; J7 scores it the one passing failure state) | DEFAULT (unbounded append; no jump-to-latest, no turn boundaries) | PARTIAL — step rows are excellent (rubric 5), but no duration, no elapsed, and autoscroll fights the reader (F-DC7) |
| Composer | DESIGNED (hidden with no session) | — | DESIGNED (a message joining a turn is not echoed until it lands, `main.js:1341`) | DESIGNED (text restored, `main.js:1360`) | **UNDESIGNED** (stays live over a dead runtime) | — | DEFAULT (`rows="2"`, no growth) | DESIGNED (Stop appears; joining a turn is a real state) |
| Approval dialog | — | — | DESIGNED (queue count, `main.js:1040`) | DESIGNED (`ask.dead`, `main.js:1047`) | DESIGNED (`refuseAsks`) | DESIGNED (invariant 5; `refuses()` decided in Rust, `an_unrecognised_option_kind_is_still_styled_as_a_refusal`) | DESIGNED (`.long` scrolls rather than truncating, `main.js:1036`) | DESIGNED |
| Credential prompt | — | — | DESIGNED ("nothing was entered" is parked, not settled, `main.js:835`) | DESIGNED (`main.js:841`) | DESIGNED | DESIGNED (declining sends no value) | — | — |
| Agent Computer | DESIGNED (hub viewer paints it) | **UNDESIGNED** (empty panel while `botroster watch` spawns, `main.js:2063-2067` → `viewer.rs:72-130`) | DESIGNED (3s poll blanks a frozen frame, `main.js:2083-2101`) | DESIGNED (`computerError`) | DESIGNED (same poll) | **UNDESIGNED** — the takeover lock is hub-enforced with no window-side surface | DEFAULT (iframe) | **UNDESIGNED** (no takeover state) |
| Command palette | DESIGNED (`Nothing matches.`) | **UNDESIGNED** (message hits land ~150-500ms after the names, `main.js:2444-2461`) | **UNDESIGNED** (names then hits appended silently, `main.js:2467-2475`) | DESIGNED (`main.js:2484`) | — | — | **UNDESIGNED** (`.slice(0,12)`/`.slice(0,8)`, no "N more") | — |
| Settings | DESIGNED ×3 (`index.html:388,408,415`) | **UNDESIGNED** (three lists paint blank then fill, `main.js:1957-1959`) | **UNDESIGNED** (one try block: a failed `connectors` read skips `routines` entirely, `main.js:1905-1950`) | DESIGNED-ish (shared line cleared per attempt, `main.js:1831`) | **UNDESIGNED** | — | DEFAULT | **UNDESIGNED** (erroring routine — B07) |
| Credentials store | DESIGNED (`index.html:440`) | **UNDESIGNED** | — | DEFAULT (raw string, `main.js:1998`) | — | — | DEFAULT | — |
| Edit Bot | — | **UNDESIGNED** (click does nothing until `roster` returns, `main.js:1585-1626`) | DESIGNED (a group has no profile and the dialog says so, `main.js:1595-1602`) | DEFAULT (raw string, `main.js:1593`) | — | — | DEFAULT | — |
| Deletion confirm | — | **DESIGNED** — "Working out what that would remove…" (`main.js:1786`) | DESIGNED (a count that cannot be read is omitted, never reported as zero, `main.js:1764-1767`) | DESIGNED | — | — | — | — |

Two columns carry the story. **`loading`** is UNDESIGNED in eight of eleven surfaces, and the single
exception — the deletion confirm — proves the pattern was available and simply never generalised
(F-DC5). **`partial`** splits cleanly down the middle: every surface where a person is about to make
a decision is scrupulous about not over-claiming, and every surface that merely *lists* things fails
silently (F-DC5, roster and Settings rows).

---

## Findings

### F-DC1 — the pinned design system in DIRECTION.md was never implemented, and no backlog row says so
`P0` · `reach: all users` · `crates/botroster-app/ui/styles.css:29-108`

**What is true now.** `LOOP.md:12-19` records launch decision #1: *"Full re-skin, as written.
DIRECTION.md's eight pinned colours and three typefaces replace the shipped token layer. The cost was
flagged and accepted."* `DIRECTION.md` says of its eight colours: *"pinned by hand and are not to be
changed by the loop."* None of it exists in the stylesheet. Not partially — at all:

| DIRECTION says | `styles.css` ships |
|---|---|
| `--base --raised --line --text --muted --waiting --live --refused` (pinned) | none of these tokens exist; `--bg --panel --raise --ink --danger --ok --warn` instead (`:33-60`) |
| accent is a **neutral fill**, and "purple-to-blue anything" is in **Banned** | `--accent: light-dark(#6f45e0, #7c4ff0)` — purple (`:49`) |
| "**there are no pills**"; `--r-1 3px --r-2 6px --r-3 10px --r-0 0` | `--r-pill: 999px` (`:67`), used 6× including every `button` (`:152`); the file's own header states the opposite rule at `:20` |
| Geist Sans / Commit Mono / Martian Mono, "vendored as WOFF2 under `crates/botroster-app/ui/fonts/`" | `ui-sans-serif, system-ui…` (`:75-76`); **`ui/fonts/` does not exist** |
| eight low-chroma coats, none within 20° of a status hue | `#946cf6` purple, `#ec5358` red (~11° from `--refused` `#E5674E`), `#eb4699` magenta, `#3c82f6` blue (`:92-107`) |
| `--shadow: 0 16px 40px` | `0 20px 50px` (`:63`) |

Four loop iterations ran (MEMORY 001–004) and every one edited inside the old vocabulary —
`--coat-4`, `--warn`, `--ghost`. The re-skin never started, and `BACKLOG.md`, the file that orders
design work, has no row for it. B06 files two coats sharing a hue, which is a symptom of the whole
palette being the wrong palette.

**Why it matters.** This is the largest single design decision in the project and it is invisible to
the system that tracks design work. Every future finding, including several below, has to be resolved
twice: once against the shipped tokens and again after the re-skin. The immediate user-visible cost
is that the product's argument — *colour in this app means status and never emphasis*, DIRECTION's
stated load-bearing derivation — is not true of the shipped build: a purple Send button and a purple
Bot coat are two hues on screen that encode nothing, which is exactly the condition rubric line 18
exists to catch, one level up from where it looks.

**The durable fix.** Do the re-skin as one commit, in this order, because the order is what keeps it
provable: (1) add the eight pinned tokens and the DERIVED set to `:root` as *aliases* alongside the
existing names; (2) rewrite the ~40 call sites to the new names and delete the old; (3) replace
`--r-pill` with `--r-1/2/3/0` — the change that alters the most pixels, and it should not be folded
into the colour commit; (4) vendor the three WOFF2 faces under `ui/fonts/` with a `PROVENANCE.md` row
each, as DIRECTION already specifies, and `@font-face` them with the DERIVED fallback stacks. No npm,
no build step, no framework — this is CSS custom properties and four font files. And **file it as a
backlog row first**, so the next reader of `BACKLOG.md` can see the project's largest design debt
without reading `LOOP.md`.

**How to prove it.** `crates/botroster-app/tests/page.rs` already reads the stylesheet from disk for
`every_coat_a_bot_can_wear_is_legible` and `no_text_in_any_surface_falls_below_the_contrast_it_needs`.
Add `the_shipped_tokens_are_the_pinned_tokens`: parse the fenced `Pinned` block out of
`.claude/ux-loop/DIRECTION.md`, resolve each name against `getComputedStyle(document.documentElement)`
in the live page, assert every pinned value matches. It fails today on all eight and cannot be
satisfied by editing DIRECTION alone, because DIRECTION's own rule is that a pinned token may not be
revised. Pair with `no_control_in_this_window_is_a_pill`, asserting no computed `border-radius`
exceeds `--r-3`.

---

### F-DC2 — the window teaches that each Bot has its own computer; the spec orders it to say the opposite, loudly
`P1` · `reach: all users` · `crates/botroster-app/ui/index.html:258`

**What is true now.** The empty-conversation card, the first sentence a new install reads about what
a Bot is, says: *"Each Bot has one job, its own memory, **and a computer it works on**."* The
architecture is one shared computer with a browser context per Bot — `CLAUDE.md:3` ("teammates
**sharing one durable computer**"), `docs/SPEC.md:232-234` ("one browser process, N logical
contexts… Each Bot gets a context"), and `README.md:51` ("a hub and **a** computer"). The Agent
Computer button takes no Bot argument (`main.js:2067`, `invoke("open_computer")`).

`docs/SPEC.md:347-349` does not treat this as a naming preference. It is an instruction to this
surface:

> **The unavoidable caveat, stated loudly in the UI:** browser sessions and shell credentials on the
> shared computer *are* accessible to every Bot on that account. Separate Bots are **not** a security
> boundary. Upstream says this; `botroster` must say it louder, because users will assume otherwise.

Grepping `index.html` and `main.js` for that caveat returns nothing. The UI does not merely omit it —
the one sentence it spends on the subject teaches the misconception the spec names.

**Why it matters.** This is the product's differentiator and its sharpest security assumption in one
sentence. Somebody who reads "a computer it works on" will put a signed-in bank session in front of
one Bot and a Bot built from an untrusted brief beside it, believing the second cannot reach the
first. It can. The spec anticipated exactly this user and told the UI to prevent it; the UI does the
opposite. It is also the load-bearing IA fact of the whole product — Bots are *people sharing a
machine*, not sandboxes — so the roster, the computer pane and the approval gate are all read through
the wrong model.

**The durable fix.** Two sentences, in the two places the mental model is formed, both within the
existing markup and tokens. (1) Rewrite `index.html:258`: "Each Bot has one job and its own memory.
They all work on **the same computer** — logins and files on it are shared." (2) Put the caveat in
the Agent Computer pane header (`index.html:270-273`), which already carries the takeover sentence
and is the moment somebody is looking at the shared thing: "Every Bot uses this computer. A login
here is a login for all of them." No new colour, no new component. Not a dismissible banner — a fact
about what the product *is* does not have a dismissed state.

**How to prove it.** `crates/botroster-app/tests/defaults.rs` already asserts properties of shipped
markup by reading `index.html`. Add `the_window_says_the_computer_is_shared`: assert the empty-state
card and the computer pane header each contain a sentence naming the computer as shared, and that
`index.html` contains no sentence matching `/a computer it works on|its own computer/`. It fails on
the second clause today; revert the copy and it notices.

---

### F-DC3 — every tool step's duration is measured, the CLI prints it, and the wire to the window throws it away
`P1` · `reach: all users` · `crates/botroster-cli/src/acp/serve.rs:469-490`

**What is true now.** `crates/botroster-agent/src/agent.rs:730-732` times every tool call:

```rust
let started = Instant::now();
let outcome = self.hub.call_tool(&name, &call_id, args).await;
let elapsed_ms = started.elapsed().as_millis() as u64;
```

It reaches `AgentEvent::ToolCallFinished { elapsed_ms, .. }` (`agent.rs:135, 792`). The terminal
renders it in a dimmed time column (`crates/botroster-cli/src/render.rs:33-42`) and the HTML export
renders it (`crates/botroster-cli/src/html.rs:79`). The ACP adapter destructures the event as
`ToolCallFinished { call_id, ok, output, .. }` and the `..` drops it (`serve.rs:469-474`);
`ToolCallUpdateFields` is built with `status` and `raw_output` only. `Chunk`
(`crates/botroster-app/src/lib.rs:233-234`) has no duration field, and `render()` at `lib.rs:1599-1608`
also discards `update.tool_call_id`, so the page has no per-call handle to hang one on.

The consequence: **no time is displayed anywhere in the window.** Not a step duration, not a message
timestamp, not turn-elapsed. `DIRECTION.md` specifies the row as "verb, target, result, **duration**"
and reserves `--faint` for "timestamps, durations, row metadata" — a token that exists in
`styles.css:43` for a purpose the app never uses.

**Why it matters.** The desktop client is the only surface in the product that cannot answer "is this
slow?". Somebody watching `shell.exec cargo test` cannot distinguish a build that is running from one
that is wedged, and afterwards a 40-step log gives no way to find the step that cost four minutes.
The terminal user gets this and the window user does not, which inverts the usual relationship
between a GUI and its CLI and is the clearest single place the client reads as a prototype beside a
finished tool.

**The durable fix.** Carry it on ACP's own extension point, which this repo already uses and tests:
`crates/botroster-cli/src/acp/mod.rs:268` documents "the key BOTROSTER claims inside ACP's `_meta`", and
`ToolCallUpdate` has `meta: Option<Meta>`
(`agent-client-protocol-schema-1.6.0/src/v1/tool_call.rs:234`). `ToolCallUpdateFields` has no
duration field and `raw_output` belongs to the tool, so `_meta` is the correct carrier rather than a
convenient one. Then: add `elapsed_ms` and `call_id` to `Chunk`; `render()` reads both;
`completeStep` (`main.js:451`) appends a `.step-time` span in `--faint`/`--micro` on the right of the
row where `--r-0` tiles. Forward `call_id` in the same change even though the loop is sequential
today (`agent.rs:720`, with a duplicate-id guard above it) — without it, `openStep` stays the only
correlation the page has, and a duration attached to a positional guess is worse than none.

**How to prove it.** Two tests, at the two seams. In `serve.rs`'s own unit tests — which already
build `ToolCallFinished { elapsed_ms: 3, … }` at `:1199, :1335, :1450` — assert the produced
`SessionUpdate` carries the 3ms; it currently cannot, which is why those three fixtures set a value
nothing reads. In `page.rs`, extend `a_tool_call_and_its_result_are_one_row` (`:3944`) or add
`a_finished_step_says_how_long_it_took`: fire a tool chunk and a result chunk carrying a duration,
assert the row renders it. Restore the `..` and both fail.

---

### F-DC4 — the waiting state has no design: a model call is a silent void with one 12px word in the far corner
`P1` · `reach: all users` · `crates/botroster-app/ui/main.js:1334`

**What is true now.** `sendPrompt` sets `setStatus("thinking…", "busy")` and that is the entire design
of the state. The model call does not stream and should not:
`crates/botroster-agent/src/providers/http.rs:440` sends `"stream": false` deliberately, and MEMORY run
3 records why — a gateway that defaulted the other way surfaced as `Malformed`, and "one field
removes the whole class". `serve.rs:491` also drops `AgentEvent::Thinking` entirely, so the window is
told nothing between the last tool result and the answer. In that interval the window shows: the
person's own message, a pulsing 6px dot, and the word "thinking…" in a 12px pill in the top-right
corner — roughly 900px from the 640px measure where the eye actually is
(`docs/botroster-approval.png` shows the pill at that distance, mid-run). No elapsed time, no phase, no
indication whether this is a two-second call or a two-minute one.

**Why it matters.** This is the majority of the wall-clock time a person spends with a working Bot,
and the interval that decides whether the product feels alive or hung. Every frontier desktop AI
client answers "it is still going" inside the reading column; this one answers it in a corner, in the
smallest type on the screen, with the least specific word available. It is also the state in which a
person decides whether to press Stop — and the control they would press is at the opposite corner
from the only evidence they have.

**The durable fix.** Give the run log a pending row, in the log, at the point in the sequence where
the work is happening — which is where DIRECTION already puts everything else ("inline gates at the
point in the log where they happened"). A `.msg pending` row on the `--live` hairline carrying the
phase and a ticking elapsed counter in `--faint`/`--micro`, appended when a turn starts and replaced
in place by the agent's prose when it arrives. That is the same in-place-completion mechanism
`completeStep` already implements for tool rows (`main.js:451`), applied one level up, so it costs no
new pattern. The motion budget is not exceeded: DIRECTION allows state transitions, and a counter
changing is a state changing. Not a spinner — a spinner asserts progress it has not measured, the
mistake `abandonOpenStep` was written to avoid.

**How to prove it.** In `page.rs`, `a_running_turn_says_so_where_the_person_is_reading`: stub `prompt`
to hang, submit the composer, assert `#log` gains a row whose bounding box is inside the log's
viewport and whose text changes at least once within two seconds; resolve `prompt` and assert the row
is gone. Today the log gains nothing at all between submit and the first chunk, so it fails on the
first assertion.

---

### F-DC5 — no asynchronous surface has a loading state, and a half-failed read is indistinguishable from an empty one
`P1` · `reach: all users` · `crates/botroster-app/ui/main.js:1905-1950, 1585-1626, 623-629`

**What is true now.** Eight of eleven surfaces go straight from blank to populated, and two of them
fail silently on the way.

- **Settings** opens immediately and then calls `refreshRules()` and `refreshWiring()` unawaited
  (`main.js:1957-1959`), so three `<dl>`s paint blank first. Worse: `refreshWiring` awaits
  `connectors` and then `routines` **inside a single `try`** (`main.js:1905-1950`), with each `await`
  sitting in `draw`'s argument list. A failing connector read therefore means `routines` is never
  fetched at all, and because `show(emptyEl, …)` only runs inside `draw`, **both sections render as
  nothing**: no list, no empty state, no indication they were never read. Only `refreshRules`, which
  has its own try/catch, survives.
- **The roster** has the same fault one level out. `refreshGroups` swallows its failure with a bare
  `catch { return; }` (`main.js:623-629`) and is called unawaited from `refreshRoster` (`:734`), after
  `show(rosterError, false)`. A failed `groups` read leaves the Bots listed and the Groups section
  silently absent — while `refreshRoster`'s own comment goes to explicit trouble never to do this:
  *"Never an empty list on failure: that is indistinguishable from having no Bots, and the person
  would go looking for their work, not the error."*
- **Edit** awaits `invoke("roster", {hidden:true})` *before* showing the dialog (`main.js:1585-1626`).
  Click it and nothing happens at all — no dialog, no pressed state — until a subprocess answers.
- **Agent Computer** shows the panel, then awaits `open_computer` (`main.js:2063-2067`), which spawns
  an `botroster watch` subprocess and waits for it to announce a port (`viewer.rs:72-130`). The panel is
  an empty black rectangle for the duration.

The exception: `$("del-start")` writes "Working out what that would remove…" before awaiting
`deletionCost()` (`main.js:1786-1789`). One place, done properly, never generalised. And
`refreshMentionable` (`main.js:2242-2247`) already argues the correct rule in a comment —
*"Independently: a home with a broken connector should still offer skills, rather than one failing
list leaving the composer with nothing"* — so the argument for splitting the try exists, in a sibling
function, applied there and not here.

**Why it matters.** Every one of these calls shells out to an `botroster` subprocess; the code says so
at `main.js:2233-2236` and budgets a 3-second throttle around it. These are not sub-100ms calls. The
user-visible result is a window that appears to ignore clicks — and, in Settings and the roster,
something worse than latency: a person with three routines and a broken connector sees a Settings
panel with no routines and no error about routines, and concludes they have none. MEMORY 004's own
lesson is that the second instance of a fault is worth looking for immediately; this is the same
fault in two functions.

**The durable fix.** Two changes, both small.

1. **Split the try.** Each list gets its own `try`/`catch` and its own error line, so one failing read
   cannot take a sibling's data with it — the rule `refreshMentionable` already states. `refreshGroups`
   reports rather than returning.
2. **One three-state convention** for anything awaited: `pending` / `empty` / `filled`, where `empty`
   may only be shown after an answer has arrived. A `setListState(listEl, emptyEl, state)` helper
   beside `show()` toggles `aria-busy` on the list and swaps the empty paragraph for a `--faint`
   "Reading…" line; every `refresh*` calls it with `pending` before its `await`. For Edit, invert the
   order — show the dialog with pending fields, then fill.

One helper, one try-block split, ~8 call sites. No framework, no new tokens.

**How to prove it.** In `page.rs`, `a_failing_connector_read_does_not_hide_the_routines`: stub
`connectors` to reject and `routines` to resolve with one row, open Settings, assert the routine is
listed and that an error names connectors specifically. It fails today — the routine never arrives.
Pair with `an_empty_state_is_never_shown_before_the_answer_arrives`: stub `routines` to resolve after
a delay, assert `#routines-empty` is not laid out immediately and `#routines-list` reports
`aria-busy="true"`, then resolve and assert the terminal state.

---

### F-DC6 — the status pill is the app's only "what is happening now" channel and it is never announced
`P1` · `reach: all users` · `crates/botroster-app/ui/index.html:209`

**What is true now.** `<span id="status" class="status">connected</span>` — no `role`, no
`aria-live`. `setStatus` (`main.js:79-82`) writes to it from a dozen call sites: "opening…",
"thinking…", "redirecting…", "connected · 12 tools", "no computer", and every short WHAT that B15
introduced. A screen reader is told none of it. The page knows the technique and applies it three
times — `#connect-error` is `role="alert"` (`index.html:139`), `#no-computer` is `role="status"`
(`:215`), `#problem` is `role="alert"` (`:231`) — so the pill is the one live surface that was missed,
and it is the one that changes most often.

Second half of the same defect: the pill's `::before` dot, the part carrying state at a glance,
differs between `.status.connected`, `.status.busy` and `.status.error` by `color` and `background`
alone (`styles.css:815-841`). The pill's *text* does distinguish them, which saves it from being
colour-only overall — but the glance-level signal is.

**Why it matters.** For a screen reader user the run has no observable state: submitting a message
produces silence until the first chunk lands in `#log`, and a failed turn announces the `#problem`
banner's record without ever announcing that the run stopped. Beyond assistive tech this shares a root
with F-DC4 — the app's only progress channel is its least prominent element — and fixing the
announcement is most of the work of fixing the placement.

**The durable fix.** `role="status"` plus `aria-live="polite"` on `#status`; and because the pill is
also the wrong place visually, make `setStatus` the single function that updates *both* the pill and
F-DC4's in-log pending row, so one call site decides what the window is saying about itself and there
are two renderings of it. Give the dot a non-colour differentiator (`--r-0` square for error, ring for
busy, filled for connected) so the glance signal survives a monochrome display — shape is a token
axis under DIRECTION; a fourth hue is not.

**How to prove it.** `page.rs` already asserts ARIA properties
(`every_message_kind_says_who_it_is_from`, `every_modal_contains_the_keyboard_while_it_is_open`). Add
`every_surface_that_changes_on_its_own_announces_itself`: enumerate the elements `setStatus` and
`reportProblem` write to and assert each sits inside a live region. It fails on `#status` today, and
would catch the next such element.

---

### F-DC7 — the log scrolls to the bottom on every chunk, so you cannot read while a Bot is working
`P1` · `reach: most users` · `crates/botroster-app/ui/main.js:338, 407, 960`

**What is true now.** Three unconditional statements, one at each append path:

```js
log.scrollTop = log.scrollHeight;
```

No check for where the reader is. A step's completion (`:338`), every new row (`:407`) and every
auto-approval record (`:960`) yank the viewport to the bottom regardless of whether the person has
scrolled up. `page.rs:4154 the_start_of_a_long_thread_is_still_reachable` proves the log *can* be
scrolled to the top, but it fires all 41 chunks first and then scrolls — the one ordering in which
this defect cannot appear.

**Why it matters.** The product's whole pitch is a Bot working for a long time on a real computer
while you watch. The natural thing to do while watching is scroll up to re-read the step that just
went past — and the next chunk snatches the page back. On a run producing a chunk every second or two
the log is unreadable until the turn ends, which means the run log, the surface DIRECTION calls "most
of the app", is only usable retrospectively. It is worse on a keyboard: `#log` carries `tabindex="0"`
specifically so it can be paged (`index.html:242-247`), and PageUp is undone by the next event.

**The durable fix.** Stick to the bottom only when already at the bottom. Read the position *before*
appending — `const wasAtBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 24` — append,
then scroll only if it was true. One helper called from all three sites so they cannot drift. When it
was false and content arrived, show a "N new below" affordance pinned to the log's bottom edge
(`--r-1`, `--faint`, no new colour) that scrolls down and clears — which is also the "jump to latest"
the `too-much-content` column has no answer for, and the thing rubric 9 ("find the last approval you
granted in under three actions") needs to exist before it can be satisfied.

**How to prove it.** In `page.rs`, `reading_back_through_a_run_is_not_undone_by_the_next_step`: open a
session, fire 40 chunks, set `log.scrollTop = 0`, fire one more chunk, `settle()`, assert `scrollTop`
is still 0 and that the new-content affordance is laid out (using the existing helper from
`an_element_this_suite_calls_shown_is_one_the_browser_lays_out`). Then scroll to the bottom, fire
another chunk, assert it followed. It fails on the first assertion today.

---

### F-DC8 — nothing tells the window the runtime died; "connected" is an unverified claim that never expires
`P1` · `reach: most users` · `crates/botroster-app/src/lib.rs:1293, 1330, 1386`

**What is true now.** The shell emits exactly three events — `permission-withdrawn`,
`permission-request`, `chunk` — and none is a liveness signal. `Engine` holds a `JoinHandle` and
aborts it on `Drop` (`crates/botroster-desktop/src/engine.rs:903-905`), but nothing watches it. If
`botroster acp` is killed, crashes, or loses the hub between turns, the window keeps showing the roster
it last fetched, the pill keeps reading "connected", the composer stays live and Send stays primary.
The truth only surfaces when somebody sends a message and `prompt` throws, which becomes
`reportProblem(err, "the run stopped")` (`main.js:1359`) — a message about a run that never started.

The window already does this correctly one layer down: `watchComputer` polls `computer_alive` every 3
seconds while the computer pane is open and blanks the frame the moment it stops, with a comment
explaining why — *"An iframe onto a dead process keeps showing what it last painted, which looks
exactly like a computer sitting idle"* (`main.js:2075-2101`). The identical argument applies to the
whole window and was not made.

**Why it matters.** This is the app's optimistic-vs-confirmed failure. Every other state here is
scrupulous about not claiming more than it knows — `abandonOpenStep` refuses to say whether a command
ran, `answerAsk` keeps the question up until the answer lands, `deletionCost` omits a count it could
not read rather than reporting zero. The one place the window asserts something unchecked is the
single word it displays permanently in the chrome. Somebody leaves a Bot on a long routine, comes
back, sees "connected", and believes work happened. This is the strongest form of B17: that item fixes
the wording of a banner that appears; here no banner appears at all.

**The durable fix.** The shell owns the child, so the shell should say when it goes. Emit a
`disconnected` Tauri event where `Engine`'s task terminates, carrying the child's last stderr — which
`Engine::connect` already buffers via `with_debug` for B11's sake, so the evidence is in hand and
B11's mistake does not need making twice. `main.js` listens alongside the other three `listen(...)`
calls: short WHAT in the pill, the record behind the `#problem` disclosure, composer hidden, every
queued ask refused (`refuseAsks()` already does exactly this). Do not silently return to the connect
panel — the person's transcript is on screen and should stay there.

**How to prove it.** In `crates/botroster-desktop/tests/engine_live.rs`, which already drives a real
`botroster acp`: kill the child and assert the engine reports it rather than hanging. In `page.rs`,
`a_dead_runtime_does_not_keep_saying_connected`: fire the new event through the stub's `__fire`, then
assert the pill no longer reads `connected`, the composer is not laid out, and the problem banner is.
All three fail today because the event does not exist.

---

### F-DC9 — the roster is destroyed and rebuilt at the end of every turn, taking focus and scroll with it
`P1` · `reach: most users` · `crates/botroster-app/ui/main.js:565`

**What is true now.** `renderRoster` opens with `botsList.innerHTML = ""` and rebuilds every row from
scratch; `refreshGroups` does the same to `#groups` (`:631`). `refreshRoster()` is called from
`sendPrompt`'s `finally` (`:1367`) — so **every turn that ends re-creates the entire sidebar.** It is
also called from `openBot`, `openGroup`, the hidden toggle, and after every edit, hide, duplicate and
delete.

Two consequences. A keyboard user who has tabbed onto a roster button has that button removed from the
document; focus falls to `<body>` and the next Tab restarts from the top of the page. And the
sidebar's scroll position resets, so with more than a screenful of Bots the list jumps to the top
whenever any run finishes.

`applyModality` (`main.js:150-174`) shows the file understands this class of problem — it takes
trouble over `returnFocusTo` and checks `back.isConnected` "because it may have been re-rendered
away". The roster is the surface doing the re-rendering and has no such care.

**Why it matters.** DIRECTION's first consequence is that the roster is a status board, which means it
is the surface a person is looking at *while* things finish. Right now finishing is exactly what
resets it. On a keyboard this is the difference between navigable and not: a Bot working in the
background silently steals your place in the list, with no way to tell what happened.

**The durable fix.** Reconcile instead of replacing. Key rows by `bot.id` in a `Map`, update the text
of rows that exist, insert and remove only what changed, and reorder by moving nodes rather than
recreating them. ~30 lines of vanilla DOM, and it removes the whole class — it is also the
precondition for B01's roster status, because a status that flickers through a full rebuild every time
a turn ends will read as noise. Preserve `scrollTop` across the update as a second measure.

**How to prove it.** In `page.rs`, `a_finished_turn_does_not_take_the_keyboard_out_of_the_roster`: open
a session, focus a roster button, complete a turn, `settle()`, assert `document.activeElement` is still
that button and is the same node (stamp a property on it before and read it after). Both assertions
fail today.

---

### F-DC10 — the command palette is invisible to assistive tech, and neither menu wires `aria-activedescendant`
`P1` · `reach: some users` · `crates/botroster-app/ui/index.html:361-364`, `main.js:2161-2183`

**What is true now.** The palette is Ctrl/Cmd+K, described in the markup as the answer to "the most
frequent thing a person does here" (`index.html:355-357`). Its results are a bare `<ul>` of bare
`<li>`s: no `role="listbox"`, no `role="option"`, no `aria-selected`, no result count, and the input
carries no `role="combobox"`, no `aria-expanded`, no `aria-controls`. Arrowing moves a `.at` class
(`main.js:2165`) and announces nothing. The mention menu beside it *was* given roles —
`role="listbox"` at `index.html:285`, `role="option"` and `aria-selected` at `main.js:2352-2353` — so
this is a known pattern applied to one of two menus.

Even the menu that has roles is incomplete: neither wires `aria-activedescendant` from the focused
control to the highlighted option, which is what actually makes an arrow key speak. Focus stays on the
input in both, so with no `activedescendant` the selection change is unobservable in both.

**Why it matters.** The palette is not a convenience here; with no other keyboard route to switch Bots,
it is the keyboard route to the product's primary navigation. Unannounced, it degrades to "type a
query, press Enter, find out where you landed". The message-search results make it worse: they arrive
150-500ms after the names (`main.js:2444-2461`) and are appended silently, so the list a screen-reader
user committed to has changed underneath them with no announcement at all.

**The durable fix.** One combobox pattern, factored once and used by both menus, since they are the
same widget: `role="combobox"` + `aria-expanded` + `aria-controls` on the input/textarea,
`role="listbox"` on the list, `role="option"` + `aria-selected` + a stable `id` per item, and
`aria-activedescendant` on the control updated wherever `paletteAt`/`mentionAt` moves. Add a polite
live region for the count ("12 results", "8 more from your conversations") so an async append is
audible. This is the natural first extraction if F-DC12 happens — two call sites of one pattern is
exactly what a shared function is for.

**How to prove it.** In `page.rs`, alongside `the_palette_finds_a_bot_and_opens_it` (`:572`) and
`typing_an_at_offers_the_teammates_the_sidebar_shows` (`:897`), add
`a_menu_that_moves_with_the_arrows_says_which_item_is_current`: open each menu, press ArrowDown, assert
the focused control's `aria-activedescendant` names the element carrying `.at` and that the element has
`aria-selected="true"`. It fails on both menus today, on different assertions.

---

### F-DC11 — a blank hub field connects to a different scheme and a different port than the two tests that pin the default
`P1` · `reach: some users` · `crates/botroster-app/ui/main.js:1259`

**What is true now.**

```js
hub: hub || "http://127.0.0.1:9812",
```

The shipped field value is `ws://127.0.0.1:8443/v1/tools` (`index.html:43`), and two tests exist to
keep it honest: `defaults.rs:78 the_connect_panel_defaults_to_the_hub_botroster_actually_starts` pins it
against the binary's own default, and `defaults.rs:91 the_shipped_hub_default_is_a_websocket_url` pins
the scheme because — in its own words — "a `--hub` that is not a WebSocket URL fails before it reaches
the network". Both read the *markup*. Neither sees the fallback, which differs in scheme (`http` vs
`ws`), in port (9812 vs 8443) and in path.

The line directly above does the opposite, deliberately, with a comment saying why:

```js
home: home || (await invoke("default_home")),
// No tilde. If the field is somehow empty the shell resolves it, which
// is the same answer the field was filled with…
```

The argument for asking the shell rather than hard-coding a literal is made in full, one line above
the place it is not applied.

**Why it matters.** Anyone who clears the hub field — to retype it, to try a different runtime, or
because they assumed blank meant default — connects to something that cannot work, and the failure is
a scheme error about an address they never typed and cannot see. Small blast radius, completely
avoidable, and it silently defeats a test written specifically to prevent this failure mode.

**The durable fix.** Delete the literal. Either resolve it the way `home` does — the shell knows the
binary's default and `defaults.rs` already extracts it — or, simpler for a field with a shipped value,
refuse to send a blank: restore the field to its shipped `value` and let the person see what will be
used before Connect fires. The general rule this file should adopt: a fallback that differs from the
shipped default is a second definition, and this codebase already has a written position on second
definitions (`main.js` on `config.toml`: "a second writer is a second definition free to drift").

**How to prove it.** In `page.rs`, `an_empty_hub_field_connects_to_the_shipped_default`: clear
`#hub-url`, click Connect, assert the `hub` argument the stub received equals the value `index.html`
ships. It fails today with `http://127.0.0.1:9812`. Cross-check in `defaults.rs` by asserting `main.js`
contains no hub literal that `the_shipped_hub_default_is_a_websocket_url` would reject.

---

### F-DC12 — `main.js` has no navigable structure at 2,632 lines, and has already had to work around its own ordering
`P1` · `reach: some users` · `crates/botroster-app/ui/main.js:104-110`

*(Reach is contributors rather than end users; filed because the brief asked directly whether vanilla
is still the right call at this size and whether the code is structured so a contributor can find
things. It also gates several fixes above.)*

**What is true now.** One flat module scope, 2,632 lines, in which element `const`s are declared in six
separate places (`:8-63`, `:1566-1570`, `:1813-1819`, `:1986-1991`, `:2121-2124`, `:2214-2216`), event
wiring is interleaved with rendering (the credential keydown handler sits at `:52`, inside the element
declarations; the bypass click handler at `:235` sits between `renderAttached` and the attach handler),
and the section banner reading `// ---- wiring` at `:1371` is followed by provider presets, path
pickers, the Edit dialog, the rules panel, the credentials panel and the computer pane.

The file has already paid for this. `MODAL_IDS` is a list of *ids* rather than elements, and the comment
says why: *"because several of these are declared much further down this file than `show` is, and
reading a `const` in its temporal dead zone throws"* (`:107-109`). That is not a style preference; it is
a data structure chosen to route around the file's own layout.

**Why it matters.** Vanilla is the right call and should stay — this is a desktop client with no network
dependency and a build step would be pure cost. But "no bundler" is not the same as "one file", and at
this size a contributor cannot find where a surface lives without a full-text search. Every finding in
this report lands in this file, and several (F-DC5's helper, F-DC10's shared combobox, F-DC6's single
status writer) are extractions the current shape makes awkward to place.

**The durable fix.** Two options; recommend the first.

**(a) Split by surface, using native ES modules.** `<script type="module" src="main.js">` and `import`
are not a bundler, not npm and not a build step — they are the platform, and every WebView2 /
WKWebView / WebKitGTK target supports them. One file per surface: `transcript.js`, `roster.js`,
`approvals.js`, `connect.js`, `settings.js`, `palette.js`, `mentions.js`, plus a `dom.js` for `$`,
`show`, `applyModality` and `setStatus`. **Three things break and must be handled in the same commit:**
`page.rs:88` splices the Tauri stub immediately before the literal marker
`<script src="main.js"></script>`, so the marker changes; `page.rs:99` serves only `/main.js`, so
sibling imports 404 until the harness serves the directory; and `main.js:3-4` reads `window.__TAURI__`
at module scope, which `type="module"` defers — the stub is injected before the tag either way, so this
is safe, but it must be confirmed rather than assumed, and the fixture is shared with
`.claude/ux-loop`'s node server, which reads the same files.

**(b) Keep one file and impose a stated order** — constants, state, render, wire — with every element
`const` in one block at the top, pinned by a test asserting no `getElementById` const is declared below
the first `addEventListener`. Cheaper, touches no test infrastructure, and it would let `MODAL_IDS`
hold elements instead of ids, which is the small proof that the ordering problem is gone.

**How to prove it.** For (b) the test is directly writable and belongs in `defaults.rs`, which already
parses these files as text: `the_page_declares_what_it_touches_before_it_wires_it`. For (a) the proof is
that `page.rs`'s existing suite passes unchanged, plus `every_ui_module_is_reachable_from_the_page`
asserting no module 404s — a refactor whose only correctness claim is "nothing changed" needs the
existing suite to be the assertion.

---

### F-DC13 — `docs/` ships pre-rename screenshots showing a third visual system, the old product name, and a developer's home path
`P2` · `reach: most users` · `docs/botroster-connect.png`, `docs/botroster-computer.png`

**What is true now.** Six screenshots sit in `docs/`. Two (`botroster-thread.png`,
`botroster-approval.png`, both 2026-08-20) show the current build. Four (2026-08-17) predate the rename
and show a different product:

- `docs/botroster-connect.png` labels its fields **"roost binary"** and **"agent home"**, its lede reads
  "Your Bots live in a **roost** you run yourself… the defaults match `roost up`", the paths read
  `C:\Users\Mandar\Desktop\grokbot-recon\roost\target\debug\roost.exe` and `C:\Users\Mandar\.roost`,
  and the frame is a navy-and-orange palette that exists nowhere in the current stylesheet. It also
  predates B18, so it shows a connect panel with no Model section.
- `docs/botroster-computer.png` shows a viewer chrome reading **"roost · computer"** and an `fs.write` of
  `roost-demo.md`, again in navy and orange.

So the folder contains three mutually exclusive visual systems, and the four stale frames carry the
pre-rename name in nine places. They also leak the developer's Windows username and working directory
into a public repository. Only two are referenced from `README.md` (`:2`, `:165`); the other four are
shipped and unlinked.

**Why it matters.** `CLAUDE.md`'s provenance section makes this project's independence from the
reference product its "entire legal position", and `PROVENANCE.md` is described as a gate rather than a
formality. Documentation carrying a superseded product name in nine places is a smaller version of the
same hygiene problem, and it is among the first things a reader browsing the repo encounters. The design
cost is separate and real: anyone who opens `docs/` to see what the client looks like sees three
different products and cannot tell which one they would get.

**The durable fix.** Delete the four stale files. Regenerate whichever states the docs need from
`.claude/ux-loop`'s shot harness, which produces 52 shots in ~4 seconds (MEMORY, "Shot budget is not
the constraint") and can therefore be re-run on any visual change rather than accumulating a fossil
layer — and which runs against a fixture, so no real home path appears. Add a check to the readme test
that greps `docs/` and the tree for the old name, so the rename is finished rather than mostly
finished. Regenerate *after* F-DC1's re-skin, not before, or they go stale on the next commit.

**How to prove it.** `crates/botroster-cli/tests/readme.rs` already parses `README.md` and fails the build
on stale content, which is the precedent. Add `no_shipped_image_predates_the_rename`: assert every file
in `docs/` referenced by `README.md` exists, and that no file in `docs/` is older than the most recent
change to `crates/botroster-app/ui/styles.css`. A text-level companion — grep the tree for the old name
outside `PROVENANCE.md` — catches the name independently of the images.

---

### F-DC14 — a working Bot pulses forever, a Stop failure says "search failed", and a search hit never says who said it
`P2` · `reach: most users` · `crates/botroster-app/ui/styles.css:834-836`, `main.js:2597`, `main.js:2468-2473`

Three small, separate, cheap defects, folded because each alone is under the materiality bar.

**What is true now.**

1. `.status.busy::before { animation: pulse 1.2s var(--ease) infinite; }` (`styles.css:834-836`).
   DIRECTION's Banned list contains "Animation that does not communicate state" and states "nothing
   loops"; its motion section allows exactly one duration, `160ms`. This is 1.2s and infinite. MEMORY
   iteration 004's own note says the pulse was half the problem — *"and it pulsed, which made the least
   interesting fact on the screen the loudest thing on it"* — and the fix that followed removed the
   amber and left the loop. `styles.css:138-145` does neutralise it under `prefers-reduced-motion`, so
   this is a DIRECTION violation, not an accessibility defect.
2. `cancelBtn`'s failure handler reads `reportProblem(err, "search failed")` (`main.js:2597`). Press
   Stop, have the cancel fail, and the window says the search failed. B15's whole point was that each
   of nine sites got "its own short WHAT rather than a generic one"; this one got another site's.
3. A palette message hit is built as `{ label: hit.text, kind: "Said" }` (`main.js:2468-2473`).
   `hit.name` is used to open the conversation and never shown, so a search across every Bot returns a
   list of sentences with no indication which teammate said any of them — and `label` is the raw
   message text, unbounded, in a 12-row list.

**Why it matters.** (1) is the one place the shipped build contradicts DIRECTION in *motion* rather than
colour, and it is on screen for the whole duration of every run — the longest-lived animation in the
product is the one DIRECTION forbids. (2) sends somebody debugging a stuck Stop to look at the search.
(3) makes cross-Bot search — the thing that justifies having many Bots — unusable for its main purpose,
which is remembering *who* knew something.

**The durable fix.** (1) Delete the `animation` line; the dot's colour and the pill's text already carry
the state, and F-DC4's in-log pending row is where "it is still going" should be expressed.
(2) `"could not stop the run"`. (3) Render the hit as two spans the way `mentions-item` already does —
the Bot's name in `--text`, the matched text in `--faint`, clamped to one line — and reuse `markEl` for
the coat, so a search result is recognisable by the same mark as the roster row it opens. That is the
provenance argument DIRECTION calls the signature element, applied to the cheapest surface it fits.

**How to prove it.** (1) `page.rs` has a reduced-motion assertion already; add `nothing_in_this_window_loops`,
sweeping computed styles for `animation-iteration-count: infinite` and asserting none. It fails on
`.status.busy::before` today. (2) A grep-level assertion in `defaults.rs` that no two `reportProblem`
call sites share a WHAT string. (3) Extend `typing_in_the_palette_searches_once_it_settles` (`:2804`) to
assert each message hit renders the Bot's name.

---

## What I could not check

- **The rendered pixels of the current build.** I was instructed not to launch the app or run the suite,
  so every visual claim comes from `styles.css`, `index.html` and the six screenshots in `docs/` — two
  of which are current and four of which are not (F-DC13). `.claude/ux-loop/shots/` is gitignored and
  was not present, so the 52-shot set the backlog cites could not be consulted.
- **Whether `role="log"` + `aria-live="polite"` on `#log` (`index.html:246-247`) causes a screen reader
  to read the entire replayed history when a Bot is opened.** `openBot` appends every history chunk into
  that container (`main.js:760`) while it still carries `.hidden`, and whether a live region announces
  content inserted while `display:none` and then revealed differs between NVDA, JAWS and VoiceOver. It
  needs a real screen reader, which I do not have. If it does announce, it is a P0 for those users; I
  have not filed it because I could not confirm it.
- **Light theme beyond one frame.** `docs/botroster-thread-light.png` is equal in quality to its dark twin
  on the surface it shows — rubric 16 passes on that evidence — but it is one of twelve scenarios and
  predates iteration 004 (its `connected` pill still wears the fill that iteration removed). The approval
  dialog, the connect panel with the Model section open, and the computer pane have no light frame I
  could inspect.
- **Whether the `#problem` banner's single slot loses a second failure.** `reportProblem` overwrites
  `#problem-what` and `#problem-raw` unconditionally (`main.js:94-99`), so two failures in quick
  succession should leave only the second — but I could not construct the timing to confirm the first is
  genuinely unreachable rather than merely replaced after being read.
- **Real latency numbers.** F-DC5 argues from the fact that each of these calls spawns an `botroster`
  subprocess — which the code states at `main.js:2233-2236` and budgets a 3-second throttle around — not
  from measurement. How bad the blank interval actually is on a warm machine is unknown to me.
- **`crates/botroster-desktop`'s Rust surface below the seam.** I read `engine.rs`, `viewer.rs` and the
  command signatures for the state questions above, but `settings.rs`, `policy.rs`, `secrets.rs`,
  `skills.rs`, `roster.rs` and `attach.rs` were read for what they return to the window, not audited for
  correctness — that is the Runtime & Security and CLI departments' seam, and I did not want to duplicate
  or contradict them.
