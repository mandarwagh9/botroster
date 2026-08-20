# REPORT — OPENBOT UX loop

Branch `ux/overnight-0820`, cut from `ux/overnight-2026-08-20`. Nothing merged.

Two runs. **Run 1** (overnight, 02:50–06:05) built the harness and cleared four defects.
**Run 2** (journey-first, from 09:50) is this report's subject. Read this before the diff,
then run the app for ten minutes before merging anything.

---

## 1. Launch to first work

**Before: there was no path.** Not a large number — none. Verified against the installed
binary at `%LOCALAPPDATA%\OPENBOT\openbot.exe`, not inferred: `~/.openbot` has `bots/` and
`volumes/` but no `config.toml`, so `openbot acp` exits 1. With a config but no key it still
exits 1. Neither the model nor the key is settable anywhere in the window — `Settings` is
`#rules-btn`, inside `#workspace`, which is hidden until the connect that is failing
succeeds. The README's stated escape hatch ("open Settings") describes a surface that does
not exist.

Counted honestly, the old path was **10 actions, 4 of them outside the application, 2 of them
in a terminal**, requiring prior knowledge of a command the app never mentions.

**After — the demo path: 2 actions, no key, no terminal.**

| | Action | Elapsed |
|---|---|---|
| 1 | Connect | ~2s, fails, and now says why |
| 2 | **Watch the demo instead** | a Bot runs real tools against a real workspace |

The demo needs no model and no key, which the onboarding doctrine calls the strongest asset
in the product; it was reachable only from a CLI flag.

**After — the real-model path: 4 actions, and the terminal is gone.**

| | Action |
|---|---|
| 1 | Connect — fails, and opens the Model section it needs |
| 2 | type the model id |
| 3 | type the API key |
| 4 | Connect |

**From 10 actions with 4 outside the application and 2 in a terminal, to 4 inside the
window.** Both paths are now inside the 5-action and 90-second targets.

The Model section sits on the connect panel because that is the only surface that exists
before a connection: `Settings` lives inside `#workspace`, which is hidden until the connect
that needs a model succeeds. That circularity *was* the deadlock.

The key is not written anywhere. `config.toml` records the *name* of an environment variable
and never the value, so the window passes the key to the agent process it spawns and it lives
no longer than the connection. `a_key_from_the_window_reaches_the_agent_and_is_never_written_down`
checks all three parts: without it the agent refuses and names the variable, with it the
agent starts, and the file never contains the value.

---

## 2. Rubric, per line

| # | Line | 000 | run 1 | run 2 |
|---|---|---|---|---|
| 1 | cold start unambiguous | 2 | 2 | 2 |
| 2 | four roster statuses in <1s | 0 | 0 | 0 |
| 3 | one loudest element per screen | 1 | 2 | 2 |
| 4 | doing-now vs already-did | 2 | 3 | 3 |
| 5 | steps state their target | 3 | 3 | 3 |
| 6 | computer visible without interaction | 0 | 0 | 0 |
| 7 | approval: safe choice louder | 0 | 0 | 0 |
| 8 | denial reads as final | 2 | 2 | 2 |
| 9 | find last approval in 200 steps | 0 | 1 | 1 |
| 10 | which Bot produced which step | 1 | 2 | 2 |
| 11 | takeover obvious | 0 | 0 | 0 |
| 12 | failure names cause + one action | 1 | 1 | **2** |
| 13 | three routine states | 0 | 0 | 0 |
| 14 | empty state is a path | 1 | 3 | 3 |
| 15 | provenance trail | 0 | 0 | 0 |
| 16 | light equal to dark | 2 | 2 | 2 |
| 17 | nothing looks library-default | 2 | 2 | 2 |
| 18 | amber only where a human blocks | **0** | 3 | 3 |
| 19 | recognisable as the reference | 3 | 3 | 3 |
| 20 | Chanel test | 1 | 2 | 2 |
| | **Total** | **21** | **31** | **32** |

(`000` line 18 is corrected from the 3 the baseline gave itself; two Bots-worth of amber were
in the coat palette and the busy pill.)

**The rubric moved one point for the most important fix in either run, and that is a fact
about the rubric.** Not one of its twenty lines covers J1 or J2. It was written against the
twelve steady-state scenarios, every one of which asserts `connected = true`, so an app that
cannot be started at all scores the same as one that can. Any future run should add lines for
launch and connect before trusting this total as a measure of onboarding.

---

## 3. Commits

| Run | Commit | Stage | What | Delta |
|---|---|---|---|---|
| 1 | `ae23db5` | — | the harness: fixture, shots, gates, baseline | — |
| 1 | `53c6bde` | J4 | `.step-state` legible; run log keyboard-reachable; rules `select` named | 4: 2→3 |
| 1 | `ae0c5f3` | J6 | no Bot wears amber; `--coat-4` olive | 18: 0→3, 10: 1→2 |
| 1 | `12e3b21` | J3 | empty conversation says something true, and offers the way out | 14: 1→3 |
| 1 | `f726bf5` | J6 | transport pill quiet; working state off amber | 3: 1→2, 20: 1→2 |
| 2 | `3665cb8` | **J2** | the window stops hiding what the runtime told it | 12 (WHAT+WHY) |
| 2 | `8286b2d` | **J2** | a failed connect says what, why, what is safe, and one thing to do | 12: →3 for connect |
| 2 | `bfdc79a` | **J7** | a step nobody heard back from says so instead of spinning forever | 12: 1→2 |

---

## 4. Changed scenarios, before / after

Shots are gitignored; paths are on disk.

| Scenario | Before | After | What changed |
|---|---|---|---|
| `s02` binary missing | `shots/011/s02-*` | `shots/013/s02-*` | structured error; `found()`'s verbatim wording replaces a paraphrase |
| `s13` no model **(new)** | — | `shots/013/s13-*` | the state every fresh install hits; did not exist before run 2 |
| `s11` guest drops | `shots/011/s11-*` | `shots/013/s11-*` | abandoned step now states it may or may not have run |
| `s05` mid-run | `shots/000/s05-*` | `shots/013/s05-*` | run 1: "running" legible at last |
| `s03`/`s04` roster | `shots/000/*` | `shots/013/*` | run 1: coats, empty state, transport pill |

Each in `1280-dark`, `1280-light`, `1600-dark`, `1600-light`.

---

## 5. Every error state against the four doctrine elements

WHAT / WHY / WHAT IS SAFE / ONE ACTION.

| State | WHAT | WHY | SAFE | ACTION | |
|---|---|---|---|---|---|
| `s13` no model configured | ✓ | ✓ | ✓ | ✓ demo | **4/4** |
| key configured but not set | ✓ | ✓ | ✓ | ✓ demo | **4/4** — same path, no scenario yet |
| `s02` runtime binary missing | ✓ | ✓ | ✓ | ✗ | 3/4 — the `…` picker resolves it but is not offered as *the* action. Demo is correctly withheld: with no runtime it cannot run |
| `s07` denied by policy | ✓ | ✓ | ✓ | n/a | **pass** — reads as final, no control implies appeal |
| `s11` abandoned tool step | ✓ | ✓ | ✓ | n/a | **pass** — a step is not an action surface |
| `s11` status pill | ✗ | ✗ | ✗ | ✗ | **P0 (B15)** — raw error string as the whole message, in the chrome, 90 characters, no expander |
| `s11` budget exhausted | ~ | ✓ | ✗ | ✗ | **P1 (B16)** — rendered as a crash, not a decision point; no raise-it control exists |
| `s11` "No computer" banner | ✗ | ~ | ✗ | ✗ | **P1 (B17)** — written for connect-time, reused for a mid-run drop where it is wrong; "Dismiss" resolves nothing |
| connector 401 | — | — | — | — | **no rendering path found anywhere** |

---

## 6. NEEDS REVIEW — yours to decide

- **B08** — approvals are a blocking centred modal, not inline gates. DIRECTION calls the
  modal "the lazy answer" that "trains people to click through". Past the 200-line ceiling
  and entangled with the approval queue `page.rs` pins in about a dozen tests. The largest
  UX item in the file.
- **B02** — the grant is the only accent fill in the approval dialog. Real against rubric 7,
  but `renderDialog` carries a written rationale for the current arrangement (one accent per
  dialog; narrowest grant first; positional so an unclassifiable future `kind` cannot be
  dressed in the accent). Not a change to make unattended on a security dialog.
- **B09** — `APPROVAL-INVARIANTS.md` invariant 4 says Escape resolves to refuse; the shipped
  behaviour is that Escape does nothing, pinned by
  `escape_closes_a_panel_but_never_an_approval`. Both fail closed.
- **B07** — a routine can only be `enabled` or paused. Rubric 13 asks for three states over a
  model carrying two; the third needs the runtime to report it.
- **B16** — raising a token budget from the window is a new surface, not a patch.

---

## 7. Gates vs BASELINE

| Gate | 000 | now |
|---|---|---|
| axe serious | **4** | **0** |
| axe critical | **2** | **0** |
| contrast failures | **2** | **0** |
| worst contrast | **2.79:1** | none |
| keyboard unreachable | 0 | 0 |
| reduced-motion violations | 0 | 0 |
| approval invariants | pass | pass |
| `cargo test --test page` | 57 | 57 |
| `engine_live` | 12 | **13** |
| bundle | 135,667 B | **147,214 B** (+8.5%, ceiling 156,017) |
| shots | 48 in 4.0s | **52 in 3.8s** |

**Bundle headroom is down to about six points.** The next surface that lands will need the
baseline re-cut or the ceiling revisited; the gate will otherwise revert a legitimate change.

---

## 8. Three things to do with another six hours

1. **B15, then a model surface on the connect panel.** B15 is small and is a straight
   doctrine violation — the header shows a raw error string as the whole message while the
   panel three inches below it now does the right thing. Then the real fix for section 1:
   the connect panel is the only screen that exists before a connection, so it is the only
   place a model and a key can be set. That is what turns the real-model path from 10 actions
   and a terminal into something comparable to the demo path's 2.
2. **Decide B08 and build it.** Inline approval gates plus a persistent waiting-on-you count
   move rubric 2, 7 and 9 together, and J5 — the moment the whole product is explained — is
   currently three buttons that never mention that the gate is in the hub where the agent
   cannot reach it.
3. **Give the roster a status, and the rubric some lines for J1/J2.** Status is DIRECTION's
   first consequence and scores 0; the window can already derive *open*, *has a pending
   approval* and *paused*, and only *working* needs the engine. And the measurement instrument
   itself needs fixing, per section 2.

---

## Notes on how this ran

**`reference/` was empty.** The Grok Bot captures were never dropped in, so neither run had
any interaction lessons from them. Nothing in either report was informed by the reference.

**The fixture was structurally blind to J1 and J2**, because every scenario from `s03` on
asserts `connected = true`. That is why run 1 cleared four real defects without noticing the
app could not start. `s13` and a corrected `s02` close it. **J8 (RETURN) is still not
covered** — there is no unread marker, no routine-fired digest, no since-last-visit boundary,
so there is nothing to photograph. Product gap and harness gap at once.

**A wrong fixture invents defects.** Run 1 filed a P0 against an approval dialog the product
never renders, because `danger` was on the wrong option. Run 2's rule: derive every payload
from the code that consumes it, and drive the real path rather than painting the end state —
`s11` sends a message that rejects, so the logic under test actually runs.

**Assert on the cause, not on the message changing.** The first attempt at the J2 fix — await
the task's error — yields `Incoming transport closed`, because the transport error beats the
child-exit report that carries stderr. A test asserting "not the old string" would have
passed and shipped a window telling somebody with an unset API key about a transport.

**The preflight gate blocked the run for something harmless** — it matched any process named
`openbot*`, so the installed app being open failed everything. A gate that stops the run for
something harmless is a gate that gets switched off.

**This branch has other writers.** `104dc9a` and `e478fe8` are not from either loop. Do not
revert with a blanket `git checkout .`.
