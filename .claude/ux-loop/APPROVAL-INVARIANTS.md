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
