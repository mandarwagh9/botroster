# REPORT — overnight UX loop, 2026-08-20

Branch `ux/overnight-2026-08-20`. Phase 0 ran ~02:50–04:15; the loop ran ~04:15–06:00.
**Five commits, four of them UX changes, all gates green.** Nothing merged.

Read this before the diff. Then run the app for ten minutes before merging anything.

---

## The number

**Rubric 21/60 → 31/60.**

The baseline in `BASELINE.md` says 24. It was wrong by three: line 18 (*is amber present
anywhere nothing requires a human*) was scored 3 by looking for stray amber in the chrome,
when two Bots-worth of amber were sitting in the coat palette and the "busy" status pill.
Corrected to 0, the true starting total is 21. **The loop's first real finding was that its
own baseline had over-scored itself**, which is worth more than the ten points.

---

## Commits

| | Commit | What | Rubric |
|---|---|---|---|
| 000 | `ae23db5` | the harness — fixture, shots, gates, baseline. No UI change. | — |
| 001 | `53c6bde` | `.step-state` legible, run log keyboard-reachable, rules `select` named | 4: 2→3 |
| 002 | `ae0c5f3` | no Bot wears amber; `--coat-4` olive | 18: 0→3, 10: 1→2 |
| 003 | `12e3b21` | empty conversation says something true and offers the way out | 14: 1→3 |
| 004 | `e85562c` | transport pill quiet; working state off amber | 3: 1→2, 20: 1→2 |

Line 9 also moved 0→1: the log taking focus makes a 200-step thread scrollable from the
keyboard, which is most of the way to "find the last approval" without being a marker.

**No iteration was reverted, and no gate failed after 001.** That is a smaller claim than it
looks — see *What this did not do*.

---

## Gates: baseline vs final

| Gate | 000 | final |
|---|---|---|
| axe serious | **4** | **0** |
| axe critical | **2** | **0** |
| contrast failures | **2** | **0** |
| worst contrast | **2.79:1** | no failures |
| keyboard unreachable | 0 | 0 |
| reduced-motion violations | 0 | 0 |
| approval invariants | pass | pass |
| `cargo test --test page` | 57 passed | 57 passed |
| bundle | 135,667 B | ~139,300 B (ceiling 156,017) |
| shot run | 4.0s / 48 | ~5s / 48 |

The baseline did not pass its own gates. Six of the eight failures were real defects in the
shipped product, and 001 cleared all of them:

1. **`.step-state` at 2.79:1.** The word "running" — the answer to *what is the Bot doing
   right now* — was the least legible text on the screen, set in `--ghost`, a tier the design
   system documents as not clearing AA and reserves for placeholders.
2. **The run log scrolled but could not take focus.** Past the last screenful, a 200-step
   thread was unreachable without a pointer.
3. **The `select` that decides what the hub allows had no accessible name.**

---

## The three things worth your attention

### 1. The fixture was wrong, and it invented a defect

The first backlog item — "the approval dialog makes the grant the loudest thing" — was filed
against a screenshot of a dialog **the product never renders**. The fixture had `danger: true`
on the session grant; `renderDialog` styles on that flag, so the harness painted the large
grant in refusal styling and the refusal in quiet styling.

It was caught by reading the option-construction loop in `main.js` before patching, not by
looking harder at the picture. Corrected in `shots/001/` onward.

**This is the loop's main structural risk and it fired on iteration one.** A screenshot is
only as true as the fixture behind it, and a wrong fixture produces confident, well-evidenced,
entirely fictional defects. If you extend the scenarios, derive every payload from the Rust
type or the code that consumes it.

### 2. Amber was misused twice, and DIRECTION is what caught it

DIRECTION says amber means *a person is blocking progress*, and rubric 18 makes any other
amber an automatic P0. Two violations, neither in the chrome where the baseline looked:

- `--coat-4` was `#f19d38`. A Bot idly assigned that coat raised the waiting-on-you signal
  with nobody waiting. It was also the same hue as `--coat-3`, so two Bots read as one colour.
- `.status.busy` was `--warn` **and pulsed**, making a Bot quietly doing its job the loudest
  thing on screen, in the colour that means it needs you.

Having found the first, grepping every other use of `--warn` found the second in about a
minute. One misuse of a semantic token predicts others.

### 3. Four items are NEEDS REVIEW and were deliberately not touched

These are in `BACKLOG.md` with full reasoning. All four are yours to decide:

- **B08 — approvals are a blocking centred modal, not an inline gate.** DIRECTION calls the
  modal "the lazy answer" that "trains people to click through". This is the single largest UX
  item in the file. It is well past the 200-line ceiling and it interacts with the approval
  queue that `page.rs` pins in about a dozen tests. Not something to attempt unattended.
- **B02 — the grant is the only accent fill in the approval dialog.** Real against rubric 7,
  but `renderDialog` carries an explicit written rationale for the current arrangement (one
  accent per dialog; the shell orders options narrowest-grant-first; positional on purpose so
  an unclassifiable future `kind` cannot be dressed in the accent). Inverting emphasis on a
  security dialog against a written in-code decision is not a 4am change.
- **B07 / s12 — a routine cannot express "erroring".** The payload has `enabled` and nothing
  else. Rubric 13 asks for three distinguishable states over a model carrying two. Fixing it
  means changing what the runtime reports, which was outside tonight's writable set.
- **B09 — invariant 4 contradicts shipped behaviour.** LOOP.md says Escape resolves to refuse;
  the shipped behaviour is that Escape does nothing to an approval, pinned by
  `escape_closes_a_panel_but_never_an_approval`. Both fail closed. Decided at launch to keep
  shipped and not touch the test.

---

## What this did not do

**The product's whole argument still scores zero.** Every rubric line that DIRECTION treats as
the differentiator is untouched:

| Line | | Score |
|---|---|---|
| 2 | roster status board | **0** |
| 6 | computer as a peer pane | **0** |
| 11 | takeover | **0** |
| 13 | three routine states | **0** |
| 15 | the provenance trail | **0** |

Tonight's four commits were legibility, accessibility, colour semantics and copy — real, and
they make the surface honest, but they are the floor, not the thesis. **A reader of this
report should not conclude the UI now argues for open, self-hosted, hub-enforced gating. It
does not. It is merely no longer inaccessible or lying in its empty states.**

Three of those five are blocked on data the window does not have (`roster` carries no status;
routines carry no error; takeover has no window-side surface), which is why B01 is scoped and
B07 is NEEDS REVIEW rather than done.

**Motion, latency, scroll feel and input responsiveness were not assessed at all.** A
screenshot cannot see them. Budget an hour by hand.

**The full re-skin did not happen.** You chose it over my recommendation, and I built for it:
`DIRECTION.md` now carries the complete token system, with the gaps it left — accent, the
eight Bot coats, the remaining text tiers, shape, depth, and the whole light theme — derived
and marked `DERIVED`, so an unattended iteration had something to reach for instead of
inventing one. The load-bearing derivation is **the accent is neutral, because colour in this
app means status and never emphasis**; every remaining hue was already spoken for, and a
fourth would have put a colour on screen carrying no state.

But swapping the token layer is a >200-line change, which STEP 3 sends to NEEDS REVIEW by
construction, and it would re-baseline every measured contrast ratio in a stylesheet whose
comments record them. With ~1h45m of loop time after Phase 0 and an eight-minute gate cycle,
starting it would have meant leaving a half-applied palette at 06:30. **The derived system is
ready; applying it is the first thing to do awake.** The three typefaces are not vendored —
that needs font files, PROVENANCE.md rows, and a licence check per family.

---

## Three things to do next with another six hours

1. **Apply the re-skin, in one sitting, with the gate running.** `DIRECTION.md` is complete
   now. Do it as one commit against a re-baselined contrast measurement, not incrementally —
   a half-swapped palette is worse than either end state. Vendor Geist Sans, Commit Mono and
   Martian Mono with their PROVENANCE rows first.
2. **Decide B08.** Inline approval gates plus a persistent waiting-on-you count is the change
   that would move rubric 2, 7 and 9 together, and it is the one DIRECTION argues hardest for.
   It needs a person because it touches the approval queue.
3. **Give the roster a status.** It is the first consequence in DIRECTION and it scores 0. The
   window can already derive *open*, *has a pending approval*, and *paused*; *working* needs
   something from the engine. Scoping that is a design decision, not a patch.

---

## Harness notes

`HARNESS.md` documents it. Three things a future run should know:

- **The gate cycle is ~8 minutes and dominates everything.** Shots are 4-5s for all 48. The
  cost is `cargo check` + `clippy` + the 57-test page suite. Start the gate in the background
  and read for the next iteration while it runs.
- **This branch has another writer.** `104dc9a` is not from this loop — it fixed README's
  stale `./openbot-data` default and swept in the `page.rs`/stub extraction, and its follow-up
  edits to `README.md` and `CLAUDE.md` landed inside `ae23db5` rather than their own commit.
  The edits are correct; the attribution is not. **Do not revert with a blanket
  `git checkout .`** — it would discard another writer's work.
- **`sh scripts/ux-verify.sh | tail` returns `tail`'s exit code**, not the gate's. Redirect to
  a file and check `$?`.
