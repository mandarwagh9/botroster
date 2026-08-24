# CLI & Developer Experience — review

**Reviewed:** `CLAUDE.md`, `CONTRIBUTING.md`, `README.md`, `docs/SPEC.md` §9; the whole of
`crates/openbot-cli/` (`src/main.rs`, `src/config.rs`, `src/status.rs`, `src/render.rs`,
`src/up.rs`, `src/watch.rs`, `src/approve.rs`, `src/acp/mod.rs`, `src/acp/serve.rs`,
`tests/readme.rs`, `tests/reaches.rs`, `tests/acp_live.rs`). Plus ~40 live invocations of the built
binary: every `--help` in the tree, `status` against three different homes, a nine-case malformed
`config.toml` battery run against scratchpad homes via `--home`, every hub-dependent command with no
hub running, and a real ACP JSON-RPC handshake (`initialize` → `session/new` → `session/prompt`)
driven over stdio with no hub running.

The binary is `target/debug/openbot.exe`, built 22 Aug 22:30; every CLI source file predates it
(`main.rs` 20 Aug, `config.rs`/`status.rs` 22 Aug 02:1x). CLAUDE.md warns that a held binary on
Windows makes tests run against a stale build — this one is not stale, so the runtime output below
is what this source does.

**Verdict.** The prose in this CLI is the best I have read in an open-source agent tool. `openbot
attach --help` explains why the prompt carries a path instead of bytes; `up.rs` explains why
`--home` and `--workspace` may not be the same directory; the "no usable model" error offers three
different recoveries and the `--api-key-env` help explains why a flag is the wrong place for a key.
Someone thought hard about the reader. Then the first ten minutes undo most of it. Getting to a
first result costs five steps and **two terminals**, and nothing says so until after you have
already occupied the first one. Eight hub-dependent commands and the whole editor bridge fail with a
bare `os error 10061` that names neither the hub nor a remedy, `openbot run`'s one remedy names two
binaries the documented install does not put on your PATH, and the config example printed in the
README is *rejected by the parser that reads it* — `action = "ask"` is what the README, the
`permission add` help and the CLI's own error message all say, and `require_approval` is the only
thing the deserializer accepts. The single biggest friction is that **`config.toml` is the product's
control surface and the commands that look like they check it don't**. The validation exists and is
excellent — `openbot permission ls` refuses every malformed rule I fed it, naming the field and
listing the alternatives — but it is filed under `permission`, where nobody goes to ask "is my
config valid?". The two commands people *do* reach for both answer wrongly: `config show` reports
success on a file `openbot up` refuses to start on, and `status` — whose one-line help is "Is
anything wrong?" — reports "nothing wrong" on a file that will not parse at all. Then `config set`,
the repair those two send you to, silently deletes every part of the file it does not recognise,
sixty lines below a doc comment saying that is worse than having no config editor.

## The first-run path, measured

The README offers two paths. The binary path — "Download it, open it, and press Connect" — bypasses
this crate entirely, so the measured path is the one that produces an `openbot` on your PATH.

**Path A — from source to a first scripted result. 5 steps, 2 terminals.**

1. `git clone … && cd roost-oss` — **KEEP.**
2. `cargo install --path crates/openbot-cli` — **KEEP.** Needs Rust 1.89. Produces exactly one
   binary, `openbot` (`crates/openbot-cli/Cargo.toml:11-13`). Note for step 5: it does **not**
   install `openbotd` or `openbot-guest`, which both exist as separate binaries
   (`crates/openbotd/src/main.rs`, `crates/openbot-guest/src/main.rs`).
   *CLAUDE.md's `scripts/sidecar.sh` trap does not apply here — that gates `cargo build
   --workspace` because of `openbot-app`'s build script, and `cargo install --path
   crates/openbot-cli` never compiles `openbot-app`. Correctly scoped in both docs.*
3. `openbot up` — **KEEP, but it blocks.** It is a foreground process; the banner it prints
   (`up.rs:465`) ends with "Ctrl-C to stop". Terminal 1 is now consumed for the rest of the session.
4. Open a second terminal — **COULD BE REMOVED.** Nothing in the README's three-line quickstart
   says a second terminal is coming. You learn it from `up`'s own banner, at `up.rs:451`:
   ```
     Next, in another terminal:
       openbot run --demo --approve auto "prove it"   no API key needed
   ```
   — which is after you have already given up the terminal you were in. There is no `openbot up
   --detach`, no `openbot down`, and no way for `run` to bring up its own hub.
5. `openbot run --demo --approve auto "prove it"` — first useful result. **CONFUSING when step 3
   was skipped or died.** `--demo` needs no model and no key, but `run` connects to the hub
   *before* it looks at `--demo` (`main.rs:2487` connect, `main.rs:2506` the demo branch), so the
   offline demo is not offline. What you get:
   ```
   $ openbot run "hello"
   Error: could not reach the hub at ws://127.0.0.1:8443/v1/tools: connect: IO error: No connection
   could be made because the target machine actively refused it. (os error 10061)
   Start it with `openbotd`, and a guest with `openbot-guest`.
   ```
   Two commands you do not have, and no mention of the one you do (`openbot up`). See F-CD5.

**Path B — to a result from a real model. +3 steps, and two of them mislead.**

6. `openbot config set --model grok-4-5 …` — **CONFUSING.** Writes `api_key_env = "XAI_API_KEY"` and
   `base_url = "https://api.x.ai/v1"` into your file even if you only passed `--model`, silently
   discards `--token-budget` (which it accepts), and deletes any table or comment it does not know
   (F-CD1). Its five flags are documented nowhere: `openbot config set -h` prints `--model
   <MODEL>` with a blank description (F-CD11).
7. `export XAI_API_KEY=…` — **KEEP.** `status` catches the omission well: `model  grok-4-5
   XAI_API_KEY is not set in this shell`. This is the CLI at its best.
8. `openbot run --bot … "…"` — result.

**Path C — the editor bridge. 2 steps, and the second one fails in the editor.**
Point Zed at `openbot acp`. `initialize` and `session/new` both succeed; the first prompt returns
`-32603 Internal error: connect: IO error: … (os error 10061)` because the hub from step 3 is not
running and `acp` never checked (F-CD7). **CONFUSING.**

## Findings

### F-CD1 — `openbot config set` deletes every part of `config.toml` it does not recognise
`P0` · `reach: most users` · `crates/openbot-cli/src/main.rs:1487` vs `crates/openbot-cli/src/config.rs:187-194`

**What is true now.** `ConfigCmd::Set` mutates the typed `Config` struct and calls `config::save`,
which serialises that struct over the file. Run against a hand-written config:

```
$ cat config.toml
# my notes about why shell.exec is denied
[model]
id = "grok-4-5"
max_tokens = 4096

[permission]
rules = [
  { tool = "shell.exec", action = "deny", reason = "not on this box" },
]

[ui]
theme = "dark"

$ openbot config --home … set --model grok-4-6
saved …\config.toml

$ cat config.toml
[model]
id = "grok-4-6"
dialect = "openai"
base_url = "https://api.x.ai/v1"
api_key_env = "XAI_API_KEY"
max_tokens = 4096

[[permission.rules]]
action = "deny"
reason = "not on this box"
tool = "shell.exec"
```

The `[ui]` table is gone. The comment is gone. Exit code 0, and the word printed is "saved".

Sixty lines above `save`, `edit_rules` carries this doc comment — and `edit_rules` is what
`permission add`/`rm` correctly use:

> *This is a targeted edit of the parsed document, not a round trip through `Config`. Saving the
> typed struct would silently drop any key serde does not know about (a person's `[ui]` table, a
> setting added by a later version), and a config editor that deletes the parts it does not
> understand is worse than no config editor.*

Same command also accepts `--token-budget 50000` (it is a `global = true` flag on `ModelOverrides`,
`config.rs:284`) and writes nothing: `ConfigCmd::Set` destructures only its own five fields
(`main.rs:1487-1493`), so the budget is parsed, accepted, reported as "saved", and discarded.

**Why it matters.** The project wrote down the invariant and then broke it one function away. Today
the casualties are comments and a hypothetical `[ui]`; the moment any version adds a table this
binary predates, a single `config set` silently strips it — which is the forward-compatibility
failure the comment was written to prevent. The silently-dropped `--token-budget` is worse in kind:
the flag exists to stop an unattended routine spending money, the CLI says "saved", and the cap is
not there.

**The durable fix.** Delete `config::save`. Give `ConfigCmd::Set` a `config::edit` that takes
`&mut toml::Table` and sets only the keys named on the command line, the way `edit_rules` already
does — one document-editing primitive for the whole crate, so a whole-struct round trip is not
reachable. Then either wire `--token-budget` into it or reject the flag on `config set` rather than
accepting and ignoring it: a flag that parses and does nothing is worse than one that errors.

**How to prove it.** A test that writes a config carrying `[ui] theme = "dark"` and a
`token_budget`, runs `config set --model x --token-budget 50000` through the real binary, and
asserts both survive. It fails today on both counts. `config.rs`'s existing tests never exercise
`save` against a file containing an unknown table — the invariant is documented and untested.

---

### F-CD2 — the `config.toml` printed in the README is rejected by the parser that reads it
`P0` · `reach: most users` · `README.md` "Configuration" vs `crates/openbot-cli/src/config.rs:162`

**What is true now.** The README documents this, verbatim:

```toml
[permission]
rules = [
  { tool = "shell.exec", action = "ask", reason = "runs a command on the computer" },
]
```

Written to a home and read back:

```
$ openbot permission --home … ls
Error: [permission] rule 1: unknown variant `ask`, expected one of `allow`, `require_approval`, `deny`
in `action`
```

Three surfaces name the same value three ways:

| Surface | Accepted spelling |
|---|---|
| `README.md` config example | `ask` |
| `openbot permission add --action` help + its error at `main.rs:2152` | "use `allow`, `ask`, or `deny`" |
| the TOML deserializer (`openbotd::policy::Action`) | `allow`, `require_approval`, `deny` |

`permission add --action ask` maps `ask` → `RequireApproval` (`main.rs:2149`) and writes
`require_approval`, so a file the CLI generates is not the file the README tells you to write.
`openbot up` loads policy through `config::policy` (`up.rs:278`), which bails on this rule — so the
hub **refuses to start** on the config the README documents. And `openbot config show` on that same
file prints the model settings and exits 0.

**Why it matters.** The README's Configuration section is the first thing a self-hoster copies, and
copying it produces a hub that will not start with an error naming a variant that appears nowhere in
the documentation. The reader's most likely repair — deleting the rule — removes the approval gate
the README was demonstrating.

**The durable fix.** One spelling reachable from all three surfaces — most cheaply as a serde alias
so `ask` and `require_approval` both deserialize, with one of them canonical for output.

*Jurisdiction:* `Action` lives in `openbotd::policy`, which is Runtime & Security's scope, not mine,
and `require_approval` may be load-bearing as the wire spelling — CLAUDE.md states `openbot-proto` is
"wire-compatible with the published Grok Build protocol", so an alias is safe where a rename may not
be. My finding is that three surfaces disagree and the README's own example is the casualty; **which**
spelling wins is Runtime's call. The CLI-side half is mine either way: `main.rs:2152`'s error and the
`--action` help must list whatever set the deserializer actually accepts.

The current error message is otherwise excellent — it names the field and lists the alternatives — so
this is purely about which set it lists.

**How to prove it.** Extend `tests/readme.rs`. It parses ` ```sh `, ` ```console ` and ` ```bash `
fences (`readme.rs:35`) and checks every `openbot …` line names a real subcommand and real long
flags — which is why this bug shipped: the ` ```toml ` fence is never read. Add a case that feeds
every `toml` fence in the README through `config::load` and `config::policy` and asserts both
succeed. It fails today.

---

### F-CD3 — the first result costs two terminals, and nothing says so until the first is gone
`P0` · `reach: all users` · `crates/openbot-cli/src/up.rs:451`, `crates/openbot-cli/src/main.rs:2487`

**What is true now.** `openbot up` runs the hub and guest in the foreground of the calling shell.
Its banner, printed after it has taken the terminal, reads:

```
  Next, in another terminal:
    openbot run --demo --approve auto "prove it"   no API key needed
```

The README's quickstart lists the two commands adjacently with no mention of a second shell:

```sh
cargo install --path crates/openbot-cli        # once
openbot up                                     # a hub and a computer
openbot run --demo --approve auto "prove it"   # a scripted run against real tools
```

A reader typing these in order gets a hung-looking third line. There is no `openbot up --detach`,
no `openbot down`, and `run` has no fallback: it connects at `main.rs:2487`, before it reaches the
`--demo` branch at `main.rs:2506`, so even the model-free demo cannot run alone.

**Why it matters.** This is the whole evaluation. Grok Bot's equivalent is one action. Two terminals
plus an ordering constraint the docs do not state is the friction that decides whether someone gets
to a result at all, and it lands on literally every source install.

**The durable fix.** Make `openbot run` start an ephemeral in-process hub and guest when
`--hub` is at its default and nothing answers there — the machinery already exists and is exactly
what `up` composes (`up.rs`), and `openbot-cli` already depends on `openbotd` and `openbot-guest` as
libraries. Five steps and two terminals collapse to three steps and one. Keep `openbot up` for the
case it is actually for: a long-lived hub several clients share.

**How to prove it.** A test that runs `openbot run --demo --approve auto "prove it"` as the very
first command against a fresh home with no hub anywhere and asserts a completed run. It fails today
with `os error 10061`.

---

### F-CD4 — eight commands and the editor bridge fail with a bare winsock errno and no remedy
`P0` · `reach: all users` · `crates/openbot-cli/src/watch.rs:156`, `main.rs:1898`, `:1964`, `:2008`, `:2026`, `:2834`

**What is true now.** With no hub running, every one of these prints the identical line and nothing
else — I ran all five:

```
$ openbot watch    →  Error: connect: IO error: No connection could be made because the target
$ openbot tools    →         machine actively refused it. (os error 10061)
$ openbot servers  →  (exit 1, byte-identical for all five)
$ openbot attach f
$ openbot call fs.read '{"path":"a"}'
```

No hub URL. No mention of a hub at all. No next command.

Those five are four unwrapped `HubClient::connect*` sites (`Command::Servers` `main.rs:1898`,
`Command::Attach` `:1964`, `Command::Tools` `:2008`, `Command::Call` `:2026`) plus `Command::Watch`
(`watch.rs:156`). A sixth unwrapped site is the shared turn helper `run_task` (`main.rs:2834`,
`let (hub, progress) = HubClient::connect_with(hub_url, approver).await?`), which serves three more
commands — `group post` (`main.rs:1324`), `event post` (`:1399`) and `routine tick` (`:1750`) — and
the ACP bridge (`acp/serve.rs:1114`). So six connect sites, **eight commands and the editor bridge**;
that last is where the same string reaches an editor as a JSON-RPC `-32603 Internal error` (F-CD7).

Of the ten hub-dependent commands, exactly one gets this right: `status`, which names the URL and
renders the failure as a row rather than an abort. `run` wraps it and names the wrong remedy
(F-CD5). The other eight do not wrap it at all.

The crate demonstrably knows how to do better twenty lines away: `watch.rs:167` maps the *second*
failure — binding the tool server — to `"cannot watch `{server}`: {e}\n  is openbot-guest
running?"`, but the *first* failure, `HubClient::connect_with` at `watch.rs:156`, has no mapping at
all. The more common failure is the unhandled one.

The crate demonstrably knows how to do better twenty lines away: `watch.rs:167` maps the *second*
failure — binding the tool server — to `"cannot watch `{server}`: {e}\n  is openbot-guest
running?"`, but the *first* failure, `HubClient::connect_with` at `watch.rs:156`, has no mapping at
all. The more common failure is the unhandled one.

**Why it matters.** `os error 10061` is Windows for "nothing is listening". It is indistinguishable
from a wrong `--hub`, a hub on a different port, a hub that crashed, and a hub never started — which
are four different repairs. For a new user it is indistinguishable from the tool being broken.

**The durable fix.** There is one call — `HubClient::connect*` — and it should be reachable from the
CLI through exactly one wrapper that owns this message, so a new hub-dependent subcommand inherits
it rather than re-deciding. One text, naming the URL it tried and `openbot up` as the remedy.

**How to prove it.** A test that runs each of the ten hub-dependent subcommands with no hub and
asserts stderr contains both the attempted URL and the string `openbot up`. Nine of ten fail today:
eight print neither, and `run` prints the URL with the wrong remedy.

---

### F-CD5 — `openbot run`'s only remedy names two binaries the documented install does not provide
`P1` · `reach: all users` · `crates/openbot-cli/src/main.rs:2492`

**What is true now.**

```
Error: could not reach the hub at ws://127.0.0.1:8443/v1/tools: connect: IO error: …(os error 10061)
Start it with `openbotd`, and a guest with `openbot-guest`.
```

`cargo install --path crates/openbot-cli` installs one binary, `openbot`
(`crates/openbot-cli/Cargo.toml:11-13`). `openbotd` and `openbot-guest` exist as crates but are not
on the user's PATH after the install the README prescribes. Meanwhile `openbot up` — the remedy the
README, the `up --help` text and `status`'s own snapshots hint all name — is in the binary the user
just typed. `main.rs:2500` repeats the pattern for the bind failure ("Is `openbot-guest` running and
pointed at this hub?"), as does `watch.rs:167`.

**Why it matters.** This is the first error most people will ever see from OPENBOT, and it sends
them to `command not found`. It is category (a) *and* (d) from the brief: it names internal
components a new user has never heard of, and it quotes back a remedy that does not exist on their
machine. The suggestion is not merely unhelpful — it is wrong.

**The durable fix.** Fold this into the single connect wrapper from F-CD4 and make its remedy
`openbot up`. `openbotd`/`openbot-guest` are the split-deployment answer and belong in a second
sentence, or in the docs, not as the first thing offered to someone whose hub is not running. Same
for `watch.rs:167` and `main.rs:2500`.

**How to prove it.** Assert no error string in `crates/openbot-cli/src/` names a binary the CLI's own
`[[bin]]` table does not produce, unless it also names `openbot up`. It fails on three sites today.

**Sub-point, same class:** `main.rs:2660` suggests `cat token.txt | openbot secret set {name}` for
empty stdin. `cat` is not a command in `cmd.exe` or PowerShell, in a repo whose CLAUDE.md is written
around Windows build hazards and whose `--home` default renders as `C:\Users\…`.

---

### F-CD6 — `openbot status` answers "nothing wrong" on a config that cannot be parsed, and exits 0 regardless
`P1` · `reach: most users` · `crates/openbot-cli/src/config.rs:105`, `crates/openbot-cli/src/status.rs:199`

**What is true now.** `ModelOverrides::applied` calls `load(home).unwrap_or_default()`
(`config.rs:105`); `token_budget` does the same (`config.rs:116`). So `status` cannot distinguish a
missing config from an unreadable one. Against a home whose `config.toml` is not valid TOML:

```
$ openbot status --home …/c_garbage
  hub         ws://127.0.0.1:8443/v1/tools  unreachable — connect: IO error: …(os error 10061)
  model       none configured — `openbot config set --model …`, or use --demo
  bots        0
  routines    none
$ echo $?
0
```

Byte-identical, except for the bots count, to a home with no config file at all. Yet `openbot config
show --home …/c_garbage` in the same binary errors with a precise TOML parse error and exits 1. Two
commands, one home, opposite verdicts. And `status` exits 0 with the hub down, the model
unconfigured and the key missing — the flag-free health check any deploy script would reach for.

**Why it matters.** `status`'s one-line help is "Is anything wrong? One screen: hub, computer, model,
routines". It is the command the product points people at when things break, and the advice it gives
on a broken config — run `config set` — is the command that will then eat the rest of the file
(F-CD1). The `unwrap_or_default` is a two-word decision that turns the diagnostic command into a
misleading one.

**The durable fix.** `Status` should carry the config as `Result`, and `render` should print a
`config` row that says the file failed to parse and quotes the parser's line. That row costs
nothing when the config is fine, and it is the only row that matters when it is not. Separately,
give `status` a nonzero exit when any row is in its red state — `status.rs` already distinguishes
red from green (`status.rs:227`, `:251`, `:272`), so the information is there and simply not
reaching the exit code.

**How to prove it.** `status.rs`'s test module has fourteen cases and every one builds a `Status`
by hand (`status.rs:342 fn base()`), so none can catch this — the bug is in the *gathering*, not the
rendering. Add a case that runs the real binary against a home with malformed TOML and asserts
stderr names the parse failure and the exit code is nonzero.

---

### F-CD7 — `openbot acp` validates the model and the Bot store at startup, but not the hub
`P1` · `reach: most users` · `crates/openbot-cli/src/acp/serve.rs:727-736`

**What is true now.** `serve` opens the Bot store and builds the model before the connection starts,
with this reasoning written next to it:

> *Fail here rather than at the first prompt. An editor that connects successfully and then errors
> on every message looks like a broken agent; an `initialize` that refuses with a reason is a fixable
> misconfiguration.*

The hub is not on that list. Driving the real binary over stdio with no hub running:

```
INIT: {"id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,…}}}
NEW:  {"id":2,"result":{"sessionId":"openbot-e410a982-9c09-4f75-a1ff-5d2db977f1f9"}}
<<    {"id":3,"error":{"code":-32603,"message":"Internal error",
       "data":"connect: IO error: No connection could be made because the target machine actively
        refused it. (os error 10061)"}}
```

Exactly the failure the comment describes — connects successfully, errors on every message — for the
one dependency an editor user is guaranteed not to have running, because it lives in a foreground
terminal they were never told to open (F-CD3).

**Why it matters.** ACP is one of OPENBOT's two front doors and the one that needs no plugin, and this
is its entire first impression. The user sees a winsock errno inside their editor, with no hub named,
no URL, and no remedy — strictly less than what `openbot run` gives them at a shell, which is already
wrong (F-CD5).

*The rest of this bridge is genuinely good, and I checked the things SPEC §9 warns about:*
`session/prompt` hands the turn to `conn.spawn` and returns (`serve.rs:875`, `:929`), so the
event-loop deadlock §9 predicts does not occur; `load_session(true)` is advertised and
`LoadSessionRequest` is implemented with a replay and a truncation notice (`serve.rs:797-858`,
`:709`); and there is not one `fs/read_text_file`, `fs/write_text_file` or `terminal/*` call site in
`crates/openbot-cli/src/acp/`. That last one is the promise §9 says should be *tested* the way
`openbot-guest/tests/isolation.rs` tests its own — and it is not: `tests/reaches.rs` is about what
reaches the model, and no test in the crate asserts the absence of those call sites. The promise
currently holds by care rather than by construction.

**The durable fix.** Add the hub to the startup preflight beside the model and the Bot store, with a
message naming the URL and `openbot up`, so a misconfiguration refuses `initialize` with a reason —
which is the rule the file already states. Better still, once F-CD3 lands, have `acp` start its own
hub the same way `run` would; an editor spawning a subprocess is precisely the case where "there is
a second thing to run" is unacceptable.

**How to prove it.** `tests/acp_live.rs` already spawns the real binary and speaks JSON-RPC to it
(`acp_live.rs:29-40`). Add a case with no hub that asserts `initialize` fails with a message naming
the hub — and a second one asserting no `fs/` or `terminal/` string appears in `src/acp/`, so §9's
promise is enforced rather than merely kept.

---

### F-CD8 — a mistyped table name is silently ignored, and `permission ls` then reports the permissive default as normal
`P1` · `reach: some users` · `crates/openbot-cli/src/config.rs:22` (no `deny_unknown_fields`)

**What is true now.** `Config` accepts unknown top-level tables. A one-letter typo:

```
$ cat config.toml
[permision]
rules = [ { tool = "shell.exec", action = "deny" } ]

$ openbot permission --home … ls
no rules — the shipped default applies: read is free, change asks
$ echo $?
0
```

The rule is not merely dropped — the tool states, positively, that the permissive default is in
force, in the same tone it would use if that were what you asked for. `[modle] id = "grok-4-5"`
behaves the same way: accepted, ignored, `config show` prints `model (not set)`, exit 0.

The README states the opposite principle: *"A rule that cannot be understood stops the hub rather
than being skipped: a rule that is silently dropped is a security failure, not a warning."* That
holds for a malformed **rule** — `rule_from` (`config.rs:150`) refuses beautifully, including a
paragraph explaining why `pattern` is rejected rather than guessed. It does not hold for a malformed
**table**, which is the more likely typo and has the same consequence.

**Why it matters.** Someone who wrote a `deny` rule and mistyped the header believes `shell.exec` is
forbidden. It is not, and the command whose job is to tell them says everything is normal. That is
the exact failure mode the README's sentence exists to rule out.

**The durable fix.** `#[serde(deny_unknown_fields)]` on `Config` and its nested structs, so an
unrecognised table is an error with the key quoted. The forward-compatibility objection —
`edit_rules`'s doc comment wants unknown keys *preserved* — is about writing, not reading, and both
can be true: preserve on write (F-CD1), refuse on read. If a namespaced escape hatch is wanted, make
it explicit (`[x-…]`) rather than making every typo one.

**How to prove it.** A test that loads `[permision] rules = [...]` and asserts an error naming
`permision`. It passes silently today.

---

### F-CD9 — `OPENBOT_HOME` is ignored by `openbot computer`, whose destructive verbs then act on the wrong store
`P1` · `reach: some users` · `crates/openbot-cli/src/main.rs:411`

**What is true now.** Every subcommand takes `--home`/`OPENBOT_HOME`. `Computer` alone takes
`--store`/`OPENBOT_STORE` (`main.rs:411`). So:

```
$ OPENBOT_HOME=<a fresh empty directory> openbot computer snapshots
snap-000000         1 files      191 B  scheduled
snap-000001         1 files      191 B  scheduled
…
```

Twenty-eight snapshots — from `C:\Users\Mandar\.openbot`, not from the home the environment named.
Byte-identical to the run with no environment variable set at all.

CLAUDE.md is explicit that this is the class of bug the project already fixed once:

> *The default home is `~/.openbot` … resolved once in `openbot_proto::default_home` so the CLI and
> the window cannot disagree about where a person's Bots are. **They did once.***

The single resolution is honoured — both flags default to `DEFAULT_HOME` — but the *override* is not,
so the CLI now disagrees with itself.

**Why it matters.** `computer` owns `restore` and `prune`, which delete data. Anyone running a work
home and a personal one via `OPENBOT_HOME` — the mechanism the docs advertise for exactly that — will
have `computer prune --keep 3` silently prune the other machine's snapshots, with output that looks
correct because a store *was* found and snapshots *were* listed. There is no error to notice.

**The durable fix.** `computer` should read `OPENBOT_HOME` like everything else. If a store genuinely
needs to be separable from a home, keep `--store` as the override and let it *default* from
`OPENBOT_HOME` — one variable answers "which account", always. And print the resolved store path in
`computer snapshots`/`status` output, so acting on the wrong one is visible rather than inferred.

**How to prove it.** A test that sets `OPENBOT_HOME` to a temp dir containing a store with one
snapshot, runs `computer snapshots`, and asserts exactly one row. It reads the real home today.

---

### F-CD10 — `openbot config show` cannot answer "is my config right?"
`P1` · `reach: most users` · `crates/openbot-cli/src/main.rs:1487` (`ConfigCmd::Show`)

**What is true now.** Its help says "Print the resolved settings and where they came from". It prints
five model keys and nothing else:

```
$ openbot config --home … show      # on a home whose [permission] rule the hub will reject
…\config.toml
model        grok-4-5
dialect      anthropic
base_url     https://api.x.ai
api_key_env  XAI_API_KEY  (NOT set)
max_tokens   8192
$ echo $?
0
```

Three gaps, all provable: (a) `[permission]` is neither shown nor validated, so a config `openbot
up` refuses to start on shows clean here while `openbot permission ls` — reading the same file —
errors; (b) `token_budget` is a documented config key and a global flag and is absent from the
output; (c) on a home with **no** `config.toml` at all, the output is identical and still names the
path as though it were the source, so "where they came from" is answered wrongly whenever the answer
is "nowhere".

**Why it matters.** `config show` is the only command whose entire purpose is to tell you your config
is right, and it does not read most of the file. The user's next stop is `openbot up`, which fails
with a rule error `config show` was in a position to have caught, having just told them everything
was fine.

**The durable fix.** `config show` should render the *whole* resolved config — model, permission
rules, token budget — through the same `config::policy` path `up` uses, marking each value with its
provenance (file / flag / env / default), and exit nonzero when the file exists but cannot be fully
understood. It should say "no config file — showing defaults" when there is none. That makes it the
preflight for `openbot up`, which is what people already assume it is.

**How to prove it.** A test asserting `config show` exits nonzero on any config where
`config::policy` errors, and that its output lists every rule `permission ls` lists. Both fail today.

---

### F-CD11 — doc comments are attached to the wrong commands, so `openbot --help` misdirects
`P1` · `reach: all users` · `crates/openbot-cli/src/main.rs:268`, `:287`, `:457`, `:484`, `:757-767`, `crates/openbot-cli/src/config.rs:275`

**What is true now.** `openbot --help`, the first screen anyone reads:

```
  permission  Credentials the hub uses on a Bot's behalf
  secret
```

`permission` manages *policy rules*; the line describing it is the description of `secret`, pasted
above the wrong variant at `main.rs:268`. `Secret` has no doc comment at all (`main.rs:287`), so the
command that actually manages credentials is a blank line in the list. `openbot permission --help`
shows the two doc comments fused into one incoherent paragraph:

> *Credentials the hub uses on a Bot's behalf. Values are held by the control plane and attached to
> outbound requests at the moment of the call. The guest never receives one, so a Bot that reads a
> malicious web page cannot exfiltrate a token it was never given. What may run without asking, what
> must be approved, and what is refused.*

Same defect in `openbot bot --help`:

```
  set     Copy a Bot's brief under a new name. The conversation is not copied. Change a Bot's name, title or description
  dup
```

`Set` (`main.rs:457`) carries `Dup`'s first line prepended to its own; `Dup` (`main.rs:484`) is
blank. And two flags are undocumented everywhere they appear: `--max-tokens` has no doc comment on
`ModelOverrides` (`config.rs:275`), so it renders as a bare `--max-tokens <MAX_TOKENS>` followed by
an empty line on all twenty-odd subcommands; and all five of `config set`'s own flags
(`main.rs:757-767`) are undocumented, so `openbot config set -h` — the command `status` tells you to
run — documents none of what it does.

**Why it matters.** This is not naming taste; it is the command surface lying about itself. A user
looking for credential management reads `permission` and never opens `secret`. A user looking to
rename a Bot reads `set` and finds a sentence about copying. In a CLI whose help text is otherwise
its strongest asset, these are the four places where reading the help makes you less correct than
not reading it.

**The durable fix.** A test, not a proofread. Assert that every `Command`/`*Cmd` variant renders a
non-empty short description in its parent's help, and that every `#[arg]` renders a non-empty one —
clap already exposes both. Then the class cannot recur, which matters because the fused comments say
a variant was reordered and the compiler had nothing to say about it.

**How to prove it.** Walk `openbot --help` and every subcommand's help, asserting no listed command
or option has an empty description. It fails on at least four sites today.

---

### F-CD12 — every command carries the model flags, so `--help` lists mostly noise
`P1` · `reach: all users` · `crates/openbot-cli/src/config.rs:257-286` (`global = true`), `main.rs:57`

**What is true now.** `ModelOverrides` is `#[command(flatten)]`ed at the root with every field
`global = true`, so all six model flags plus `--hub` and `--server` render on every subcommand.
Measured:

```
$ openbot servers -h        # 9 option lines; 1 (--hub) is relevant.
      --hub … --server … --model … --dialect … --base-url … --api-key-env …
      --max-tokens … --token-budget …
$ openbot bot ls -h         # 11 option lines; 3 (--all, --home, --json) are relevant.
```

`openbot bot --help` is 69 lines. `openbot secret ls --help` is 59 lines to document a command that
takes one meaningful flag. `openbot computer --help` offers `--server` ("Tool server to bind for
this invocation") on a command whose own help says it "does not go through the hub", alongside
`--volume`, whose default is the same string.

**Why it matters.** The brief asks whether `--help` teaches or lists. This one teaches — the prose is
excellent — and then buries it: on `servers`, eight of nine option lines are about a model the
command never contacts. A reader scanning for the flag they need is reading noise 80% of the time,
and the density is worst on the small read-only commands people run most.

**The durable fix.** `ModelOverrides` should be flattened into the subcommands that build a model
(`run`, `acp`, `status`, `group post`, `event post`, `routine tick`, `config set`), not made global.
The same for `--server`, which belongs on the commands that bind one. This is one attribute per
field, and it roughly halves every help screen in the tree.

**How to prove it.** Assert that `openbot servers -h` and `openbot bot ls -h` list no `--model`,
`--dialect`, `--base-url`, `--api-key-env` or `--max-tokens`. Both fail today.

---

### F-CD13 — one directory has four names, and the one in the docs does not work in the obvious position
`P2` · `reach: some users` · `crates/openbot-cli/src/main.rs:77`, `:336`, `:411`, `:415`

**What is true now.** The same account directory is `--home` on eighteen subcommands, `--store` on
`computer` (F-CD9), and is further subdivided by `--volume`, whose default (`openbot-workspace`)
collides with `--server`'s. `--home` is declared `global = true` on each subcommand
(`main.rs:77` and others), which propagates it *downward* to that subcommand's children but never
*upward* to the root — so it is absent from `openbot --help` and this fails:

```
$ openbot --home /tmp/x bot ls
error: unexpected argument '--home' found
  tip: 'bot --home' exists
```

Clap's tip rescues the interaction, which is why this is a P2 and not higher. `search` additionally
declares `--home` *without* `global` (`main.rs:336`), so it alone does not propagate — invisible
today because `search` has no children, and a trap the moment it gains one.

**Why it matters.** `--home` is the flag every multi-account or CI user needs, it is the one flag the
docs mention by name most often, and it is the one flag that does not work where a user would first
try it. Combined with F-CD9, "which account am I operating on" has no single answer in this CLI.

**The durable fix.** Promote `--home`/`OPENBOT_HOME` to a root-level global beside `--hub`, declared
once, and delete the eighteen copies. Derive `--store` from it. One flag, one declaration, one place
it is documented.

**How to prove it.** Assert `openbot --home <tmp> bot ls` and `openbot --home <tmp> computer
snapshots` both operate on `<tmp>`. Both fail today, for different reasons.

---

### F-CD14 — `openbot bot rm` destroys a Bot, its conversation and its routines with no confirmation and no rehearsal
`P2` · `reach: some users` · `crates/openbot-cli/src/main.rs:1149`

**What is true now.** `bot rm` resolves the name and deletes immediately. It reports the damage
afterwards, and the comment beside it shows the author knew the stakes:

```rust
println!("deleted {}", b.id);
// Report what went with it. Deletion is irreversible, and
// an attached routine should not be discovered by noticing it stopped running.
if gone.routines > 0 {
    render::outln!("  {} routine(s) — they will not run again", gone.routines);
}
```

There is no `--yes`, no prompt, and no `--dry-run` — though `routine tick` has `--dry-run`
(`main.rs:828`) and `computer restore` takes a safety snapshot first so it is reversible. `bot rm`
sits between two commands that were designed for rehearsal and reversal, and has neither. `bot
forget` and `group rm` are the same.

**Why it matters.** A Bot is the product's durable unit — a conversation, a brief, an inbox and a
schedule. Deleting one is the largest destructive act the CLI offers and it is the one with the
least friction. The consequences are printed at the only moment they cannot be acted on.

**The durable fix.** Print the same summary *before* deleting and require confirmation on a TTY,
with `--yes` for scripts — reusing `approve.rs`'s existing TTY detection (`approve.rs:44`), which
already knows how to degrade a prompt when there is no terminal. `--dry-run` should print the
summary and stop, matching `routine tick`.

**How to prove it.** A test asserting `bot rm` on a Bot with a routine, run with stdin not a
terminal and no `--yes`, refuses and leaves the Bot on disk. It deletes it today.

---

## What I could not check

- **`openbot up` in the running state.** The brief forbids starting a daemon, so everything about
  `up` is read from `up.rs` and from its banner text (`up.rs:400-470`), not observed. I could not
  check what it prints when `127.0.0.1:8443` is already taken, whether the guest-registration
  timeout at `up.rs:397` ("the guest never registered with the hub") is reachable in practice, or
  whether the volume-attach message at `up.rs:181` fires on a second `up`.
- **Three of F-CD4's eight commands, empirically.** I ran `watch`, `tools`, `servers`, `attach` and
  `call` with no hub and saw the bare errno. `group post`, `event post` and `routine tick` I did
  *not* run — they need a Bot and a model — so their inclusion rests on tracing all three to the
  same unwrapped `HubClient::connect_with` in `run_task` (`main.rs:2834`, called from `:1324`,
  `:1399`, `:1750`). The ACP bridge's share of that site I did observe directly.
- **Everything downstream of a live hub.** `openbot run` against a real or demo model, the approval
  card in `approve.rs`, the streaming renderer in `render.rs` (`Renderer::event`, `render.rs:264`),
  the HTML transcript in `html.rs`, `openbot watch`'s viewer page and its takeover flow
  (`watch.rs`/`watch.html`), and `attach`. These are the largest untested-by-me surface in my scope,
  and the run renderer in particular is a first-ten-minutes surface I am reporting nothing about.
- **The desktop client's use of the ACP bridge.** `crates/openbot-desktop` and `openbot-app` belong
  to another department; I checked only the agent side of the wire.
- **Whether `openbot up` actually refuses the README's config** (F-CD2). I traced `up.rs:278` →
  `config::policy` → `rule_from`, and proved the same call path errors via `permission ls`, but did
  not observe `up` itself refusing to start.
- **`cargo install --path crates/openbot-cli` end to end.** I did not run it; the claim that it
  produces only `openbot` is read from `crates/openbot-cli/Cargo.toml:11-13`, and the claim that it
  does not trip CLAUDE.md's sidecar requirement is read from the dependency graph, not observed.
- **Non-Windows behaviour.** Every command was run on Windows 11. The `os error 10061` in F-CD4 is
  the Windows spelling of a connection refusal; the finding is about the missing remedy, which is
  platform-independent, but the exact text a Linux user sees will differ.
- **`CONTRIBUTING.md` as a contributor path.** I read it and it is short, specific and honest, but I
  did not attempt the loop it prescribes (`cargo fmt` / `clippy` / `test`), so I cannot say whether
  a fresh clone passes it.
