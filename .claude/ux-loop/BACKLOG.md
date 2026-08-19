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

### B02 — the approval dialog makes the consequential choice the loudest thing
`open` · rubric 7 · invariant 2 · `shots/000/s06-1280-dark.png`

"Allow once" is the filled primary button — the highest-contrast object in the dialog. "Not
this time" is the quietest. The safe choice should be the loud one. LOOP.md invariant 2 says
the session-scoped option is never primary (it is not — it carries a danger outline, which is
correct), but nothing says the *approve* action gets to be primary either, and rubric 7 asks
whether the consequential choice is visually subordinate to the safe one. It is not.

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
