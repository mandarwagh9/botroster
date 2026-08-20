# BACKLOG

Ordered by severity, then by how much of the product the surface carries. Every item names
the screenshot it can be seen in. An item with no evidence file is not an item.

Severity: **P0** breaks a task. **P1** makes a task slow or ambiguous. **P2** is polish.

Status: `open` / `doing` / `done <commit>` / `NEEDS REVIEW` / `reverted <reason>`.

---

## P0

### B01 — the roster carries no status at all
`open` · rubric 2 · `shots/000/s04-1280-dark.png`

Four Bots render as four identical rows: coat mark, name, subtitle. There is no idle,
working, waiting-on-you or paused-by-the-brake anywhere — not in the payload `renderRoster`
receives, not in the DOM, not in the chrome. DIRECTION's first consequence is "the roster is
a status board first, a contact list second. Status is the loudest thing on each row." Today
status does not exist, so the rubric line asking to tell four states apart in under a second
scores 0.

Note this is partly a data gap: `roster` returns `{id, name, title, description, hidden,
messages}`. Presentation can render a status it is given, but something has to give it one.
`crates/openbot-desktop` is writable presentation-layer; `openbotd` is not. Scope the fix to
what the window can already know (open session, pending approval count, routine paused) and
file the rest.

### B02 — the approval dialog makes the grant the loudest thing
`NEEDS REVIEW` · rubric 7 · `shots/001/s06-1280-dark.png`

**The 000 screenshot this was first raised from was wrong** — the fixture had `danger` on the
session grant instead of on the refusal, so it painted a dialog the product never renders.
Corrected in `shots/001/`. The true state: "Allow once" is the accent fill (loudest), "Allow
for the rest of this session" is a quiet outline, "Not this time" is a red outline.

Invariant 2 holds: the session option is not primary and is visually distinct. What does not
hold is rubric 7's stronger question — is the *consequential* choice subordinate to the safe
one? No: the grant carries the only accent fill in the dialog.

**Not changed unattended, and this is deliberate.** `renderDialog` in `main.js` carries an
explicit rationale for the current arrangement: one accent per dialog, the shell orders
options narrowest-grant-first, and the first non-refusing option gets the accent because it
is the smallest grant — positional on purpose, so an unclassifiable `kind` ACP adds later
cannot be dressed in the accent. Inverting the emphasis on a security dialog, against a
written in-code decision, is exactly the change LOOP.md says goes to the operator rather than
into a 4am commit. **Operator decision.**

### B03 — the computer is behind a button, not a peer pane
`open` · rubric 6 · `shots/000/s04-1280-dark.png`, `s05-1280-dark.png`

"Agent Computer" is a text button in the top-right chrome; the pane is `hidden` until
clicked. DIRECTION: "The computer is not hidden behind a button. It is a peer pane. It is the
differentiator; burying it throws the differentiator away." Rubric 6 scores 0 by construction
today.

### B04 — no "waiting on you" count anywhere in the chrome
`open` · rubric 2, 9 · `shots/000/s05-1280-dark.png`

DIRECTION asks for "a persistent 'waiting on you' count in the chrome". There is none. With
the dialog closed there is no way to know an approval is outstanding, and in a 200-step
thread (s08) no way to find the last one you answered.

---

## P1

### B05 — the empty conversation pane wastes about 60% of the window
`open` · rubric 3, 14 · `shots/000/s04-1280-dark.png`

With a roster populated and nothing open, the right pane is one centred paragraph in a very
large void. The copy is good ("A teammate, not a chat."), but rubric 14 wants an empty state
that is a path to the thing being empty. It offers no action; the only way forward is the
sidebar.

### B06 — two of the eight Bot coats are the same hue
`open` · rubric 2, 17 · `shots/000/s04-1280-dark.png`

Talent Scout and Support Triage both wear near-identical orange. Coats are the only
per-Bot identity in the roster, so two Bots reading as the same colour defeats the mechanism.
DIRECTION's DERIVED coat set fixes this by construction (eight hues, none within 20 degrees
of another or of the status hues) but the shipped set is not that set yet.

### B07 — the routine model cannot express "erroring"
`NEEDS REVIEW` · rubric 13 · `shots/000/s12-1280-dark.png`

A routine reports `{bot, bot_name, id, enabled, trigger, next}`. Two states are expressible,
enabled and paused. Rubric 13 asks for three distinguishable states, and the third — a
routine that is failing every night — has nowhere to come from. Adding it means touching what
the runtime reports, which is outside tonight's writable set. **Operator decision.**

### B08 — approvals are a blocking centred modal, not an inline gate
`NEEDS REVIEW` · rubric 7 · `shots/000/s06-1280-dark.png`

DIRECTION: "Approvals are inline gates at the point in the log where they happened... A
blocking modal is the lazy answer and it trains people to click through." Moving the gate
inline is a structural change well past the ~200-line ceiling in STEP 3, and it interacts
with the approval-queue logic that `page.rs` pins in about a dozen tests. **Operator
decision** — this is the single largest UX item in the file and should not be attempted
unattended.

### B09 — invariant 4 diverges from shipped behaviour
`NEEDS REVIEW` · invariant 4

LOOP.md section 3 says Escape and click-outside resolve to refuse. The shipped behaviour is
that Escape does nothing to an approval, pinned by
`crates/openbot-app/tests/page.rs::escape_closes_a_panel_but_never_an_approval`. Both fail
closed. Decided at launch: keep shipped, do not touch the test. **Operator decision** on
which is actually wanted.

---

## P2

### B10 — the status pill reads "connected" and nothing else
`open` · rubric 3, 4 · `shots/000/s05-1280-dark.png`

The only always-visible state in the chrome is a green "connected" pill about the transport.
It is the least interesting fact on the screen and it is styled as the most prominent
non-button element in the header.

---

## Gaps in the harness (not defects in the product)

- **s04 statuses** — unreachable; the model has no status field. See B01.
- **s07 denied-by-policy** — partial. A hub `deny` never reaches the window as an ask; the
  refusal arrives as a result row in the thread, which is what the fixture shows. There is no
  client-side "denied by policy" dialog to photograph.
- **s10 computer viewer** — partial. The pane is an `<iframe>` whose `src` comes from
  `open_computer` and is painted by the hub's viewer; with no hub there is nothing inside the
  frame. Human takeover is a hub-enforced lock with no window-side surface at all today.
- **s12 erroring routine** — unreachable. See B07.

---

# THE JOURNEY — analysis, 2026-08-20 (run 2, journey-first)

Evidence is `shots/010/` unless stated. J1 and J2 are **not** evidenced from the fixture —
see the blindness note at the end, which is the most important line in this section.

## The headline number

**Launch to a Bot producing visible work: unreachable on a fresh install.**

Not a large number. Not a bad number. There is no path. Verified against the installed
binary at `C:\Users\Mandar\AppData\Local\OPENBOT\openbot.exe`, not inferred:

```
$ openbot.exe acp --home "%USERPROFILE%\.openbot"
Error: no usable model: no model configured.          exit 1
```

`~/.openbot` has `bots/` and `volumes/` but no `config.toml`. With a config but no key it
still exits 1 (`$XAI_API_KEY is not set`). With both it exits 0 and the handshake path is
clear. So J2 needs **two** things a fresh install does not have, and **neither is settable
anywhere in the window**: `Settings` is `#rules-btn`, it lives inside `#workspace`, and
`#workspace` is hidden until the connect that is failing succeeds. `main.js` contains no
model surface at all.

Counted honestly, the current path to first work is:

| | Action | Where |
|---|---|---|
| 1 | find the runtime binary | file explorer |
| 2 | `openbot config set --model … --api-key-env …` | **a terminal, outside the app** |
| 3 | `setx XAI_API_KEY …` | **a terminal, outside the app** |
| 4 | restart the app so it inherits the variable | outside the app |
| 5 | Connect | J2 |
| 6 | New Bot | J3 |
| 7 | type a name | J3 |
| 8 | submit | J3 |
| 9 | type a task | J4 |
| 10 | Send | J4 |

**10 actions, 4 of them outside the application, 2 of them requiring a terminal and prior
knowledge of a command the app never mentions.** Against a target of 5 actions and 90
seconds. The README's stated escape hatch — *"To point a Bot at a real model, open
Settings"* — describes a surface that does not exist.

Steady-state, with config already present, it is **6 actions** (5→10 above). That is the
number every J1–J5 change must not increase.

---

## J1 LAUNCH — `s01`

- **Job:** orient, and get to a working state.
- **Actions to complete:** 1 (Connect) — but see J2; it does not complete.
- **Eye lands on:** the OPENBOT mark and wordmark, then three path fields. The fields are
  the visual weight of the screen.
- **Will try and cannot:** set a model or a key; run the demo; find out what a Bot is
  before committing. Nothing on this frame mentions that a model is required.
- **Says why if stuck:** no.
- **Unsigned-binary moment:** not acknowledged anywhere. The `.sha256` files exist on the
  release page and the first frame does not mention them.

## J2 CONNECT — `s02`

- **Job:** attach the window to a runtime.
- **Actions:** 1, and on a fresh install it fails.
- **Eye lands on:** the error line under the fields.
- **Will try and cannot:** understand the failure. The message is
  `openbot acp ended before the handshake` — a protocol event standing in for a
  configuration fact the child process stated plainly on stderr and the window discarded.
- **Says why if stuck:** **no, and it has the answer in hand.** `engine.rs:673` returns the
  generic string; the spawned task is `JoinHandle<Result<(), acp::Error>>` and the SDK
  formats `Process exited with {status}: {stderr}` — then `connect` calls `task.abort()`
  instead of reading it. There is even a comment at `engine.rs:459` explaining that a
  pre-spawn existence check was added *specifically* so a missing binary would not produce
  this exact string. The same class of fault, one step later, is unhandled.

## J3 FIRST BOT — `s03`

- **Job:** zero to one teammate with a real standing brief.
- **Actions:** 3 (New Bot → name → submit). Unreachable behind J2.
- **Eye lands on:** the empty-pane card, which since `12e3b21` carries a primary action.
- **Will try and cannot:** write the brief here. The dialog asks only for a name; the
  standing brief — the thing that makes a Bot a teammate rather than a chat — is set later
  via Edit, and nothing on this screen says so or pre-fills one.
- **Says why if stuck:** n/a, no failure path.

## J4 FIRST RUN — `s05`

- **Job:** send work, watch it happen.
- **Actions:** 2. Unreachable behind J2.
- **Eye lands on:** the agent's prose, then the step rows. Correct.
- **Will try and cannot:** see the computer while it works — it is behind a top-right
  button (B03).
- **Says why if stuck:** partially; see J7.
- **Note:** this stage is the product's strongest surface and needs no work. Steps state
  their target ("Read applications/2026-08.jsonl · 120 characters"). Rubric 5 is the only
  line at 3 in the baseline.

## J5 FIRST GATE — `s06`

- **Job:** the moment the whole product is explained.
- **Actions:** 1.
- **Eye lands on:** "Allow once" — the accent fill, and the consequential choice.
- **Will try and cannot:** learn *why* they are being asked, or that the gate is enforced
  in the hub where the agent cannot reach it. The dialog states the tool, the arguments and
  the choices, and never states the thesis. This is the product's whole argument and its
  best moment to make it, used to render three buttons.
- **Says why if stuck:** the tool's own reason line is shown, which is good.

## J6 STEADY STATE — `s04`, `s09`, `s10`, `s12`

- **Job:** run several Bots without losing track.
- **Eye lands on:** four identical roster rows (B01 — no status exists at all).
- **Will try and cannot:** tell which Bot is working, waiting on them, or paused; see the
  computer without a click; find which of three routines is failing (only `enabled` exists).
- **Says why if stuck:** n/a.

## J7 FAILURE — `s11`, `s07`, `s02`

Scored against the four doctrine elements: WHAT / WHY / WHAT IS SAFE / ONE ACTION.

| State | WHAT | WHY | SAFE | ACTION | Verdict |
|---|---|---|---|---|---|
| `s02` connect failure | ✗ protocol string | ✗ discarded | ✗ | ✗ | **P0** |
| `s11` guest disconnected | ~ banner names it | ~ | ✗ **silent on the open `shell.exec`** | ✗ "Dismiss" | **P0** |
| `s11` budget exhausted | ~ in prose | ✓ | ✗ | ✗ no raise-it | **P0** |
| `s07` denied by policy | ✓ | ✓ | ✓ reads final | n/a correctly | **pass** |
| connector 401 | — | — | — | — | **no rendering path found** |

The denial state is the one that already reads the way the doctrine asks. Everything else
fails on WHAT IS SAFE, which the doctrine calls the worst possible silence: `s11` drops the
computer mid-`shell.exec` and never says whether the command ran.

## J8 RETURN — **no fixture scenario exists**

Coming back after two days is not represented in the twelve states at all. There is no
surface for "what happened while you were away": no unread marker, no routine-fired digest,
no since-last-visit boundary in a thread. Cannot be scored from screenshots because it
cannot be photographed. **Filed as a harness gap and a product gap, not scored.**

---

## The blindness note — why run 1 missed all of this

**Every fixture scenario from `s03` onward sets `window.__replies.connected = true`.** The
harness starts on the far side of the wall. That is correct for photographing steady-state
surfaces and it is exactly why last night's loop — which cleared four real defects — never
noticed that a fresh install cannot start at all.

A screenshot harness is only as honest as the state it opens in. J1 and J2 had to be
established by running the shipped binary, not by looking at pictures of the app.

---

## Run 2 items

### B11 — J2 reported a protocol event instead of the configuration fact
`done 3665cb8` · J2 · rubric 12

`Engine::connect` discarded the child's stderr and said "openbot acp ended before the
handshake". WHAT and WHY now carried. See MEMORY for why awaiting the task's error does not
work.

### B12 — the J2 error carried none of the four doctrine elements
`done` (this iteration) · J2 · rubric 12, 1, 3

Was a single red string, `String(err)`, with no structure, no statement of what is safe, and
no action. Now: WHAT in the person's vocabulary, WHY in the runtime's own words, WHAT IS SAFE
("Nothing started. No Bot ran, and nothing on this computer was touched."), and one action.

The two failure modes are distinguished, which was the spec's own example of a J2 defect: a
missing runtime binary and a runtime with no model are different faults with different fixes,
and telling somebody the wrong one sends them to retry the wrong thing. `CONNECT_FAULTS`
matches on what the runtime said; the key-not-set case is tested before the model case
because the runtime words it as "no usable model: $KEY is not set" and the model rule would
otherwise swallow it.

The demo is **not** offered when the binary is missing — the demo runs *in* the runtime, so
with no runtime it would be a button that fails the same way.

Emphasis moves to the action that resolves the state. After a failed connect the primary is
no longer Connect, because pressing Connect again does the same thing and fails the same way;
the loudest control on the screen would have been the one that cannot work.

### B13 — the fixture could not see J1 or J2
`done` (this iteration) · harness

Every scenario from `s03` on set `connected = true`. Added `s13` (no model configured) and
corrected `s02` to `found()`'s verbatim wording. 52 shots now, still under 5s.

**Still not covered: J8 (RETURN).** There is no surface for "what happened while you were
away" — no unread marker, no routine-fired digest, no since-last-visit boundary — so there is
nothing to photograph. Product gap and harness gap together.
