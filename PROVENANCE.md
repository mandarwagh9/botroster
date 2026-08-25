# Provenance

Every component in `botroster` that derives from someone else's work, what licence it carries, and what
that obliges us to do. This file is a hard requirement of the project, not documentation courtesy:
it is what makes `botroster` safe for other people to adopt and redistribute.

**Rule:** nothing enters this repository without a row in this table.

---

## 1. Direct upstream

### `xai-org/grok-build`: Apache-2.0
SpaceXAI's coding agent harness and TUI. Published 2026-07-14 as a periodic export from a private
monorepo (`SOURCE_REV` records the internal SHA). External contributions are not accepted; the tree
is published for source transparency and local builds.

| What we take | From | How |
|---|---|---|
| Computer Hub wire protocol (frames, methods, handshake, error codes) | `crates/common/xai-tool-protocol` | **Reimplemented** in `botroster-proto` from the published types, to stay wire-compatible. Structural derivation: attributed under Apache-2.0 §4. |
| Hub transport / registry / resolver concepts (local-shadows-remote) | `crates/common/xai-computer-hub-core` | Design adopted; our own implementation |
| Guest tool-server shape (`--capabilities` probe, in-guest `/ready` + `/statusz`, daemonize, hub-connect dwell) | `crates/codegen/xai-grok-workspace/src/bin/workspace_server.rs` | Design adopted; our own implementation |
| Skills / plugins / hooks / permissions / sandbox **file formats** | `/build/features/*` docs + config crates | Format adopted verbatim for compatibility. Formats are interfaces, not expression. |
| Agent runtime, tools, TUI | `xai-grok-shell`, `xai-grok-tools`, `xai-grok-pager` | **Planned: vendored or depended on directly.** Not yet integrated. Will carry full LICENSE + NOTICE when it lands. |

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
| `docs/botroster-*.png` (seven screenshots) | `README.md` | **First party.** Screen captures of this project's own desktop client, taken from a local build. No third-party UI, artwork, wordmark or window chrome from another product appears in them. | ✅ Apache-2.0 with the rest of the repository. |

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
