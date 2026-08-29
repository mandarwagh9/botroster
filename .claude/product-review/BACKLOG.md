# Backlog

Six departments produced 78 findings. The full text of each lives in `reports/`; this file is the
**one ranked order** they get worked in, because six reports with six priority orders is six
opinions and no plan.

Ranked by `reach × cost-of-living-without`, per `CHARTER.md` §0 — **not** by severity label and not
by how interesting the fix is. A P0 that reaches few people loses to a P1 that reaches everyone.

Status: `open` · `doing` · `done <commit>` · `NEEDS REVIEW` · `dropped <reason>`

---

## The headline

**Three of the top five are features that are fully built and never run.** The routine scheduler
parses cron correctly, computes due-ness correctly, and executes correctly — and nothing calls it.
`DIRECTION.md`'s design system was adopted as launch decision #1 and never implemented.
`AgentConfig::context_budget` is the one knob that makes a small model usable and its only caller in
the workspace is a test. This is not a codebase that needs more features. It needs the last inch
wired on the ones it has, and that is a much better position to be in than the reverse.

The second theme: **the security story is stronger in design than in deployment.** The gate really is
in the hub, `Secret` really cannot be serialised, hooks really are fail-closed — and then the hub
accepts any peer on a fixed port, and `shell.exec` inherits the environment holding the credential.
The architecture is sound and the perimeter is missing.

---

## Tier 1 — the product does not do what it says

### T1-1 — routines never fire. `done`
`P0` · reach: everyone who creates a routine · `reports/parity.md` §3, `crates/botroster-cli/src/up.rs:236`

The cron parses, the due-check works, `tick` runs what is due, and **nothing calls `tick`**. The only
timer in `up` is the snapshot timer. A user who sets a 9am routine and closes the laptop gets
nothing, having been given no reason to think they also needed a crontab entry.

*Durable fix:* a supervisor loop beside `snapshot_on_a_timer`, which is the shape to copy — it
already handles the "one failed tick is not a reason to stop" case. Not a shell-out to system cron:
that reintroduces the platform divergence `up` exists to hide.

### T1-2 — a long conversation 400s against every vendor. `done`
`P0` · reach: most users · `reports/agent-model.md` F-AM2

`history(id, Some(40))` takes a raw line tail, so the window frequently starts on a `tool_result`
whose `tool_use` was excluded. Both dialects emit it and both vendors reject it. It heals on retry,
which is exactly why nobody will report it — and why a routine loses a whole firing.

*Durable fix:* the window boundary must be chosen over turn units, not lines. A tail that can split
a pair is wrong however many lines it keeps.

### T1-3 — the model cannot click what it can see. `done` (F-GT4 and F-GT5)
`P0` · reach: most users · `reports/guest-tools.md` F-GT4, F-GT5

`browser.read` returns `innerText`; `click`/`fill` require CSS selectors that nothing in the output
ever emits. The model is asked to guess selectors for a page it has only seen as a wall of text.
`click` also never waits for navigation, so it returns the *previous* page's title.

*Durable fix:* perception and action must share one coordinate system — read emits stable handles,
act takes them. This is the difference between a browser tool that demos and one that works.

### T1-4 — `config.toml` is the control surface and the tools around it lie. `done` (F-CD2, F-CD6, F-CD10 partial; F-CD1 `config set` data loss still `open`)
`P0` · reach: most users · `reports/cli-devex.md` F-CD1, F-CD2, F-CD6, F-CD10

The README's own example is rejected by the parser that reads it (`action = "ask"` versus
`require_approval`). `config show` reports success on a file `up` refuses to start on. `status`,
whose help is "Is anything wrong?", says nothing is wrong about a file that will not parse. And
`config set` — the repair those two send you to — silently deletes every part of the file it does
not recognise, sixty lines below a doc comment saying that is worse than having no config editor.

*Durable fix:* one validation path, reachable from `status`, and an editor that preserves what it
does not understand. Four findings, one root cause: validation exists and is excellent, and is filed
where nobody looks for it.

### T1-5 — a dead tool server hangs the run forever. `done` (hub side; agent-side timeout and `tool.cancel` wiring still `open`)
`P0` · reach: some users, total when hit · `reports/runtime-security.md` F-RS2 · `reports/agent-model.md` F-AM1

No timeout on a forwarded `tool.call`, no cancel path, no disconnect propagation.
`WorkspaceUnavailable` is defined in the protocol and used nowhere. The only exit discards the run.

---

## Tier 2 — the security story is not yet what the README implies

### T2-1 — the hub authenticates nobody. `done`
`P0` · reach: all users · `crates/botrosterd/src/server.rs:62`

`let principal = dev_principal()` — no token, no `Origin` check, on a fixed `127.0.0.1:8443`. Any
local process, and plausibly any web page the user visits, can open a session. Approvals are
addressed to the session **owner** and a stranger cannot answer someone else's — that part is built
correctly — but a rogue connection opens *its own* session, so it is the owner, and approves its own
`shell.exec`.

*Durable fix:* two layers. Reject any upgrade carrying an `Origin` header — a native client never
sends one, so this closes the browser vector at zero cost to every real client. Then a token
generated at start, written `0600` beside the home, presented in `hello`. The gate is perfectly
built and then asked to trust whoever knocks.

**Closed 2026-08-29.** Both halves are in. `Hub::admitting(Admission)` is checked first in `register`,
ahead of the protocol-version check, so an unadmitted peer learns nothing about the hub; the compare
is constant-time. `boot::hub_from_home` takes the admission as an argument rather than reading the
home, so the decision is visible at the call site and cannot be lost by reordering two statements.
`up` always requires the token it just generated; `botrosterd` requires whatever its home holds and
warns loudly when that is nothing, because refusing to start would break deployments to defend a
home whose owner has not been given a way to write one.

Three things this turned up that were not in the finding:

- **`--home` did not reach the token lookup.** The flag is declared per subcommand with
  `env = "BOTROSTER_HOME"`, so passing it sets no variable and `hub_token` read the *default* home —
  presenting the wrong token and being refused with a message about a token the person had.
  `proto::use_home`, set once from `home_from_argv`, fixes it for all eleven `connect` call sites at
  once; `the_scanned_home_is_the_home_clap_parses` holds the scanner to clap's answer.
- **A refused handshake read as "no hub is running", in two places.** `hub_or_start`'s
  `if let Ok(Ok(_))` swallowed every failure alike, so a wrong home silently started a *second*
  stack on top of the first — two computers, two homes, and the double-firing routine `up`'s own
  docs warn about. Fixed there, and then found again in the window: `hub::reach` had no `Refused`
  variant, so `botroster-app` called `hub::start` on a hub that was already holding the port, and
  the person saw "address in use" about a computer that was running. Both now distinguish it, by one
  marker (`botroster_proto::REFUSED_PREFIX`) that a test in `botroster-agent` holds the wording to.
  The second half was found by review, not by the suite, which is the finding under T2-1a below.
- **The rule was applied at four of six sites.** `attach::put` and `settings::test_run` were missed
  by hand. `every_child_pointed_at_a_hub_is_given_the_token_for_it` sweeps the crate's source and
  found both; it is the instrument, not the memory, that keeps this true.

### T2-1a — what T2-1 left, all small
`P2` · reach: some users

- **No UI for a foreign hub's token.** A window pointed at a hub on *another machine* cannot be
  given its token from inside the window. `BOTROSTER_HUB_TOKEN` in the environment before launch
  works and is documented in `hub::token_at`; a field beside the hub URL in Connect is the finished
  version. Until then the window at least *reports* the refusal rather than starting a second
  computer, which is the half that mattered.
- **`botrosterd` warns where its neighbour fails.** `boot.rs` says of a malformed hooks file:
  *"starting anyway would run unguarded while the operator believes otherwise."* A `botrosterd`
  bound to a non-loopback address on a home with no `hub.token` is that sentence exactly, and it
  gets a `warn!`. Refusing to start for **non-loopback + `Admission::Anyone`** cannot break a
  deployment that was ever safe; the warning is right for loopback and only there. Left out of T2-1
  to keep that change one thing.
- **The split-home configuration is supported and barely tested.** `Config` takes `home` and `hub`
  independently and `connect` accepts both, but every live test now uses the production shape where
  they are one path. `start_live`'s two new tests cover `reach`; the engine and the shell commands
  are not covered for it.

### T2-2 — `shell.exec` hands over the credential the crate graph protects. `done` (environment half; the filesystem half needs a real isolation boundary and is recorded as such in isolation.rs)
`P0` · reach: all users · `reports/guest-tools.md` F-GT1

`isolation.rs` proves no crate edge from the guest to `botrosterd` and panics with "this is the reason
a prompt injection cannot exfiltrate a credential". Then `botroster up` runs hub, secret store and
guest in one process, and `shell.exec` inherits its environment — so `cat ~/.botroster/secrets.json`
is one approved command away, and the model key is already in the environment.

*Durable fix:* `env_clear` plus an explicit allowlist. The crate-graph invariant is real and worth
keeping; it just does not deliver what its own message claims while the process boundary leaks.

### T2-3 — `browser.open` is an unprompted exfiltration primitive. `open`
`P1` · reach: all users · `reports/guest-tools.md` F-GT3

Allow-listed, any URL, no approval. Also a loopback SSRF reach. And `browser.screenshot` silently
overwrites any workspace file while `fs.write` asks.

### T2-4 — Chrome's own sandbox was disabled by a false premise. `done 6f477d4`
`P0` · reach: all users — **fixed.** Opt-in via `BOTROSTER_BROWSER_NO_SANDBOX=1`; 19 live browser
tests pass with it enabled. The vocabulary that justified it is corrected across five crates and
`review.sh` G5 now scans Rust source so it cannot return.

---

## Tier 3 — the first ten minutes

> **Re-ranked 2026-08-25.** The order below was set by reach × cost-of-living-without *within the
> product as it is*. Judged against what the product should feel like, that put "five steps and two
> terminals" underneath "the config tools lie", which was wrong: the first is what everyone meets
> first, and the second is what they meet only if they get that far. T3-1 was taken out of order for
> that reason, and the rest of this tier should be read as higher than its position suggests.

### T3-1 — a first result costs two terminals and nobody says so. `done` (F-CD3 and F-CD5; `run` starts a computer if one is not up)
`P0` · reach: all users · `reports/cli-devex.md` F-CD3, F-CD5

Five steps and two terminals, and nothing warns you until `up` has already taken the first. `run`'s
one remedy names two binaries the documented install does not put on your PATH.

### T3-2 — eight commands fail with a bare winsock errno. `done`
`P0` · reach: all users · `reports/cli-devex.md` F-CD4

`os error 10061` names neither the hub nor a remedy. One command of ten gets this right, so the
good version already exists in the tree.

### T3-3 — help text on the wrong commands. `open`
`P1` · reach: all users · `reports/cli-devex.md` F-CD11, F-CD12

`permission` documented as credentials, `secret` blank, `bot set` carrying `dup`'s text. Model flags
on every command: 8 of 9 option lines on `botroster servers -h` are noise.

---

## Tier 4 — the client stops at the third dimension

The design department's verdict: the client was designed as a sequence of still frames, and it shows
the moment a Bot works for longer than a screenshot.

- **T4-1** `P1` all — `DIRECTION.md`'s tokens were never implemented; the build ships a purple accent
  DIRECTION bans. **This resolves carried-forward decision #3 in `CHARTER.md` §4** — the derivation
  was not merely unapplied, it was never applied at all. `reports/design-client.md` F-DC1
- **T4-2** `P1` all — no elapsed time anywhere in the window; `serve.rs:469` drops the `elapsed_ms`
  the runtime already measures and the CLI already renders. F-DC3, F-DC4
- **T4-3** `P1` most — the log yanks to the bottom while you are reading it. F-DC7
- **T4-4** `P1` most — nothing notices the runtime dying; "connected" never expires. F-DC8
- **T4-5** `P1` all — no loading state on any async surface; a failed connector read silently erases
  the routines list. F-DC5
- **T4-6** `P1` all — the window teaches "a computer it works on"; SPEC §348 orders it to say loudly
  that the computer is **shared**. Teaching the differentiator backwards. F-DC2

---

## Tier 5 — the open-source thesis, cheaply

### T5-1 — xAI's commercial caps are hard-coded in an unmetered self-hosted product. `open`
`P1` · reach: some users, but it is the whole argument · `crates/botroster-bots/src/lib.rs:37,1594,2236`

`MAX_BOTS = 50`, `MAX_GROUP = 6`, `MAX_ROUTINES = 50`. These are a managed platform's billing
limits, copied into a product whose entire pitch is that you run it yourself. There is even a test
pinning `MAX_BOTS == 50`. Cheap to make configurable, and it removes an argument we cannot win.

### T5-2 — the persona is a string literal. `open`
`P1` · reach: most users who chose OSS to change behaviour · `reports/parity.md` §3

No editable system prompt, no per-Bot model. The parity report calls this the customization gap that
most directly contradicts the pitch: someone chose an open product precisely to change this.

### T5-3 — `context_budget` is unreachable from the product. `open`
`P1` · reach: everyone running a local model · `reports/agent-model.md` F-AM7

Set by exactly one caller in the workspace, and it is a test. The one knob that makes a small model
viable, and no surface exposes it — directly against the Ollama-by-default path shipped last week.

---

## Deliberately not doing

Per `CHARTER.md` §0 and the explicit instruction not to spend effort on what 1% of people hit.

- **Windows 8.3 short-name path escapes** (`reports/guest-tools.md` F-GT12, partial). Real, and it
  costs a week to do properly for a case that needs an attacker already able to choose paths.
  Reconsider if the guest ever becomes a genuine boundary.
- **Per-endpoint parameter variation for six OpenAI-compatible gateways** (F-AM12). Speculative
  until a specific endpoint is measured failing. The `stream: false` fix shipped last week is what
  evidence-led looks like here.
- **`bot rm` confirmation** (F-CD14). Real, small, and a `--dry-run` on a destructive command is
  worth more than a prompt people learn to hit through. Folded into T3-3's pass.
- **The `session_attach_server` phantom method** (F-RS12). Advertised and unimplemented, but nothing
  calls it. Delete the advertisement when next in that file.
- **`main.js` at 2,632 flat lines** (F-DC12). Real, and restructuring it competes with T4-2 through
  T4-6 for the same file. Do it *as* those land, not as its own commit.


---

## Tier 6 — the thing that puts BOTROSTER ahead rather than level

Added 2026-08-29 from `PROPOSAL-run-records.md`, which is the reasoning, the competitive analysis
and the explicit list of what must never be claimed about it. Read that before starting any of
these; the short version is that every product in this category competes on *prevention* and nobody
competes on *what happened, and what of it can be taken back*.

Ordered so each step ships something usable and none of it is wasted if the next is dropped.
Steps T6-1 to T6-3 need no change to `botroster-store` at all.

### T6-1 — a run is not an artefact. `done`
`P1` · reach: all users · `PROPOSAL-run-records.md` §3.1

Nothing durable records what a run did. `conversation.jsonl` holds the messages; the tool calls,
their results, their `elapsed_ms` and — the part nobody else has — **the approval decisions** pass
through `hub.rs:1046` and are not written anywhere. The hub is the only correct writer: a record the
recorded thing can edit is not a record, for the same reason the policy gate is not in the agent.

*Durable fix:* `<home>/bots/<id>/runs/<run-id>.jsonl`, append-only, beside the conversation log that
already works this way. Must not be able to contain a credential; that gets its own failing test.

**Closed 2026-08-29.** `crates/botrosterd/src/record.rs`, written at both ends of a call: the five
endings decided inside the hub, and `finish_relay` for everything forwarded to the guest — which is
the one place a forwarded call ends, and therefore the only place that catches the two endings that
are *not* a reply. `botroster bot record <bot>` reads it back.

Decisions worth keeping:

- **`sessions/`, not `runs/`.** The hub sees sessions; it does not see turns or prompts. A "run"
  means a turn to anybody reading it, and for the desktop client — one session across a whole
  conversation — that would be false. Naming the directory after what is in it cost nothing.
- **Every captured value carries the full byte length and a SHA-256 of the *whole* value** beside
  the 4 KiB kept. Two different four-megabyte reads must not compare equal because their first four
  kilobytes match, and that is exactly the comparison T6-3 makes.
- **Arguments are canonical.** `serde_json`'s `Map` is a `BTreeMap` in this build, so keys sort and
  the same call always records the same bytes. Without it, T6-3's first act would be re-normalising
  every record ever written.
- **One writer task behind a channel.** The hub never blocks on a disk to answer a tool call, and
  the file's order is the record's order — `seq` is assigned by the writer, so two concurrent calls
  finishing in either order still produce a file whose numbering and whose lines agree.
- **A session naming no Bot is not recorded**, and there is a test asserting it, because the
  alternative is a script's one-off calls landing in a teammate's history.

Five mutations were run against it. The one that mattered: removing the record from `finish_relay`
left every test passing, because nothing covered the path every `fs.*` and `shell.exec` call takes.
That is now `a_call_that_reaches_the_guest_is_recorded_with_how_it_ended`, against a real guest.

### T6-2 — a past run cannot be re-run. `open`
`P1` · reach: most users · `PROPOSAL-run-records.md` §3.2

*Durable fix:* a stub at the hub's forwarding point that answers every `tool.call` from the
recording instead of forwarding it. Enforced by the same code path that enforces policy, so replay
touches nothing — no file written, no page opened, no outbound call — which is what makes it safe to
run on the machine a person actually works on. A call the recording does not contain is a
**divergence**, not an error, and naming the step where it happened is the output.

### T6-3 — changing a Bot's brief is unfalsifiable. `open`
`P0` · reach: everyone who chose an open product in order to change something

`botroster bot test <bot>`: replay the golden runs, report divergences. This is the surface that
pays for T6-1 and T6-2, and it **unblocks T5-2 and T5-3 rather than competing with them** — the
reason it is frightening to expose an editable persona and a `context_budget` is that nobody can
tell whether turning one broke the Bot. Shipping those knobs without this ships the contradiction
`reports/parity.md` already names.

### T6-4 — the run log does not say what can be taken back. `open`
`P1` · reach: all users · `PROPOSAL-run-records.md` §3.4

Per-step checkpoints, and a badge per row derived from the tool name, which the hub already knows:
`fs.write` can be undone, `shell.exec` only partly (it may `cd` anywhere the user can reach),
`browser.click` not at all (and `Volume::browser_profile` is deliberately outside the snapshot, so a
rewind never restores a login). No competitor tells you which of its actions it can take back.

*Blocked on:* a cheap checkpoint. `Volume::ingest` copies **every** file on every snapshot, so
per-step is O(whole workspace). Needs a separate function using git's racily-clean rule; the durable
path and its two tests must not change.

### T6-5 — branch a run from any step. `NEEDS REVIEW`
`P2` · reach: unknown · `PROPOSAL-run-records.md` §5.4

Fork the world and the transcript at step N, change one thing, run forward, diff the branches. The
most exciting item here and deliberately not scoped: it needs a new volume seeded from a manifest,
which is well past `CHARTER.md` §5's ~200-line rule. Strictly easier once T6-1 to T6-4 exist.
