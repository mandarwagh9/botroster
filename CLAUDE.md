# CLAUDE.md

OPENBOT: persistent, named AI teammates sharing one durable computer. Rust workspace, nine crates,
a Tauri desktop client. Apache-2.0.

`README.md` is the product and the crate layout. `docs/SPEC.md` is the architecture, numbered by
section — code comments cite it as "SPEC.md §7". `CONTRIBUTING.md` is the bar every change is held
to. This file is only the things you would otherwise learn by breaking something.

## Building

Run the sidecar script before the first build of a fresh clone:

```sh
sh scripts/sidecar.sh          # builds openbot-cli, stages it at crates/openbot-app/binaries/
cargo build --workspace
```

The desktop client ships the runtime as a Tauri sidecar, and Tauri's build script wants that binary
to exist before `openbot-app` will compile. Skip it and the workspace fails at a build script with
an error that says nothing about the cause. The staged binary is an artefact and is not committed.

On Windows, stop any running `openbot.exe` or `openbot-app.exe` first. A held binary makes
`cargo build` fail with "Access is denied", and the tests then run against the stale one — which
looks like a passing suite and is not.

Building `openbot-app` on Linux also needs the WebKitGTK development libraries; `.github/workflows/ci.yml`
installs the set that works.

## Before you push

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three, all clean. CI additionally builds against Rust 1.89 exactly — `rust-version` in
`Cargo.toml` is a promise to anyone on an older toolchain, and the `msrv` job is what keeps it
true. `unsafe_code` is `forbid` across the workspace.

## Two invariants that do not weaken

- **`openbot-guest` must never be able to reach `openbotd`.** The credential store is
  `openbotd::secrets`, and the guest stays away from it by not depending on `openbotd` at all —
  there is no dependency cycle preventing that edge, so a test walks the manifests instead:
  `crates/openbot-guest/tests/isolation.rs`.
- **The policy gate stays in the hub.** The agent asks the hub to call a tool; the hub evaluates
  policy, asks the person if it must, and only then forwards to the guest. Moving the gate into the
  agent lets the thing being gated remove the gate.

A change that weakens either is not merged, whatever else it improves.

## Tests

Files named `*_live.rs` run against a real hub, a real guest, real files, a real browser over CDP
and a real HTTP model endpoint. They are slow because they are real. Do not replace one with a mock
to make it fast — that deletes the coverage the file exists for.

The browser suite skips politely when no Chromium is installed. CI sets `OPENBOT_REQUIRE_BROWSER=1`
so a missing browser fails instead, because a skip that reads as a pass is worse than a failure.
Set `OPENBOT_BROWSER` to point at a browser outside the usual install locations.

`crates/openbot-cli/tests/readme.rs` parses the shell fences out of `README.md` and checks that
every `openbot ...` line names a real subcommand and real long flags. **Editing the README can
therefore fail `cargo test`.** It checks commands, not prose — which is how the stale
`./openbot-data` sentence above survived.

## Where things are

- `crates/openbot-app/ui/` — the desktop UI is `index.html`, `main.js` and `styles.css`. Vanilla:
  no bundler, no framework, no npm, no build step. Keep it that way.
- `crates/openbot-app/gen/` — Tauri regenerates it. Gitignored, never edited.
- `/openbot-data` — local state from running the binaries inside the checkout. Gitignored.
- The default home is `~/.openbot` (`%USERPROFILE%\.openbot` on Windows), resolved once in
  `openbot_proto::default_home` so the CLI and the window cannot disagree about where a person's
  Bots are. They did once. It falls back to `./openbot-data` only when `HOME`/`USERPROFILE` is
  absent or empty.
- `$OPENBOT_HOME` overrides it. Runtime knobs are `OPENBOT_*` environment variables throughout.

## Provenance

Nothing enters this repository without a row in `PROVENANCE.md` mapping it to its upstream and
licence. OPENBOT is an independent implementation of the shape xAI shipped as Grok Bot — not a
fork, and containing none of their code. `openbot-proto` was reimplemented from the published
types. This is the project's entire legal position; treat the table as a gate, not a formality.

## How this repository is written

Comments and commit messages explain *why*, and say what broke before. Not terse. The CI workflow
carries a paragraph about apt mirrors stalling because they stalled three times and held a leg of
the matrix for hours; `Cargo.toml` explains the version drift it fixed. When you change something
for a reason, the reason goes next to it.

Commit subjects are lowercase and read as prose, often with a clause saying what it buys:
"the window starts its own computer, and ships as an installer", "ui: the rules editor is two rows,
so its fields fit on every platform's font". Long bodies are welcome — record the alternatives you
rejected and what you measured.

Claims get tests. A doc comment saying "never" or "always" is a claim. After a test passes, break
the code and confirm the test notices; a test that stays green when the behaviour it guards is
removed is not a test.

## What does not exist yet

The hypervisor backend. Today's guest is an ordinary process running as the user — not a VM, not a
container. `fs.*` refuses paths escaping the workspace root, but that is enforced by this code and
not by the operating system. Do not write documentation, comments or UI copy that implies isolation
the project does not have.
