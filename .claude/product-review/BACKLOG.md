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
`P0` · reach: everyone who creates a routine · `reports/parity.md` §3, `crates/openbot-cli/src/up.rs:236`

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

### T1-3 — the model cannot click what it can see. `open`
`P0` · reach: most users · `reports/guest-tools.md` F-GT4, F-GT5

`browser.read` returns `innerText`; `click`/`fill` require CSS selectors that nothing in the output
ever emits. The model is asked to guess selectors for a page it has only seen as a wall of text.
`click` also never waits for navigation, so it returns the *previous* page's title.

*Durable fix:* perception and action must share one coordinate system — read emits stable handles,
act takes them. This is the difference between a browser tool that demos and one that works.

### T1-4 — `config.toml` is the control surface and the tools around it lie. `open`
`P0` · reach: most users · `reports/cli-devex.md` F-CD1, F-CD2, F-CD6, F-CD10

The README's own example is rejected by the parser that reads it (`action = "ask"` versus
`require_approval`). `config show` reports success on a file `up` refuses to start on. `status`,
whose help is "Is anything wrong?", says nothing is wrong about a file that will not parse. And
`config set` — the repair those two send you to — silently deletes every part of the file it does
not recognise, sixty lines below a doc comment saying that is worse than having no config editor.

*Durable fix:* one validation path, reachable from `status`, and an editor that preserves what it
does not understand. Four findings, one root cause: validation exists and is excellent, and is filed
where nobody looks for it.

### T1-5 — a dead tool server hangs the run forever. `open`
`P0` · reach: some users, total when hit · `reports/runtime-security.md` F-RS2 · `reports/agent-model.md` F-AM1

No timeout on a forwarded `tool.call`, no cancel path, no disconnect propagation.
`WorkspaceUnavailable` is defined in the protocol and used nowhere. The only exit discards the run.

---

## Tier 2 — the security story is not yet what the README implies

### T2-1 — the hub authenticates nobody. `open`
`P0` · reach: all users · `crates/openbotd/src/server.rs:62`

`let principal = dev_principal()` — no token, no `Origin` check, on a fixed `127.0.0.1:8443`. Any
local process, and plausibly any web page the user visits, can open a session. Approvals are
addressed to the session **owner** and a stranger cannot answer someone else's — that part is built
correctly — but a rogue connection opens *its own* session, so it is the owner, and approves its own
`shell.exec`.

*Durable fix:* two layers. Reject any upgrade carrying an `Origin` header — a native client never
sends one, so this closes the browser vector at zero cost to every real client. Then a token
generated at start, written `0600` beside the home, presented in `hello`. The gate is perfectly
built and then asked to trust whoever knocks.

### T2-2 — `shell.exec` hands over the credential the crate graph protects. `open`
`P0` · reach: all users · `reports/guest-tools.md` F-GT1

`isolation.rs` proves no crate edge from the guest to `openbotd` and panics with "this is the reason
a prompt injection cannot exfiltrate a credential". Then `openbot up` runs hub, secret store and
guest in one process, and `shell.exec` inherits its environment — so `cat ~/.openbot/secrets.json`
is one approved command away, and the model key is already in the environment.

*Durable fix:* `env_clear` plus an explicit allowlist. The crate-graph invariant is real and worth
keeping; it just does not deliver what its own message claims while the process boundary leaks.

### T2-3 — `browser.open` is an unprompted exfiltration primitive. `open`
`P1` · reach: all users · `reports/guest-tools.md` F-GT3

Allow-listed, any URL, no approval. Also a loopback SSRF reach. And `browser.screenshot` silently
overwrites any workspace file while `fs.write` asks.

### T2-4 — Chrome's own sandbox was disabled by a false premise. `done 6f477d4`
`P0` · reach: all users — **fixed.** Opt-in via `OPENBOT_BROWSER_NO_SANDBOX=1`; 19 live browser
tests pass with it enabled. The vocabulary that justified it is corrected across five crates and
`review.sh` G5 now scans Rust source so it cannot return.

---

## Tier 3 — the first ten minutes

### T3-1 — a first result costs two terminals and nobody says so. `open`
`P0` · reach: all users · `reports/cli-devex.md` F-CD3, F-CD5

Five steps and two terminals, and nothing warns you until `up` has already taken the first. `run`'s
one remedy names two binaries the documented install does not put on your PATH.

### T3-2 — eight commands fail with a bare winsock errno. `open`
`P0` · reach: all users · `reports/cli-devex.md` F-CD4

`os error 10061` names neither the hub nor a remedy. One command of ten gets this right, so the
good version already exists in the tree.

### T3-3 — help text on the wrong commands. `open`
`P1` · reach: all users · `reports/cli-devex.md` F-CD11, F-CD12

`permission` documented as credentials, `secret` blank, `bot set` carrying `dup`'s text. Model flags
on every command: 8 of 9 option lines on `openbot servers -h` are noise.

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
`P1` · reach: some users, but it is the whole argument · `crates/openbot-bots/src/lib.rs:37,1594,2236`

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
