# OPENBOT client redesign

**Design read:** dense desktop product UI for people supervising autonomous agents, with a console
language, leaning toward instrumentation typography and status-only colour — not a chat app.

Written 2026-08-25 after reading a competitor's desktop client for interaction lessons. Governed by
`DIRECTION.md`, which is the authority on colour, type, shape, depth and motion. This file is the
sequence.

---

## 0. The strategic call, first, because everything else follows from it

**Do not rebuild their chat app.** The reference is a chat product with a computer bolted to one
side. `DIRECTION.md` already committed to something else and was right:

> This is a console, not a chat app. Chat is the input method. The product is a set of named
> teammates working on one durable computer under your gate.

Copying the bubbles would land OPENBOT as a worse version of a thing that already exists, and would
throw away the only structural advantage it has. A clone that looks worse is the weakest position
available. The goal is not parity of appearance. It is that a person watching four Bots work sees
more, sooner, than they would in the reference.

Two things are true at once and both matter:

- Their **interaction architecture** is better than ours in specific, nameable ways. Those are worth
  taking, and section 1 lists them.
- Their **information design** is weaker than what DIRECTION describes. Section 2 lists where we
  should be plainly better, not equal.

---

## 1. What the reference genuinely teaches

Interaction principles only. No colour, type, spacing, icon or layout is lifted; each of these is
restated in OPENBOT's own language before it becomes work.

1. **The computer is a permanent peer pane.** Theirs sits in a right rail with a live thumbnail and
   a caption, present even when idle. Ours is behind a button. This is already `BACKLOG.md` B03, and
   seeing it standing in someone else's product settles the argument: burying the differentiator
   throws the differentiator away.

2. **Editing happens in the rail, in place, with a back stack.** Their routine editor and Bot
   settings both open in the right rail with a back arrow, not a modal. You keep watching the Bot
   while you change its schedule. A modal that covers the conversation to edit the conversation's
   owner is the lazy answer.

3. **Rehearsal is a first-class control.** `Test run` sits beside `Active` and `Delete`, at the top
   of the routine editor. The parity report ranked "routine test run" as a top-five gap, and their
   placement is the answer: not buried in a menu, adjacent to the toggle that arms it.

4. **Triggers are named integrations, not a generic event field.** Their menu reads Slack message,
   Git event, Linear issue, Sentry alert, PagerDuty incident, Webhook. OPENBOT's event model already
   supports this shape — `openbot-bots` has source matching, conditions and nested path lookups, all
   tested. It has no surface. A dropdown of real names is a different product from a JSON field.

5. **Placeholders carry the instruction.** "Describe what your Bot does", "What this Bot is for".
   No help text, no tooltip, no info icon. The empty field teaches.

6. **A toggle comes with its consequence.** Their notification switch sits in a card with a sentence
   saying what it does. A bare switch labelled "Notifications" makes a person guess.

7. **Scope decides placement.** App-level settings are a modal with its own left nav. Bot-level
   settings live in the rail with the Bot. Nothing has to be learned; the location says which one
   you are changing.

8. **Expanding the computer is a focused overlay, not a second window.** The app dims behind it. It
   stays one application.

---

## 2. Where we should be plainly better

Named so the redesign aims past parity rather than at it.

- **Their empty conversation is empty.** A new channel is a blank panel with a composer. DIRECTION
  says empty states are the setup path — "No Bots yet" is a screen that creates the first Bot with a
  real brief pre-filled. B05 already says our empty pane wastes 60% of the window; the fix is not to
  match their blankness.
- **Bubbles hide the work.** A tool call rendered as prose in a chat bubble cannot be scanned,
  collapsed, copied, or timed. The run log is the whole point and it is where we win.
- **No status on their roster rows.** Rows carry a name, a timestamp and a snippet. Ours must carry
  state first: working, waiting on you, failed, idle. B01.
- **No elapsed time anywhere.** The runtime measures `elapsed_ms` per tool call and the CLI already
  renders it; `serve.rs:469` drops it before the window sees it. F-DC3.
- **Approvals.** Theirs are not visible in the captures, but DIRECTION is explicit: an inline gate at
  the point in the log where it happened, plus a persistent count in the chrome. A blocking modal
  trains people to click through, and `APPROVAL-INVARIANTS.md` already pins the rest.

---

## 3. The constraint that shapes the sequence

```
current bundle   175,391 bytes
ceiling          177,363 bytes   (baseline 154,229 + 15%)
headroom          1,972 bytes
```

**None of this work fits.** The ceiling exists to stop an unattended loop from bloating the client,
and it did its job; it is not a reason to stop shipping design work. Re-cut the baseline **once**, at
the start, deliberately, in its own commit, with the new number and the reason recorded in
`BASELINE.md` — not silently, and not repeatedly as each phase overruns.

The other standing constraints, from `CLAUDE.md`:

- Vanilla `index.html` + `main.js` + `styles.css`. No bundler, no framework, no npm, no build step.
- No new token without adding it to `DIRECTION.md` in the same commit, with a reason.
- `scripts/ux-verify.sh` gates every change: axe, contrast, keyboard, the approval invariants, and
  bundle size.
- `crates/openbotd`, `openbot-guest` and `openbot-proto` stay read-only from here. Presentation only.

---

## 4. The sequence

Six phases. Each is independently shippable, each ends green on the gate, and each is ordered so the
thing it depends on already exists.

### Phase 0 — Implement the design system that was already decided

`DIRECTION.md` was adopted as launch decision #1 and never implemented. The build ships a purple
accent DIRECTION bans and a pill radius it forbids. This is F-DC1, and it is first because every
later phase would otherwise be built against tokens that are going to move.

- Replace the shipped token layer with the eight pinned values and the derived set.
- The accent becomes a **neutral fill**. This is the load-bearing decision and it is worth restating:
  colour in this app means status and never emphasis, so a primary button is the highest-contrast
  object on screen rather than a fourth hue that means nothing.
- One radius scale, tight, no pills.
- Vendor the typefaces as WOFF2 under `ui/fonts/` with a `PROVENANCE.md` row each. No runtime fetch:
  this is an offline desktop app.
- Light theme is built in the same commit and is as good, not an inversion.

*Gate:* contrast passes in both themes. No hue on screen that does not encode state.
*Risk:* this re-baselines the contrast audit. Expected, and the reason it goes first.

### Phase 1 — The thread becomes a run log

The signature change, and the one that makes OPENBOT look like a different category of product.

- A tool step is a structured row: verb, target, result, duration. Collapsible. Copyable.
- The model's prose is one row type among several, not the frame everything else sits inside.
- Elapsed time appears, which means `serve.rs` stops dropping `elapsed_ms` (F-DC3 — a small change in
  `openbot-desktop`, the one crate outside the UI this phase touches).
- Approvals render inline at the point in the log where they happened.

*Gate:* `page.rs` asserts a tool row exposes verb, target, result and duration as distinct nodes, and
the approval invariants still hold.

### Phase 2 — The computer becomes a peer pane

- Right rail, always present, live thumbnail while a Bot is working, an honest idle state when not.
- Expands to a focused overlay over a dimmed app, not a new window.
- The rail is collapsible, and the conversation takes the space when it is.

*Gate:* B03 closed. The viewer's existing poll already tells the pane whether the computer is alive;
no new transport.

### Phase 3 — Liveness — **done, 2026-08-25**

All five items landed. F-DC9's scroll half did not reproduce and is recorded as such in
`renderRoster`; the ceiling in §3 was re-measured rather than re-cut, and now counts gzipped
bytes — `BASELINE.md` carries the reasoning and the operator's decision.


The department review's verdict was that the client was designed as a sequence of still frames. This
phase is the fourth dimension.

- Loading states on every async surface, shaped like the content that will arrive (F-DC5).
- The log stops yanking to the bottom while you are reading it (F-DC7). Follow only when already at
  the bottom.
- A dead runtime stops reading as "connected" (F-DC8). The poll exists; it is not wired to the pill.
- The roster stops stealing focus and scroll when it rebuilds at the end of every turn (F-DC9).
- The model call gets a real state: elapsed, phase, and a place on screen near the reading column
  rather than 900px away in a corner (F-DC4).

*Gate:* a `page.rs` test drives a long run and asserts the log does not scroll while a synthetic user
is scrolled up.

### Phase 4 — The rail earns its keep

- Bot settings in the rail with a back stack. Placeholders carry the instruction.
- Routine editor in the rail: Active, Delete, **Test run** across the top.
- Triggers as a named list, wired to the event model that already exists and has no surface.
- Every toggle carries the sentence that says what it does.

*Gate:* creating a routine and firing `Test run` records a run, driven through the shipped binary.

### Phase 5 — Empty states are the setup path

- "No Bots yet" creates the first Bot, with a real brief pre-filled.
- The empty conversation offers the three things this Bot can do, not a blank pane (B05).
- Every failure state states what happened, what it means, and the one action that resolves it.
  Errors do not apologise and are never vague.

*Gate:* B05 closed; the state-coverage matrix in `reports/design-client.md` has no `UNDESIGNED` cells.

### Phase 6 — The signature element

The provenance trail, which `DIRECTION.md` names as the thing nobody else in this category has.

- Every artifact in a thread traces to the step that produced it and the snapshot it landed in.
- Hovering a file, a browser action or a command surfaces its lineage.

Spend the boldness here and keep everything around it quiet. This is last because it needs the run
log (Phase 1) and the computer pane (Phase 2) to point at.

---

## 5. What this plan deliberately does not do

- **No framework.** The temptation at this scope is React. `CLAUDE.md` forbids it and the constraint
  is a good one for a client that must start instantly and ship inside a Tauri bundle. `main.js` is
  2,632 flat lines and does need structure (F-DC12) — that happens *as* these phases land, not as a
  separate rewrite commit.
- **No motion budget beyond what a state change needs.** A console that animates for personality is
  a console you stop trusting. Motion communicates hierarchy, state transition or feedback, or it
  does not ship.
- **No new colour.** Three status hues, one neutral accent. If a fourth hue appears, something has
  gone wrong upstream of the CSS.
- **Nothing from the reference's visual identity.** No colour value, spacing scale, icon, typeface or
  screen composition. The lessons in section 1 are restated in our own terms before they become work,
  and if a proposed change can only be justified as "that is how they do it", it is rejected.
