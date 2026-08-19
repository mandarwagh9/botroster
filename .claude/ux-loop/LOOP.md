# OPENBOT overnight UX loop

Phase 0 built the harness. Phase 1 is the unattended loop. `DIRECTION.md` and `RUBRIC.md`
are separate files in this directory and are the authority; this file is the procedure.

---

## Decisions taken at launch (2026-08-20, ~03:00)

Three questions were resolved before the operator slept. The loop follows these, not its own
judgement:

1. **Full re-skin, as written.** DIRECTION.md's eight pinned colours and three typefaces
   replace the shipped token layer. The cost was flagged and accepted: the shipped system was
   contrast-audited and the re-skin re-baselines it. To stop an unattended iteration from
   inventing a token at 4am, the gaps DIRECTION.md left — accent, Bot coats, the remaining
   text tiers, shape, depth, light theme — were **derived and written into DIRECTION.md
   before launch** and are marked DERIVED there. The load-bearing derivation: *the accent is
   neutral, because colour in this app means status and never emphasis.*
2. **Escape stays inert on approvals.** Invariant #4 in section 3 below says Escape and
   click-outside resolve to refuse. The shipped behaviour is that Escape never closes an
   approval at all, pinned by `crates/openbot-app/tests/page.rs`
   (`escape_closes_a_panel_but_never_an_approval`). Both fail closed. The loop **does not
   change this** and does not touch that test; the divergence is filed in BACKLOG.md as
   NEEDS REVIEW.
3. **`reference/` and `shots/` are gitignored.** Origin is public and this project's whole
   position is that it contains none of the reference product's material. Text artefacts
   (this file, DIRECTION, RUBRIC, BACKLOG, MEMORY, BASELINE, REPORT) are committed; images
   are not. REPORT.md points at shot paths on disk.

---

## The three things that decide the outcome

1. **A screenshot harness.** Without it the loop is blind and every iteration is a guess.
   Built in Phase 0; see `HARNESS.md`.
2. **Hard gates.** Build, a11y, contrast, keyboard, and the approval invariants. A change
   that fails a gate gets reverted, not argued with.
3. **A pinned design direction.** An unattended loop told to "make it nice" converges on the
   generic AI look. `DIRECTION.md` pins it.

---

## 1. What the reference is for, and what it is not

The loop reads `reference/` for **interaction lessons only**:

- Where does the eye land first on each screen
- How many actions from launch to first useful result
- What is progressive-disclosed vs always visible
- How state changes are announced
- Where the product spends its motion budget
- What it refuses to show you

Hard rule:

> Never copy visual identity. No logos, icons, wordmarks, illustrations, fonts, exact color
> values, exact spacing scales, or animation curves lifted from the reference. No screen that
> would be recognised as the reference with a different name on it. Extract the principle,
> then solve it in OPENBOT's own language as defined in DIRECTION.md. If a proposed change
> can only be justified as "that is how they do it," reject it.

You are not shipping a cheaper Grok Bot. A clone that looks worse is the weakest possible
position. OPENBOT has a thesis Grok Bot cannot claim: **it is open, self-hosted, and the gate
is in the hub where the agent cannot reach it.** The UI should be the argument for that.

---

## 2. Direction

See `DIRECTION.md`. It is the only source for colour, type, shape, depth and motion. No new
token without adding it to that file in the same commit, with a reason.

---

## 3. Approval invariants (non-negotiable, enforced by test)

The security story lives in this dialog. An unattended loop restyling it can quietly break
it. These run in `scripts/ux-verify.sh`:

- The affirmative action is **never** the default-focused element on mount.
- "Allow for the session" is visually distinct from "Allow once" and is never the primary.
- The exact tool name and the full arguments are rendered before any choice is reachable.
  No truncation without an expand affordance that is in the tab order.
- Escape and click-outside resolve to **refuse**, never to allow, never to
  dismiss-and-continue. — **AMENDED AT LAUNCH, see Decision 2: shipped behaviour is that
  Escape does nothing to an approval, and that is what the gate asserts tonight.**
- A `deny` from policy renders as unappealable. No control in the UI suggests otherwise.
- Nothing auto-resolves on timeout. No countdown. No "remember my choice" that spans sessions.
- The gate is not skippable by keyboard mashing: a single Enter on mount cannot approve.

`crates/openbotd`, `crates/openbot-guest`, and `crates/openbot-proto` are **read-only**.
Presentation only. If a UX improvement requires changing what gets gated, it goes in
BACKLOG.md for the operator to decide, not into a commit.

**The generic keyboard and a11y gates carve the approval dialog out explicitly.** "Escape
closes anything dismissible" and "no focus trap outside intentional modals" both point
straight at a dialog that is deliberately non-dismissible and deliberately traps focus.
Without the carve-out the loop spends the night "fixing" the property the gates exist to
protect.

---

## 4. The loop

Runs until 06:30 or a stop condition. Branch `ux/overnight-2026-08-20`. Never commit to main.

**WRITABLE:** `crates/openbot-app/**`, `crates/openbot-desktop/**` (presentation layer only),
`scripts/ux-*`, `.claude/ux-loop/**`
**READ-ONLY:** everything else. `crates/openbotd`, `crates/openbot-guest`,
`crates/openbot-proto` are off limits without exception.

Read before starting: `DIRECTION.md`, `RUBRIC.md`, `MEMORY.md`, `BACKLOG.md`, `BASELINE.md`,
and the images in `reference/` (interaction lessons only, never visual identity).

### Each iteration

**STEP 1 — PICK ONE.** The single highest-severity open item from BACKLOG.md. One item. One
surface. Not "improve the roster" — something like "the roster does not distinguish
waiting-on-you from working at a glance." If BACKLOG is empty, go to STEP 2 to generate more.

**STEP 2 — CRITIQUE** (fresh subagent, no memory of authoring anything). Give it
DIRECTION.md, RUBRIC.md, and the current iteration's screenshots. Do not tell it who made
them. Do not give it the diff. Its only job:

> Score each rubric line 0-3 with a one-sentence justification tied to something visible in a
> specific screenshot. Then output a JSON defect list:
> `[{ "scenario": "s06", "surface": "approval gate", "defect": "...", "severity": "P0|P1|P2",
> "rubric_line": 7, "suggested_fix": "...", "evidence": "s06-1280-dark.png" }]`
>
> A defect you cannot point at in a screenshot is not a defect. Do not write taste opinions.
> Do not suggest adding features. Severity: P0 breaks a task, P1 makes a task slow or
> ambiguous, P2 is polish.

Append new defects to BACKLOG.md, deduplicated.

**STEP 3 — PATCH.** Implement the fix for the one item picked in STEP 1.
- Derive every colour, size, and type decision from DIRECTION.md. No new tokens without
  adding them to DIRECTION.md and saying why.
- No new dependencies. No new npm packages. No CSS frameworks.
- Do not touch a surface unrelated to the item.
- If the fix requires a structural change bigger than ~200 lines, write the plan into
  BACKLOG.md as a P1 with "NEEDS REVIEW" and pick the next item instead.

**STEP 4 — VERIFY.** Run `scripts/ux-verify.sh`, then `scripts/ux-shots.mjs`.
- Any gate fails: `git checkout .`, append the failure and the reason to MEMORY.md, move on.
  Do not attempt more than two fixes on a failing gate before reverting.
- Gates pass: re-run the STEP 2 critic on the new shots for the touched scenarios only. Net
  rubric score must be equal or higher and the targeted defect must be gone.
- Score dropped: revert, log why in MEMORY.md.
- Score improved: commit with a message naming the rubric line and the delta. Mark the
  BACKLOG item done.

**STEP 5 — REMEMBER.** Append to MEMORY.md: what you tried, whether it held, and the
generalisable lesson. Read this file at the top of every iteration. It is the only thing that
makes the loop compound instead of random-walk.

### Stop conditions
- No open P0 or P1 in BACKLOG, or
- Three consecutive iterations with no net rubric gain, or
- 06:30, or
- Five consecutive failed gates (something is structurally wrong, stop and write it up).

### On stop
Write `REPORT.md`: rubric scores at iteration 000 vs final, every commit with its delta,
before/after shot pairs for each changed scenario, everything in BACKLOG marked NEEDS REVIEW,
every gate number vs BASELINE, and the three things to do next with another six hours. Push
the branch. Do not merge.

---

## 5. What is actually true at 08:00

**Will be genuinely better:** information hierarchy, status legibility, empty and error
states, contrast and keyboard access, copy, spacing consistency, the twelve states existing
at all.

**Will be mediocre:** motion. A screenshot cannot feel the app. Anything about latency,
scroll feel, input responsiveness, or how a transition lands is invisible to it. Budget an
hour for motion and feel by hand, with the loop's work as the foundation.

**Realistic throughput:** Phase 0 ran from ~02:50. That leaves roughly three hours, and every
iteration carries a `cargo check` and `clippy` on a Tauri crate, a shot run, and two critic
subagent calls. **Expect 8-15 committed iterations, not 15-30.** A REPORT.md showing eleven
solid commits is the good outcome; forty commits with flat rubric scores is the bad one.

**The failure mode to watch:** if the rubric scores are flat across many commits, the critic
went soft and started rewarding change instead of improvement. Fix by tightening the rubric
lines that drifted, not by adding iterations.

Read REPORT.md before the diff. Then run the app for ten minutes before merging anything.
