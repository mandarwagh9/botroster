# MEMORY

Read this at the top of every iteration. It is the only thing that makes the loop compound
instead of random-walk. Append; never rewrite history.

---

## Phase 0 — what the harness is, and what it cost to learn

**The IPC seam is one line.** `main.js:3-4` reads `window.__TAURI__.core.invoke` and
`.event.listen` at module scope. Defining `window.__TAURI__` before `main.js` loads is the
entire fixture — no transport rewrite, no change to `main.js`, no bundler. Any future harness
work should reach for this seam first.

**There was already a double, and it is now shared.** `crates/openbot-app/tests/page.rs` had
a 32-line `window.__TAURI__` stub inline, plus a loopback static server and a splice
assertion on the `<script src="main.js">` marker. Writing a second double for the JS harness
would have been the exact failure `page.rs` warns about in its own comments — a hand-written
fixture that cannot fail when the Rust shape changes. The stub is now
`crates/openbot-app/tests/fixture/tauri-stub.js`; `page.rs` reads it with `include_str!` and
the node server reads the same file. **Lesson: look for the existing double before writing
one.**

**Theme is `light-dark()`, not a class.** `styles.css` themes entirely through CSS
`light-dark()`, which reads `prefers-color-scheme`. Playwright's `colorScheme` context option
switches it; a `data-theme` attribute or a class toggle silently does nothing. This cost a
few minutes to notice and would have produced 24 identical "light" screenshots.

**axe's own contrast rule is useless here** for the same reason: it cannot resolve
`light-dark()` against the context scheme and reports the light values while the page renders
dark. It is disabled in `ux-audit.mjs`; contrast is computed from what actually rendered.

**Module resolution.** The harness keeps `node_modules` under `.claude/ux-loop/` so
`crates/openbot-app/ui/` stays npm-free and buildless, which is a documented property of this
repo. Node resolves upward from `scripts/` and never finds it, so both scripts use
`createRequire` pointed at `.claude/ux-loop/package.json`. Do not "fix" this by adding a
root `package.json`.

**Shot budget is not the constraint.** 48 shots (12 scenarios x 2 sizes x 2 themes) take
**4.0 seconds**, against a 90s budget. The iteration cost is entirely `cargo check`, `clippy`
and the `page` suite. If iterations need to be faster, that is where to look — not here.

---

## Reachability — three of the twelve states are not fully reachable

Recorded here because a future iteration will otherwise try to "fix" a screen that does not
exist, or worse, fake the fixture to make a rubric line pass.

- **s04 statuses** — the roster payload has no status field. Four Bots render identically.
  The fixture does **not** invent one. See BACKLOG B01.
- **s07** — a hub `deny` never arrives as an ask; it lands as a refusal result row in the
  thread. That row is what the fixture shows. There is no client-side denial dialog.
- **s10** — the pane is an `<iframe>` fed by the hub's viewer. No hub, nothing inside the
  frame. Takeover has no window-side surface at all.
- **s12 erroring** — a routine can only be enabled or paused. No error state exists.

---

## Iterations

### 001 — cleared every gate the baseline failed · held · `53c6bde`

`.step-state` `--ghost` → `--muted`; `#log` given `tabindex="0"` and `role="log"`; the rules
`select` given a label. axe serious 4→0, critical 2→0, contrast 2→0.

**The lesson is the fixture bug, not the fix.** The first backlog item (B02, "the approval
dialog makes the grant the loudest thing") was raised off a screenshot of a dialog the product
never renders: the fixture had `danger: true` on the session grant, and `renderDialog` styles
on that flag, so the harness painted the large grant in refusal styling and the refusal in
quiet styling. It was caught by reading the option-construction loop in `main.js` before
patching — not by looking harder at the picture.

**Generalisable: before filing a defect against a rendered state, read the code that renders
it.** A fixture is an assertion about what the shell sends, and a wrong one produces a
confident, well-evidenced, entirely fictional defect. The screenshot is not the ground truth;
it is only as true as the fixture behind it.

### 002 — no Bot wears amber · held · `ae0c5f3`

`--coat-4` `#f19d38` → olive. Two faults: amber is reserved for "a person is blocking
progress", so an idle Bot wearing it raised the waiting-on-you signal for nothing; and it sat
at roughly `--coat-3`'s hue, so two Bots read as the same orange.

**Generalisable: a semantic palette is violated from the identity layer, not just the status
layer.** The baseline scored rubric 18 as clean because it looked for stray amber in the
chrome. The violation was in the set of colours a Bot can be *assigned*. When a token means
something, audit every palette that can produce it, not just the places that mean it on
purpose.

**Also: run the narrow test first.** `every_coat_a_bot_can_wear_is_legible` answered in 0.85s
what the full gate would have answered in eight minutes. On a token change with a dedicated
test, that test is the fast reject.

### 003 — the empty conversation says something true · held · `12e3b21`

`#no-bot` copy now comes from the roster count, and the card carries the action. It read "Pick
one on the left, or make your first" in the one case a new install actually starts in — zero
Bots, nothing on the left.

**Generalisable: empty-state copy is usually written for the populated case and then left.**
Check every string that describes the rest of the UI against the state where the rest of the
UI is absent.

### 004 — transport is quiet, working is not amber · held (see below)

`.status.connected` gives up its filled wash and keeps a dot; `.status.busy` moves off
`--warn`. Same class of fault as 002: a *working* Bot was painted in the "needs a human"
colour, and it pulsed, which made the least interesting fact on the screen the loudest thing
on it.

**Generalisable: the second instance of a fault is worth looking for immediately.** Having
found amber misused once (002), grepping the stylesheet for every other use of `--warn` found
this one in about a minute. One misuse of a semantic token predicts others.

---

## Process notes for whoever runs this next

- **The gate cycle is ~8 minutes and it dominates everything.** Shots are 4-5s. `cargo check`
  + `clippy` + the 57-test page suite is the whole cost. Batch a patch, start the gate in the
  background, and do the next iteration's reading while it runs.
- **This branch has another writer.** Commit `104dc9a` is not from this loop; it fixed
  README's stale `./openbot-data` default and swept in the `page.rs`/stub extraction. Because
  of that, **do not use blanket `git checkout .` to revert** — it would discard somebody
  else's in-flight work. Revert the specific files the iteration touched.
- **`sh scripts/ux-verify.sh | tail` hides the exit code** (you get `tail`'s). Redirect to a
  file and check `$?`, which is what the `.gate-NNN.log` files do.

---

## Run 2 — journey-first, 2026-08-20 morning

### The fixture is structurally blind to J1 and J2

**Every scenario from `s03` onward sets `window.__replies.connected = true`.** The harness
opens on the far side of the wall. That is correct for photographing steady-state surfaces,
and it is exactly why run 1 cleared four real defects and never noticed that a fresh install
cannot start at all.

J1 and J2 had to be established by running the shipped binary
(`%LOCALAPPDATA%\OPENBOT\openbot.exe acp --home …`), not by looking at pictures.

**Generalisable: a screenshot harness is only as honest as the state it opens in.** Before
trusting a journey analysis, ask what the fixture asserts as already true. Whatever it
asserts, it cannot see.

### 011 — the window had the answer and threw it away · held

`openbot acp` refuses to start with no model and says both the fault and the fix on stderr.
`Engine::connect` reported `openbot acp ended before the handshake`.

Two routes were tried, and the first one failed for an instructive reason:

1. **Await the task's error.** The task is `JoinHandle<Result<(), acp::Error>>` and the SDK
   formats a nonzero child exit as `Process exited with {status}: {stderr}`. But the
   transport-closed error beats the child-exit report to the return value, so awaiting the
   task yields `Incoming transport closed: {"reason":"incoming_transport_closed"}` and
   nothing about the cause. **The test caught this** — it was written to assert the message
   names the model, so a plausible-looking improvement that carried no cause still failed.
2. **`AcpAgent::with_debug(|line, direction|)`**, buffering `LineDirection::Stderr` lines.
   This does not race: the lines are read as they arrive.

**Generalisable: when a fix must surface a *cause*, assert on the cause in the test, not on
the message being different.** Asserting "not the old string" would have passed on route 1
and shipped a window that said `Incoming transport closed` to somebody whose real problem
was an unset API key.

Also: `Engine` implements `Drop` (it aborts the task), so the handle cannot be moved out of
an assembled engine. The engine is now built only on the success arm.

### The preflight gate blocked on something harmless

The check matched any process named `openbot*`, so it failed the whole run because the
**installed** app in `%LOCALAPPDATA%\OPENBOT` was open — a different file on a path cargo
never writes to. Only a binary running out of this checkout can hold this build, so it
matches on path now.

**Generalisable: a gate that stops the run for something harmless is a gate that gets
switched off.** False positives cost more than the check is worth.

### Persisting the bypass — two failures worth keeping

**The fixture inherited state it could not see.** Bypass is remembered in `localStorage`,
which is per-origin, and Playwright shares storage across pages in one browser context. So
`s14` clicking bypass *on* leaked into every scenario loaded after it, and `s06`'s approval
dialog stopped opening — the window was already approving everything. The gate caught it at
exactly the right place: `approval invariant: the approval dialog did not open in s06`.

**Generalisable, and this is the second time it has bitten:** whatever the fixture does not
set, it inherits, and whatever it inherits it cannot see. The first time it was
`connected = true` hiding J1/J2. Every scenario now clears storage before `pre()`.

**I made the mistake this file already warns about.** The two persistence tests used
`settle()` twice after `location.reload()`. They passed alone and failed in the full run,
because a reload under a loaded suite takes longer than 300ms. `wait_until` now, with a
deadline. Reading your own notes is not the same as applying them.

### The page suite is near its concurrency limit

One run failed with `chrome-error://chromewebdata/` on a test nothing had touched — a
navigation that never completed, not an assertion. Evidence: three clean runs at 229s, 173s
and 177s against one failure at **104s**. The failing run was the fastest, which is the tell:
more parallelism, more contention.

Each test in `page.rs` spawns a Chromium *and* a loopback server. Adding five browser-driving
tests for bypass pushed a suite that was already close. Nothing here is a defect, and a
retry-once on navigation in `page()` would probably remove it — but that is shared test
infrastructure and the flake could not be reproduced on demand, so it is recorded rather than
guessed at. **If CI starts failing on unrelated browser tests, this is the first thing to
look at.**

---

## Run 3 — ready by default, 2026-08-22

### There is no free unmetered agent-capable endpoint, and it was measured

The task was "make the exe ready by default", believed to mean *free* by default.
Three candidates, all rejected on evidence rather than on reading:

- **Pollinations** (keyless) genuinely returns `tool_calls` with `finish_reason:"tool_calls"`.
  It then 402s — "API key budget too low" — on roughly the second *fresh* request. The trap:
  a repeat of an identical request comes back **200 from cache**, same `id`, same `created`
  timestamp. Retrying the same body and seeing 200 proves nothing. Vary the body, or read the
  id.
- **OmniRoute** (`npm i -g omniroute`, 1196 packages) survives repeated calls where Pollinations
  dies, so its fallback is real. But `stream:false` fails outright — "Maximum combo retry limit
  reached", poolSize 54, attempted 27 — and the diagnostics name the zero-credential pool:
  `theoldllm/CLAUDE_4_6_OPUS`, `duckduckgo-web/gpt-5-mini`, `auggie/aug/prism`. Those are
  reverse-engineered endpoints, not documented free tiers. **Read the `diagnostics` block of a
  gateway's failure — it lists what it actually tried, which the README does not.**
- **Ollama** passes: `tool_calls`, non-streamed, unauthenticated, `{base}/chat/completions`
  matching what `http.rs` already builds. Free, no account, offline, no ToS question.

**Generalisable: "free" and "no setup" are different axes, and a local model trades one for the
other.** The deliverable became zero-*terminal* setup plus a genuinely free local path, which is
what could honestly be built.

### `stream` was never sent, and the vendors hid it

`HttpModel::turn` reads the whole body and parses one JSON object. Every real vendor defaults to
non-streaming, so the missing field was invisible for the life of the project. A compatible
gateway defaulted the other way and answered in `data:` chunks, surfacing as `Malformed` — which
reads as a broken provider rather than as streaming at something that cannot stream.

**Generalisable: a default you rely on but never state is a dependency on every implementation
agreeing with you.**

### The width test caught a CSS bug I would not have looked for

Adding the first `<input type="checkbox">` to this UI tripped
`no_dialog_field_is_narrower_than_the_placeholder_in_it`. The reflex is to exempt checkboxes and
move on. The actual cause was that the global `input` rule sets `width: 100%` and a text field's
padding, so the checkbox rendered as a **373px full-width bordered box**. Both were needed: fix
the CSS, *and* exempt checkboxes from a text-width rule (`value` is `"on"`, never drawn).

**Generalisable: when a new element trips an old test, find out why before exempting it. The
exemption is often correct and still not the whole answer.**

### The page suite flake has a likelier cause than "concurrency"

`escape_closes_a_panel_but_never_an_approval` failed inside `ux-verify.sh` and passed 3/3 alone
and in a standalone full-suite run. The gate had been started **immediately after
`cargo test --workspace`, which runs the page suite too** — so Chromium instances from the
previous run were probably still winding down. Re-run alone: clean, 68 passed.

**Before filing a page-suite flake, check what was running just before it.**

### Credential persistence reused what was already there

The window collected a key into the spawned agent's environment and nowhere else, so it was
retyped at every launch. No new dependency was needed: `openbotd::secrets::SecretStore` is the
0600 `secrets.json` connector tokens already use, `openbot secret set` reads the value from
**stdin** (no `--value` flag, deliberately), and `openbot-cli` already depends on `openbotd` — so
`isolation.rs` was unaffected. Environment first, store second, and that order is asserted:
a stored key that silently overrode an exported one makes "why is it using the wrong key"
unanswerable from the shell.

**Generalisable: before adding a crate for a capability, check whether a sibling subsystem
already solved it — and whether the invariant tests already permit the edge you need.**

### The page-suite flake is now two data points, and I made it likelier

`storing_a_credential_sends_it_as_a_value_not_as_an_option` failed inside `ux-verify.sh` and
passed 3/3 alone and on the very next gate run. That is a **different test** from the earlier
`escape_closes_a_panel_but_never_an_approval` failure, which is the tell: it is not a bug in
either test, it is the suite.

Each test in `page.rs` spawns a Chromium *and* a loopback server. This session added five
browser-driving tests (three for providers and the remembered key, one for the keep-this-key box,
one for the product mark), taking the suite from 63 to 69. Every one of them makes the next flake
likelier.

**This is worth fixing and was deliberately not fixed here**, because it is shared test
infrastructure and the task in hand was a logo. The shape of the fix: `page()` retries **once**
when navigation lands on `chrome-error://chromewebdata/`, which is a navigation that never
completed and is distinguishable from an assertion failure. A blanket retry would be wrong — it
would hide real failures — so it must key on that condition only.

**Generalisable: when you add load to a suite that is already flaking, say so in the same breath
as the feature.** A flake that arrives with a change looks like the change caused it, and the next
person will go looking in the wrong place.

### The obvious liveness check was the one that could never fail

`Engine` holds a `JoinHandle`, so `!task.is_finished()` reads as the answer to "is the agent still
there". The SDK's own documentation says otherwise: a clean incoming EOF "does not cancel unrelated
work in `main_fn`", and that `main_fn` waits on a command channel for the life of the `Engine`. The
check would have been **true over a corpse, forever**, and the test written against it would have
passed while asserting nothing.

Then the measurement disagreed with the documentation in the other direction. Killing the child
trips *both* signals every time, five runs — because a child that dies of a signal ends the
transport with an *error*, not a clean EOF, and that does end the task. Both are kept, and the
comment says which ending each covers and which one no test here can stage.

**Generalisable: a liveness check has two failure modes and only one of them is visible. It can say
dead when alive, which you find immediately, and alive when dead, which you never find — because
that is also what a working system looks like. Mutate it: implement the naive version and run the
test. If the naive version passes too, the test is not testing the check.**

### A negative test needs the window the bug lives in

`disconnecting_is_not_reported_as_a_crash` passed with the two statements in **either order**,
because the stub answered `disconnect` instantly and the poll never got a tick in between. The bug
lives entirely inside the time that call takes, so the test now stages a four-second `disconnect` —
which is not a contrivance: that call drops the viewer and waits on the children it kills.

**Generalisable: when the fix is an ordering, the test has to hold the window open. A race you
cannot lose is a race you are not testing.**

### Ask the system, do not read its error text

A turn that failed over a dead runtime arrived as the literal string `openbot acp is gone`. Matching
on it would have worked, and would have broken silently at the next rewording — in the direction of
showing the worse message. One extra IPC call on an already-failed path answers the question
outright.

**Generalisable: branching on somebody else's error wording is a dependency nobody records. If the
state is queryable, query it.**

### A focus assertion that matched the whole document

`the_roster_does_not_take_focus_off_the_row_you_were_on` first asserted
`document.activeElement.textContent.includes("Bot 7")`. When focus is thrown to `<body>`,
`activeElement.textContent` is **the text of the entire document** — so the assertion matched, the
test passed, and it passed against the exact code it was written to condemn. Node identity
(`activeElement === querySelectorAll(...)[7]`) is the assertion that discriminates.

**Generalisable: an assertion about *which* element must compare elements. Any assertion about a
focused element's text is really an assertion about the document, because `<body>` is the fallback
and `<body>` contains everything.**

### Half of a review finding did not reproduce

F-DC9 said the roster redraw threw away both focus and scroll. Focus, yes, always. Scroll, no: the
wipe and the rebuild are one synchronous block, no layout runs between them, and the offset is never
clamped — 300px before, 300px after. Forcing a layout in between does zero it, which is presumably
where the claim came from.

**Generalisable: reproduce each half of a finding separately before fixing it. A compound claim can
be half true, and the false half quietly becomes a test asserting something that never broke —
which then passes against the unfixed code and certifies it.**

### The rule was already in the file, stated once, applied once

`refreshMentionable` carried this comment: *"Independently: a home with a broken connector should
still offer skills, rather than one failing list leaving the composer with nothing."* Three hundred
lines away, `refreshWiring` did exactly what that sentence forbids — two awaits in one `try`, so a
failing connector read meant routines were never fetched and **both** sections rendered as nothing.
`refreshGroups` had the same fault, one level out, with `catch { return; }` — directly under
`refreshRoster`'s comment insisting it must never do that.

Three functions, one rule, stated in prose next to the one place that honoured it.

**Generalisable: a rule written as a comment is applied exactly where it is written. If a comment
argues for a general principle, that is the signal to grep for the other call sites now — and the
durable fix is a helper, so honouring it is the default rather than something to remember.**

### Every state a surface can be in needs a name, or `empty` absorbs them all

Blank-because-nobody-has-looked, blank-because-there-is-nothing, and blank-because-the-read-failed
are three different facts that rendered identically. The empty state is the greedy one: it is the
only one with copy written for it, so it becomes the default answer to every kind of blank.

**Generalisable: `pending` / `empty` / `failed` / `filled`, and `empty` may only be shown after an
answer has arrived. If a surface can only say one of these, it is saying it about states it has not
distinguished.**

### The gate was measuring the practice the project requires

Phase 3's last item went through the bundle ceiling by 610 bytes. The rule for that case was
already written — *"a finding about the phase, not a reason to move the line"* — so the phase got
measured, and the measurement indicted the line: **comments were 44% of the bundle and 52% of
`main.js`**. `CONTRIBUTING.md` requires those comments, there is no build step, so they ship, and a
raw-byte ceiling on a file that ships its comments is a limit on how much of the reasoning
survives. Two project rules pulling against each other with only one of them enforced.

Gzip resolves it without a minifier or a parser: prose discounts about ten to one, novel code
counts close to full, and the number is what a person actually waits for.

**Generalisable: when a limit blocks work the project separately requires, check what the limit is
a proxy for before treating the work as the problem. And put the change to the operator — a gate
the gated thing may redefine is not a gate.**

### Changing a gate's unit can turn it vacuous instead of failing

`.baseline-bytes` is **gitignored**. Switching the gate to compressed bytes while reading the same
filename would have left every machine that ran the old gate comparing ~62,000 compressed bytes
against a ceiling derived from ~180,000 raw ones — passing everything, forever, silently. Failing
loudly would have been fine. Passing silently is the failure mode this repository names as worse
than a failure.

The fix is the filename: `.baseline-gzip-bytes`. The old file is then simply not this file.

**Generalisable: when the unit of a stored number changes, change its name. A stale value in the
old unit does not announce itself, and a gate that stops gating looks exactly like a gate that
keeps passing.**

### A full disk presents itself as a flaky page suite, for the second time

The suite failed 88 of 94, three runs running. I had just made the loopback server a shared
singleton to relieve what I had diagnosed as descriptor pressure, so I read the failures as my
change and began reverting — and the revert failed with **out of diskspace**, which is when the real
cause appeared: `tempfile::tempdir()` deletes on drop, that delete is best-effort on Windows, and
`Browser`'s `kill_on_drop` ends Chrome's *root* process while its renderers keep the profile open.
Every page test leaked a profile. **Twenty thousand directories, a 952GB disk at zero bytes.**

`MEMORY.md` already recorded a full disk masquerading as `FAIL page suite` once. Two occurrences is
not a coincidence — the diagnosis now lives in a doc comment beside the code that caused it, not
only here.

**Generalisable: when a suite fails en masse right after a change, check the machine before
believing the change. Disk, descriptors, held binaries — an environment failure is indistinguishable
from a regression in the test output, and it is the reading that costs a good change.**

The corollary bit twice in one hour: `git checkout` truncated `page.rs` to **zero bytes** when it
ran out of space mid-write. Committed work came back; uncommitted work would not have.

**Generalisable: on a machine that may be near full, commit before reverting. A failed write is not
a no-op.**

### The measurement that measured the wrong thing

The shared-server change was never evaluated — its three "failing" runs were the disk. It is not in
the tree, and not because it was judged bad. Shipping it on that evidence, or discarding it as bad
on that evidence, would both have been the same error: treating a measurement of the environment as
a measurement of the change.

**Generalisable: when the ground moves under an experiment, the experiment has no result. Say so,
rather than keeping whichever conclusion is convenient.**
