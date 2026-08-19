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
