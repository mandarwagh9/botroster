# Approval invariants

Non-negotiable, enforced by test in `scripts/ux-audit.mjs`. Extracted verbatim from
`LOOP.md` section 3 so the file the loop spec names actually exists. **Relocated, not
rewritten** — including the launch amendment on invariant 4, which a reader of a freshly
extracted file would otherwise take at face value and re-open.

The security story lives in this dialog. An unattended loop restyling it can quietly break
it.

1. The affirmative action is **never** the default-focused element on mount.
2. "Allow for the session" is visually distinct from "Allow once" and is never the primary.
3. The exact tool name and the full arguments are rendered before any choice is reachable.
   No truncation without an expand affordance that is in the tab order.
4. Escape and click-outside resolve to **refuse**, never to allow, never to
   dismiss-and-continue.
   > **AMENDED AT LAUNCH (2026-08-20).** The shipped behaviour is that Escape does *nothing*
   > to an approval — it does not resolve it and does not dismiss it. That is pinned by
   > `crates/openbot-app/tests/page.rs::escape_closes_a_panel_but_never_an_approval`. Both
   > behaviours fail closed. The gate asserts the **shipped** behaviour, and the divergence
   > is filed as BACKLOG B09 for the operator. Do not "fix" the code to match the line above
   > without that decision.
5. A `deny` from policy renders as unappealable. No control in the UI suggests otherwise.
6. Nothing auto-resolves on timeout. No countdown. No "remember my choice" that spans
   sessions.
7. The gate is not skippable by keyboard mashing: a single Enter on mount cannot approve.

## The carve-out

The generic keyboard and a11y gates exempt this dialog **on purpose**. "Escape closes
anything dismissible" and "no focus trap outside intentional modals" both point straight at
a dialog that is deliberately non-dismissible and deliberately traps focus. Applied
literally they would have the loop spend the night dismantling the property the gates exist
to protect.

## J5 and this file

The loop spec asks J5 to make the thesis land — *the gate is in the hub, the agent cannot
remove it* — in one sentence, once, never again. Nothing above forbids adding a sentence.
What it forbids is buying that sentence with any of: focus on an affirmative, a softened
session grant, truncated arguments, a countdown, or a dismissal that resolves.

`crates/openbotd`, `crates/openbot-guest` and `crates/openbot-proto` are read-only. If a
UX improvement requires changing *what* gets gated, it goes to the operator, not into a
commit.

## Bypass

The window can be told to answer approvals itself, from the composer. This is the CLI's
`--approve auto` with a face — *"approve everything without asking, for runs where the
operator has explicitly accepted the risk"* — and it has been product policy since before
this window existed.

**What it does not do.** It does not move the gate. The hub still evaluates policy on every
call and still refuses anything a `deny` rule covers; a client has never been able to approve
past one, and invariant 5 is untouched. `openbotd`, `openbot-guest` and `openbot-proto` are
not involved. What bypass removes is the person, not the control plane.

The rules it must keep, each pinned by a test in `page.rs`:

- **Off when the window opens.** `the_window_starts_by_asking_and_not_by_approving`. This one
  matters more than it looks: every other approval test answers the dialog, so a bypass that
  defaulted on would leave the whole suite green while the product approved everything.
- **The narrowest grant, never the session one.**
  `bypass_takes_the_narrowest_grant_and_never_the_session_one`. `allow_once` is a client-side
  convenience; `allow_always` is a hub-side grant that outlives the toggle. Reaching for the
  larger one is the single way this feature could actually weaken the gate.
- **A credential request still stops and asks.**
  `bypass_does_not_answer_a_request_for_a_credential`. It wants a value, and a bypass has none
  to give.
- **Session-scoped, never persisted.** Cleared on disconnect and gone on reload. Invariant 6
  forbids a choice that spans sessions, and a flag surviving a restart would be exactly that.
- **Every auto-approved call is recorded with its arguments.** Invariant 3 requires the full
  argument list to be readable before a choice is reachable; with no choice to reach, the
  arguments still have to land where a person can find them afterwards. Marked distinctly,
  because "approved" and "approved by you" are different facts.
- **Loud while on.** Amber, which is consistent with DIRECTION rather than an exception to it:
  this is a state where a person's judgement is being skipped, and that is what amber is
  reserved to mean.

It applies to requests arriving after it is turned on. A click that instantly answered a
dialog already on screen — one somebody was part-way through reading — would be the opposite
of a considered decision.
