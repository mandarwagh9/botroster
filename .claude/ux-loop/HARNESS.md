# The harness

Built in Phase 0. Three pieces: a fixture that runs the shipped frontend with no Rust behind
it, a shot script, and a gate script.

## Fixture

`crates/openbot-app/ui/` is three hand-written files with no bundler and no npm, served
verbatim by Tauri. `main.js` reads `window.__TAURI__.core.invoke` at module scope, so the
whole fixture is: define `window.__TAURI__` before `main.js` loads. Nothing in the shipped UI
changes.

- `crates/openbot-app/tests/fixture/tauri-stub.js` — the IPC double. **Shared**: `page.rs`
  pulls it in with `include_str!` and the node server reads the same file, so the Rust suite
  and the shot harness cannot drift apart. It records every `invoke` in `window.__calls`,
  answers from `window.__replies`, throws from `window.__throw`, and delivers events through
  `window.__fire`.
- `.claude/ux-loop/fixture/scenarios.js` — the twelve states. Each has `pre()` (fills
  `__replies` before `main.js` runs) and `post()` (drives the page afterwards). Selected by
  `?s=s06`.
- `.claude/ux-loop/fixture/serve.mjs` — splices stub + scenarios + a driver around the
  shipped `index.html` and serves it on loopback. The driver awaits `post()`, waits two
  frames, then sets `window.__ready`.

```sh
node .claude/ux-loop/fixture/serve.mjs      # http://127.0.0.1:4173/?s=s06
```

Determinism: no `Math.random`, no `Date.now`, no network, animations frozen before capture.
Two runs produce identical pixels.

## Shots

```sh
node scripts/ux-shots.mjs 007
```

12 scenarios x {1280x800, 1600x1000} x {dark, light} = 48 PNGs into
`.claude/ux-loop/shots/007/`, plus `shots.json` with timings and any scenario that threw.
**Measured: 4.0s** against a 90s budget. Theme comes from Playwright's `colorScheme`, because
`styles.css` themes through CSS `light-dark()` and a class toggle would do nothing.

## Gates

```sh
sh scripts/ux-verify.sh
```

Exit non-zero on any failure; last line is `GATES total_failures=N bundle_bytes=N ...`.

| Gate | What it does |
|---|---|
| preflight | No `openbot*.exe` holding the build; a sidecar is staged |
| rust | `cargo check` + `clippy -D warnings` on `openbot-app` and `openbot-desktop`, `cargo fmt --check` |
| frontend | `node --check` on `main.js` and `scenarios.js`, then `cargo test -p openbot-app --test page` |
| axe | every scenario, both themes, zero serious or critical |
| contrast | computed from rendered pixels: 4.5:1 body, 3:1 large, both themes |
| keyboard | every visible interactive element reachable by Tab |
| reduced motion | nothing still transitioning transform/opacity under `prefers-reduced-motion` |
| approval invariants | LOOP.md section 3, asserted against the real dialog in a real browser |
| bundle size | bytes of `ui/*` + vendored fonts, fail on >15% over baseline |

### Two gates that were translated rather than invented

**"frontend typecheck and production build."** There is no TypeScript and no bundler in this
repo, and adding one to satisfy a gate would be the tail wagging the dog. The honest
equivalent is a syntax check plus `cargo test -p openbot-app --test page`, which drives the
shipped `main.js` in a real browser and is where its behaviour is actually pinned — about
forty tests covering the approval queue, the refusal mapping, the credential form and the
session filter. That suite runs every iteration despite costing the most, because the loop
edits exactly the file it guards.

**"bundle size."** Byte count of `index.html` + `main.js` + `styles.css` + `ui/fonts/`.

### The approval carve-out

The generic keyboard rules in LOOP.md — "Escape closes anything dismissible", "no focus trap
outside intentional modals" — point straight at a dialog that is *deliberately*
non-dismissible and *deliberately* traps focus. Applied literally they would have the loop
spend the night dismantling the security property the gates exist to protect. `ux-audit.mjs`
therefore asserts the approval dialog's own invariants explicitly and exempts it from the
generic ones.

Invariant 4 is amended: LOOP.md says Escape resolves to refuse; the shipped behaviour is that
Escape does nothing to an approval, pinned by `page.rs`. The gate asserts the shipped
behaviour. See BACKLOG B09.

## What is not covered

Motion, latency, scroll feel and input responsiveness. A screenshot cannot see any of them.
