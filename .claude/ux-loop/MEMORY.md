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

<!-- Append one block per iteration: what was tried, whether it held, the generalisable
     lesson. A revert is more valuable here than a success, so write those first. -->
