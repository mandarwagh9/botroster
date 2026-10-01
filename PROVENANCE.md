# Provenance

Every component in `botroster` that derives from someone else's work, what licence it carries, and what
that obliges us to do. This file is a hard requirement of the project, not documentation courtesy:
it is what makes `botroster` safe for other people to adopt and redistribute.

**Rule:** nothing enters this repository without a row in this table.

---

## 1. Direct upstream

### `xai-org/grok-build`: Apache-2.0
SpaceXAI's coding agent harness and TUI. Published 2026-07-14 as a periodic export from a private
monorepo. External contributions are not accepted; the tree is published for source transparency and
local builds.

**Everything in this section was read at `SOURCE_REV` `559751fdcec02d413e4c57c8832ab275e4f44980`**
(mirror commit `2bdd1d6`, 2026-09-29). The revision is pinned because upstream publishes a sync every
few days **and never bumps its own `PROTOCOL_VERSION`**, so the version string cannot identify a set
of types — the SHA is the only thing that can. Re-verify this section against a new revision before
relying on it; a sync that touches `xai-tool-protocol` invalidates the table below.

| What we take | From | How |
|---|---|---|
| Computer Hub wire protocol (frames, methods, handshake, error codes) | `crates/common/xai-tool-protocol` | **Reimplemented** in `botroster-proto` from the published types. **Not wire-compatible** — see the divergence table below. Structural derivation: attributed under Apache-2.0 §4. |
| Hub transport / registry / resolver concepts (local-shadows-remote) | `crates/common/xai-computer-hub-core` | Design adopted; our own implementation |
| Guest tool-server shape (`--capabilities` probe, in-guest `/ready` + `/statusz`, daemonize, hub-connect dwell) | `crates/codegen/xai-grok-workspace/src/bin/workspace_server.rs` | Design adopted; our own implementation |
| Skills / plugins / hooks / permissions / sandbox **file formats** | `/build/features/*` docs + config crates | Format adopted verbatim for compatibility. Formats are interfaces, not expression. |
| Bot relay method shapes and bot tool contracts | `bot_relay.rs`, `bot_tools.rs` | **Read for design only. Nothing borrowed.** No code, no identifier, no string and no file layout was copied from either; the 12 `bot_*` tool ids and 110 gateway commands that exist there are contracts with no implementation behind them, so there was nothing to take. Recorded because §5 says a component is recorded before it is merged, and "we read it and took nothing" is a fact a reader cannot otherwise check. |
| Agent runtime, tools, TUI | `xai-grok-shell`, `xai-grok-tools`, `xai-grok-pager` | **Not taken, and not planned.** Measured at `SOURCE_REV` above: the `grok` binary's dependency closure is 101 crates and ~1.93M lines, `xai-grok-shell` alone is 430k own lines across an 81-crate closure, and upstream moved +855k/-401k lines in 24 syncs in seven weeks. A fork is a moving two-million-line target. Revisit only with a measurement, not a preference. |

#### `botroster-proto` is not wire-compatible with `xai-tool-protocol`

This section corrects a claim it used to make, and the claim was false when written. The row above now
records the reimplementation as *not* wire-compatible, where it previously promised compatibility in
order to justify reimplementing rather than vendoring. The promise failed on the first field either
end reads, and it failed before any of the newer upstream features came into it. The reasoning that
produced the old wording was sound — a reimplementation should not silently fork the protocol — but
"compatible" was asserted rather than checked, and nobody checked it. The table below is the check.

An unmodified upstream harness **cannot** talk to `botrosterd`. Every difference below was read from
both trees at the pinned `SOURCE_REV`; the left column is a recorded fact about that revision, and the
right column is checked against the live types by `crates/botroster-proto/tests/divergence.rs`.

| Divergence | Upstream `xai-tool-protocol` | BOTROSTER `botroster-proto` |
|---|:---:|:---:|
| tool call id field | `tool_call_id` | `call_id` |
| tool call arguments field | `arguments` | `args` |
| approval request method | `permission_request` (in `xai-computer-hub-sdk`) | `approval.request` |
| approval reply | `hook_reply`, a second frame | `ApprovalDecision`, the JSON-RPC result of the request |
| tool id character set | `[a-zA-Z0-9_-]+` per segment, at most one `:` | unvalidated; `fs.read`, `shell.exec` and `browser.*` are dotted and do not parse upstream |

**Every application error number we share with upstream, and what it means on each side.** These are
the rows that matter most in the table, because a collision is not a mismatch: a peer that maps numbers
reads our code as something else entirely, and three of these invite a retry of a call that was
deliberately refused. Upstream's own file notes that receivers *should* switch on the string
`data.code` rather than the number, which is good practice and does not help a peer that does not.

| error number | Upstream `xai-tool-protocol` | BOTROSTER `botroster-proto` |
|---|:---:|:---:|
| `-32001` | `timeout` | `WORKSPACE_UNAVAILABLE` |
| `-32002` | `unauthorized` | `SESSION_NOT_FOUND` |
| `-32003` | `forbidden` | `NO_SERVER_BOUND` |
| `-32004` | `connection_lost` | `FORBIDDEN` |
| `-32005` | `tool_server_gone` | `APPROVAL_DENIED` |
| `-32006` | `session_not_found` | `TAKEN_OVER` |
| `-32008` | `session_draining` | `DIVERGED` |

Upstream's table jumps from `-32006` to `-32008`, so the three numbers BOTROSTER uses at `-32007`,
`-32009` and `-32010` are ones it leaves free. Those are genuinely *not* collisions, and an earlier
draft of this file listed one of them as a shared number — a row that was simply false, and worse for
the provenance of this document than the row it displaced. `divergence.rs` now checks both directions:
every shared number must be recorded with both meanings, and no number upstream leaves free may be
recorded as one.

Two more consequences worth stating rather than leaving in the tables. An upstream peer's `fs.read`
cannot be represented here, and ours cannot be sent there — the tool-id difference is a naming
difference, not a spelling one. And the collisions are worst exactly where a retry is the natural
response: read our `APPROVAL_DENIED` as `tool_server_gone` and a caller re-issues a call a person
refused; read our `DIVERGED` as `session_draining` and it waits for a session that will never drain.

**Three places this hub is deliberately a superset of the published shapes.** None is a divergence,
because none of them stops a peer that speaks the published protocol — the list above is the list of
things that do. They are recorded because a superset is still a difference, and the next person
reading this file will want to know which differences are deliberate.

- `botroster-proto`'s `Hello` carries a `token` field that upstream's `HelloMsg` does not have
  (`xai-tool-protocol/src/handshake.rs:31-37`). A client of this project's protocol has always
  authenticated in the frame; a client of the published one sends `Authorization: Bearer` and has
  nowhere to put a token. The hub reads the header and falls back to the frame.
- `session_open` accepts the session id from `params` **or** from the envelope. The published params
  have no session field at all, so an upstream client's session rides on the envelope; this project's
  clients name it in params. Params win where both are present.
- `ServerInfo` gained `status`, which the published protocol requires
  (`xai-tool-protocol/src/frames.rs:328-356`) and this hub previously omitted, so a client built
  against it could not parse a `servers.list` result at all. The variant names match upstream's.

**The version string is not a compatibility claim.** `botroster-proto` announces `botroster-1`, not
`1.0.0`. Upstream has published `1.0.0` continuously without ever bumping it, so a shared string would
let an incompatible peer clear the hub's only version check — a string equality test — and then fail
obscely on its first field read. Naming the protocol after its owner turns that into an early, legible
refusal instead.

A hub started with `BOTROSTER_INTEROP=1` answers `"1.0.0"` as well, and says so in
`supported_protocol_versions`. It is off by default, and turning it on only ever widens: the same hub
still speaks `botroster-1`, so no client this project already had changes behaviour. The switch exists
because the table above is still non-empty — the shapes behind `"1.0.0"` are not all in place, and the
alternative to a switch is claiming compatibility this project has not finished proving, which is the
claim this section was corrected to remove. **Nothing in this file asserts the two protocols
interoperate**; what has been demonstrated is listed in `interop/baseline.tsv` and the slice table in
the superplan's phase 2, measured against a client built from this pinned revision and no other.

**Obligations when we vendor or copy any of it (Apache-2.0 §4):**
1. Ship the Apache-2.0 `LICENSE` with the distribution.
2. Retain all copyright, patent, trademark and attribution notices.
3. Carry any `NOTICE` file content, in the same places.
4. **State prominently that files were changed**, where they were.

### Transitive: inherited through `grok-build`
Grok Build's own `THIRD_PARTY_NOTICES.md` discloses that its tool layer is itself ported:

| Component | Upstream | Licence |
|---|---|---|
| `apply_patch`, `grep_files`, `list_dir`, `read_file` (under `src/implementations/codex/`) | **openai/codex** (`codex-rs/core/src/tools/handlers/`) | Apache-2.0 |
| `bash`, `edit`, `glob`, `grep`, `read`, `skill`, `todowrite`, `write` (under `src/implementations/opencode/`) | **sst/opencode** (`packages/opencode/src/tool/`) | MIT |
| `ripgrep`: embedded in every release build, self-extracted to `~/.grok/vendor/` | BurntSushi/ripgrep | MIT / Unlicense |
| `ugrep`, `bfs`: embedded only when the release pipeline supplies them | respective upstreams | see upstream |
| Mermaid diagram stack | vendored under `third_party/` | see `third_party/NOTICE` |

If we take the tool layer, **all three licence chains come with it** and every notice must be
carried through. MIT requires the copyright notice and permission notice in all copies or
substantial portions.

### `agentclientprotocol/rust-sdk`: Apache-2.0

Not inherited through `grok-build` and not derived from it: the **Agent Client Protocol** is an
independent published standard with its own governance ([agentclientprotocol.com](https://agentclientprotocol.com)),
which editors including Zed already speak. It is listed here because §5 says nothing enters this
repository without a row, and a dependency is a dependency however respectable its origin.

| What we take | From | How |
|---|---|---|
| ACP wire types (`StopReason`, `SessionUpdate`, `PermissionOption`, the `initialize` handshake) | `agent-client-protocol` 2.0.0 (crates.io) | **Depended on, not copied.** Reimplementing another project's protocol types produces a copy that drifts; the SDK is the definition. |
| Protocol semantics: method names, the permission model, capability negotiation | the published spec | Implemented against, as any client of a standard is. |

No files are modified, so Apache-2.0 §4(b) does not apply. Its `LICENSE` and any `NOTICE` ship with
the crate and must be carried into any binary distribution of BOTROSTER, the same as every other
Apache-2.0 dependency. Botroster is the **Agent** side; §9 of the spec records which client-side methods
it deliberately never calls, and why that is a security position rather than an unfinished one.

### `tauri-apps/tauri`: Apache-2.0 / MIT

The shell of the BOTROSTER desktop client (`crates/botroster-app`) is built on Tauri 2: a Rust windowing
and webview layer, so the client keeps one toolchain with the rest of the project instead of
shipping an Electron-sized runtime. Listed here under §5's "a dependency is a dependency" rule, and
because §9 names Tauri as the client's engine.

| What we take | From | How |
|---|---|---|
| Window, webview, and command bridge between the page and the Rust commands | `tauri` / `tauri-build` 2.11.x (crates.io) | **Depended on, not copied.** The frontend (static HTML/CSS/JS in `crates/botroster-app/ui`) is ours; the runtime is the crate. |
| Native folder picker | `rfd` 0.17 (crates.io, MIT) | Depended on, not copied. |
| Dialog request ids in the shell | `uuid` 1.x (crates.io, Apache-2.0/MIT) | Depended on, not copied. |

No files are modified, so §4(b) does not apply. Apache-2.0/MIT notice text ships with the crates and
must be carried into any binary distribution of BOTROSTER, like every other dependency. The webview
itself is the OS's own (WebView2 on Windows, WKWebView on macOS, webkit2gtk on Linux), not
third-party code we redistribute.

## 2. Trademarks: granted by nothing

Apache-2.0 **§6 explicitly withholds trademark rights.** The code grant does not license the name.

- "Grok", "Grok Bot", "Grok Build", xAI, SpaceXAI, X.AI LLC, Cursor, Anysphere: **not ours to
  use.** No logos, no wordmarks, no "compatible with"/"powered by" branding, no confusingly similar
  naming.
- Referring to them factually ("derived from grok-build") is nominative use and is fine. Presenting
  `botroster` *as* Grok Bot is not.
- `botroster` is a placeholder name pending a trademark search.

## 3. Clean-room boundary

Components with no open upstream: VM orchestration, the multi-Bot layer, routines, the approval
engine, the credential broker, the clients: are built from **published documentation and observed
behaviour only**:

- `docs.x.ai/grok-bot/*` and `docs.x.ai/build/*`: public documentation, read with an HTTP client
- `x.ai/bot`, `x.ai/news/introducing-grok-bot`: public marketing pages
- the public npm registry and the public GitHub repository

**Not used, and not to be used:** the contents of any proprietary-licensed binary. The
`@xai-official/grok-*` platform packages at `0.1.x` are published as `Proprietary`; only the
Apache-2.0 `1.0.x` line and the GitHub source tree are in scope. No `strings` output, no
decompilation, no lifted prompts or string tables. There is no reason to go near them: the
successor's source is published.

## 4. Our own licence

`botroster` first-party code is **Apache-2.0**, matching the primary upstream so the combination is
frictionless and downstream users inherit one coherent grant.

### Brand assets

An icon is not code, and Apache-2.0 §6 withholds trademark rights in both directions: the grant on
this repository's code does not license its marks to anyone either.

| Asset | Where it is used | Origin | Status |
|---|---|---|---|
| `docs/brand/app-icon-source.png` (penguin on violet) | source for `crates/botroster-app/icons/*` and the `.mark.product` data URI in `ui/styles.css` | **Supplied by the repository owner.** Not drawn in-repo, not taken from any upstream in this file. | ⚠️ **Origin unconfirmed.** See below. |
| `docs/botroster-*.png` (nine screenshots) | `README.md` | **First party.** Screen captures of this project's own desktop client. The two named `botroster-readme-*` are rendered by `scripts/ux-shots.mjs` from the shipped `ui/`, so the picture at the top of the README cannot drift from the product; the rest were taken by hand from local builds and are cited in `.claude/product-review/reports/design-client.md` as dated evidence of what the client looked like then, which is why they are not refreshed. No third-party UI, artwork, wordmark or window chrome from another product appears in them. | ✅ Apache-2.0 with the rest of the repository. |

The screenshot row looks like a formality and is not. §5 says a component is recorded before merging,
and the rule only works if it is applied to the boring cases too — the one asset that arrived without
a row arrived precisely because nobody thought a picture counted. `scripts/review.sh` now fails on any
committed image, font or icon that this table does not name, so the next one cannot be quiet. A row
covering a whole directory has to say so with an explicit `dir/*`; the first version of that gate
accepted a bare directory name, which let one recorded file vouch for every future sibling.

**The open question, recorded rather than assumed.** The file arrived as
`bc2dd8dcd42bee12-penguin-violet-bg.png` — a content-hash filename, which is what a download or a
generator produces, not what an author names their own artwork. Whether it is original, commissioned,
generated, or taken from somewhere with terms attached is not something this repository can tell by
looking at the pixels, and it is published to the world in every release.

This row exists because §5 below says a component is recorded **before** merging, and the honest
record here is "we do not yet know". It is deliberately not a licence claim. Before a release that
is offered to anyone else, the owner should either confirm the asset is theirs to license and
replace this row with the actual terms, or swap the artwork for one whose terms are known.

Nothing else in the repository depends on the answer: the icon set and the data URI are both
regenerated from this one file, so replacing it is a one-command change.

## 5. Adding a dependency

1. Record it in the table above **before** merging.
2. Check licence compatibility with Apache-2.0 (GPL/AGPL is a hard stop for linked code).
3. Copy required notices into `NOTICE`.
4. If any file is modified, state the change in-file per Apache-2.0 §4(b).
