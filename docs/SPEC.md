# BOTROSTER: v0 architecture spec

An open-source, self-hostable equivalent of Grok Bot: **a team of persistent, named AI teammates
that share one durable cloud computer.**

> `botroster` is a placeholder name (a BOTROSTER is where a flock lives between flights). Whatever it ends
> up called, it must not use "Grok", "Grok Bot", or xAI/SpaceXAI/Anysphere branding: Apache-2.0 §6
> grants patent and copyright, explicitly **not** trademark.

**Companion documents:** [`RECON.md`](RECON.md) (what Grok Bot is, all evidence) ·
[`PROTOCOL.md`](PROTOCOL.md) (the Computer Hub wire protocol, extracted from source).

---

## 1. The one-paragraph thesis

Six iterations of verification established that **the hard technical core is already open source**.
`github.com/xai-org/grok-build` publishes, under Apache-2.0, the entire agent runtime *and* the
Computer Hub: the WebSocket tool-routing fabric with a live production endpoint at
`wss://computer-hub.grok.com/v1/tools`: *and* the guest-side daemon (`xai-workspace-server`) that
runs inside the sandboxed VM. What is **not** published is everything that turns a coding CLI into
Grok Bot: VM provisioning and lifecycle, the multi-Bot layer, routines, approvals policy, the
credential broker, and the clients. **That gap is `botroster`.** We are not rebuilding an agent; we are
building the cloud and teammate layer on top of one that already exists.

## 2. Scope

### In scope (the genuinely closed pieces)
1. **Computer orchestrator**: provision, supervise, update, recover, reset a per-user VM with a
   durable volume.
2. **Bot layer**: named personas with their own conversation, memory, skills, routines; bot-to-bot
   messaging; group chats; handoffs.
3. **Routines**: schedules and event triggers, with run history.
4. **Approvals / policy**: pre-execution interception of tool calls *and* computer actions, with
   synced rules.
5. **Credential broker**: MCP/OAuth tokens held **outside** the VM.
6. **Clients**: desktop and web. (Mobile: later.)

### Adopted, not rebuilt (Apache-2.0, with attribution)
- The agent runtime (`xai-grok-shell`) and tools (`xai-grok-tools`)
- The Computer Hub (`xai-computer-hub-{core,sdk,mcp-adapter}`) and its protocol
  (`xai-tool-protocol`)
- Skills / plugins / hooks / permissions / sandbox formats: which are **Claude Code compatible**,
  so the entire existing skill and plugin ecosystem works unchanged

### Explicitly out of scope for v0
WebAuthn/CTAP forwarding, teach-by-demonstration, iOS, Windows guests, static egress IP pools.
Each is a real feature; none is on the critical path to a working system.

## 3. Component architecture

```
┌── clients ────────────────┐
│  desktop (Tauri) · web    │
└──────────┬────────────────┘
           │ 1. app protocol (WS + JSON, own schema)
┌──────────▼──────────────────────────────────────────────┐
│  CONTROL PLANE  (botrosterd)                                 │
│  ├ identity / OIDC          ├ bot registry + personas    │
│  ├ conversation store        ├ routine scheduler         │
│  ├ approval engine + policy  ├ credential broker ◄── the │
│  └ computer orchestrator ──┐ │                    tokens │
└────────────┬───────────────┼─┴──────────────── live here │
             │               │                             │
   2. hub protocol      3. orchestration API (not the hub) │
             │               │
┌────────────▼───────┐  ┌────▼──────────────────────────────┐
│  COMPUTER HUB      │  │  HYPERVISOR / RUNTIME             │
│  (adopted)         │  │  one guest per user               │
│  WS · JSON-RPC 2.0 │  │  ┌──────────────────────────────┐ │
│  routes harness    │  │  │ guest: botroster-workspace-server│ │
│  ⇄ tool servers    │◄─┼──┤  browser · shell · fs        │ │
└────────────────────┘  │  │  /workspace (durable)        │ │
                        │  └──────────────────────────────┘ │
                        └───────────────────────────────────┘
```

Four processes, four protocols, one rule: **the guest is untrusted.**

## 4. The computer orchestrator

The single most important component, because "context compounds instead of resetting" is the whole
product promise and it is purely a state-management problem.

### Storage split

> **Built in P3** as `botroster-store`, hypervisor-agnostic so a container today and a microVM later
> share it. One correction from building it: the layout below says nothing about *how* snapshots
> are taken, and the obvious answer: hard links: is wrong. A hard link preserves history only if
> every writer replaces files; `fs::write`, `>` redirects and appends all truncate in place and
> would silently rewrite every snapshot pointing at that inode. Snapshots are therefore
> content-addressed: files are hashed and their bytes copied into a blob store, so identical
> content costs one copy however many snapshots hold it and nothing in the live tree can reach a
> blob. `reset` also takes a safety snapshot first, so unlike the product we studied it cannot
> discard work: undoing a mistaken rollback is just another rollback.
>
> **Status: design decision, not observed fact.** Two things *are* verified: the `devbox` sandbox
> profile ("Cloud devbox environments") makes everything writable **except `/data`**, and
> `/workspace` is the documented shared workspace that survives update and recovery. The layout
> below: that `/data` *is* the durable volume and `/workspace` is bind-mounted into it: is our
> inference from those two facts plus the update/recover/reset semantics. It is a reasonable
> reading and a good design regardless, but nothing observed states it. Do not cite it as
> Grok Bot's actual layout.

| Layer | Contents | Survives |
|---|---|---|
| **Base image** | OS, browser, runtimes, the guest daemon | replaced on update |
| **Durable volume** → `/data` | browser profile, credential stores, `/workspace` (bind-mounted), package state we choose to persist | update, recover, VM replacement |
| **Ephemeral** | `/tmp`, manually installed packages, uncommitted app state | nothing: documented as replaceable |

`/workspace` is the user-facing path; it lives on the durable volume. Keep that indirection: it
lets the durable layout change without breaking a single user path.

**This was written and then not done, for the whole of P3 through P7.** `botroster up` ran its guest in
a plain `./workspace` while the volume sat empty beside it, so the entire storage layer: the thing
this section specifies and the README lists as built: operated on a directory the product never
wrote to. `botroster computer status` beside a working agent reported `live 0 files`. Now `up` resolves
its workspace to `<home>/volumes/<server-id>/current` and holds the volume while it runs, so
`snapshot`, `restore`, `prune` and the attach check all act on the files the agent is actually
using. A layer being *built and tested* says nothing about it being *reached*.

### Lifecycle operations
Mirror the three documented operations, because they are correctly factored:

| Op | Effect | Durable volume |
|---|---|---|
| `update` | rebuild guest from the newest base image | preserved, reattached |
| `recover` | replace an unreachable guest | preserved, reattached |
| `reset` | roll back to a snapshot | rolled back: **but a safety snapshot is taken first, so it is undoable** |
| `kill` (admin) | destroy the running guest | preserved; next session provisions fresh |

**Status.** `reset` is built, under the name `botroster computer restore`: the table's verb, but the
CLI says what it does to a workspace rather than what it does to a machine. `update`, `recover` and
`kill` all mean *replace the guest process*, which needs the hypervisor backend to mean anything;
today's guest is a process you started, so there is nothing here to rebuild. They are pending, not
missing by oversight.

**Scheduled snapshots are built.** `botroster up` takes one every 30 minutes and keeps 48: a day of
history, on by default, because a rollback is worth nothing if the only snapshots are the ones
somebody remembered to take. Retention is keyed on the label the schedule writes, so it trims its
own and never a snapshot taken by hand: `prune` is the only irreversible operation in this layer,
and a timer reaching it is that operation with nobody watching. This had to wait for two other
things to be true first: the computer had to actually live on the volume, and `snapshot` and
`prune` had to be safe to run at the same time.

Snapshot the durable volume on a schedule and before every `update`.

**Amended in P3.** This table originally called `reset` "the only destructive op" and said the UI
must warn "you may lose work since &lt;timestamp&gt;": copying the behaviour of the product we
studied, which documents reset as something that "can discard recent unsaved work". Building it
showed the warning was the wrong fix for the wrong problem: snapshots are cheap enough to take
before every rollback, so `reset` returns the safety snapshot it took and undoing a mistaken
rollback is just another rollback. **No lifecycle operation: `update`, `recover`, `reset`: is destructive
any more.** A confirmation dialog that shifts blame to the user is not a substitute for making the
operation recoverable.

`prune` is the exception and the only irreversible operation in this layer: it deletes manifests
and garbage-collects the blobs nothing else references. That is what it is for, and it is why it
takes an explicit keep-count rather than running on a timer: and why the count is now **required**
rather than defaulting to ten. A command that destroys history should not guess how much.

**Snapshot, restore and prune hold the volume while they run.** A snapshot ingests every blob
before it writes the manifest naming them, so a prune overlapping that window collects blobs the
finished snapshot will point at: for the length of a full workspace copy, not an instant. The same
gap lets two snapshots claim one sequence number. The lock is an **OS advisory lock**, so the kernel
drops it when the process ends however it ended; a lock file holding a pid would trade these races
for a stale lock needing a `force-detach`, which is a worse bargain for an operation nobody watches.
Waiting has a deadline, because a command that sits silently is indistinguishable from a hung one.

### Supervision
The guest daemon runs under a supervisor that:
- version-probes the daemon binary via a `--capabilities` manifest before trusting it: **the
  manifest is built** (`botroster-guest --capabilities` prints protocol version, methods and tool ids,
  and a build predating the flag exits non-zero, which is the probe); *no supervisor reads it yet,
  because there is no supervisor*. The mismatch it guards against is handled at the other end
  instead: the hub refuses a handshake whose `protocol_version` it does not speak, and: since a
  refusal is not an outage: the guest reports the hub's own words and **stops**, rather than
  retrying something waiting cannot fix
- exposes in-guest `/ready` and `/statusz` over a **Unix socket** (loopback TCP on Windows guests)
 : *pending*
- daemonizes (double-fork + `setsid`) so it survives the launcher's process-group reap: *pending;
  belongs with the hypervisor backend, since there is no launcher to survive yet*
- **dwells** after a hub-connect failure rather than exiting instantly, so the host can observe the
  failed state: a small, genuinely good idea worth keeping. **Built.**
- **reconnects for as long as the process lives.** Not in the original list, and the most important
  of them: without it, restarting the control plane killed every computer attached to it, silently
 : the hub came back with no tool servers and the next task failed with "no such server".
  Upgrading a hub is routine; repairing every guest by hand afterwards is not. A *first* connection
  that fails still returns, because that is a wrong address rather than an outage. **Built.**

### Runtime choice
| Option | Verdict |
|---|---|
| **Firecracker / Cloud Hypervisor microVM** | ✅ **target.** Real isolation, fast boot, snapshot support, well-matched to one-guest-per-user |
| Docker + gVisor | acceptable **v0 shortcut** for self-hosters without KVM; document the weaker boundary honestly |
| Plain Docker | ❌ not a tenant boundary; do not ship it as one |

## 5. The Bot layer

A **Bot** is a persona bound to a persistent session. Grok Build already supplies both halves:
personas (`[subagents.personas]`, `.grok/personas/*.toml`: behavioural overlays: tone, focus,
contracts) and sessions (auto-saved, resumable, forkable, rewindable). A Bot is their composition
plus identity and scheduling.

```
Bot {
  id, name, title, description, avatar
  persona          -> behavioural overlay
  session_id       -> the durable conversation
  memory           -> stable preferences + role context (separate per Bot)
  enabled_skills[] -> subset of installed skills
  routines[]       -> max 50, matching observed limits
  notification_pref
  state: active | hidden | deleted
}
```

**The description/message split is the memory design and must be preserved:** the description holds
standing rules ("never send external messages without approval"); messages hold task instructions.
It is what makes a Bot a role rather than a chat.

**Duplication copies** profile, settings, enabled skills, routines, avatar: **not** conversation
history, learned memory, or attachments. That distinction is what makes "one Account Health Bot per
region" work.

### Screens: resolved
Evidence: nothing in 1.59M lines of the open runtime implements multi-display work surfaces, and
the docs insist screens are *"separate work surfaces, not separate security boundaries"* while
cookies and logins are shared.

**Decision: one browser process, N logical contexts** (CDP targets / windows): *not* N Xvfb
displays each with its own browser. A single Chromium profile directory cannot be opened by
concurrent processes anyway (profile lock), so the multi-process reading does not compose without a
cookie-sync layer nobody wants. Each Bot gets a context; the "one computer-use task per Bot at a
time" rule is a per-context lock.

That same profile lock decides what a crash costs. A guest killed with `kill -9` leaves its browser
running and the profile locked, so a launch that spawns unconditionally can never succeed again on
that computer. **Built:** the guest reads the port the profile advertises and **adopts** a browser
that is still listening, spawning only when nothing answers. Recovering the session is strictly
better than reaping it: the profile is durable precisely so that signing in survives, and a crash
is the moment that promise is worth the most.

The mirror case is the browser dying while the guest lives. **Built:** the connection carries a
closed flag, set by whichever of the reader or writer notices first, so a call against a dead
browser fails at once instead of waiting out the 60-second call timeout; and the guest holds the
browser somewhere replaceable rather than in a `OnceCell`, checking it is alive before reuse. One
lazy launch that is kept forever is the right shape only while nothing can take it away.

### Bot-to-bot messaging
Async message-passing between Bots, visible in the transcript. Receiving Bot wakes, handles, may
reply later. Group chats hold 2-6 Bots with `@mention` routing and `@everyone`. **Require a single
owner per stage**: the documented failure mode is duplicate work from parallel handoffs, and that
is worth encoding as a constraint rather than a guideline.

## 6. Approvals: where BOTROSTER diverges

### 6.0 Where enforcement lives (decided before P2)

The obvious place to intercept is the agent loop, right before `hub.call_tool`. It is also the
wrong place, and getting this wrong would make every later approval feature decorative.

§7 already states the guest is untrusted. **The harness is not a trust boundary either**: it is a
client. It runs wherever the user runs it, can be modified, and in a hosted deployment is not even
the same process as the control plane. An approval check inside the loop is a check the caller can
delete.

> **Decision: the hub refuses an unapproved `tool.call`. The harness only renders the card and
> relays the answer.**

Consequences, all of which shape the wire protocol rather than the loop:

1. Policy evaluation happens in `botrosterd`, against rules stored server-side per account (§6's
   "account-scoped, not machine-scoped": the same reasoning, arrived at twice).
2. Approval needs a **request/response pair on the wire**, not a local prompt: the hub sends an
   approval request down the harness connection, the harness answers, the hub then proceeds or
   refuses. That is a new method pair, and it is why this had to be settled before writing code.
3. A tool call awaiting approval is a **third terminal state** alongside ok and error: the
   existing `Progress* Terminal` invariant still holds, but `Terminal` gains a `Denied` variant.
4. `AgentEvent` gains approval variants so every surface renders the same card from the same
   stream.
5. A harness that ignores the approval request simply never gets its result. Nothing is enforced by
   the client's cooperation.

The one thing that stays in the loop is *presentation*: the model should be told when a call was
denied, so it can revise rather than retry blindly. That is the same path a failed tool already
takes (§ the loop feeds errors back), so it costs nothing new.

Grok Build's `PreToolUse` hook is the natural interception point, and `botroster` keeps the hook
**format** wire-compatible (same JSON, same events, same `{"decision":"deny","reason":…}`) so
existing Claude Code hooks work.

**But it inverts one default.** Upstream is **fail-open**: a timeout, crash, or malformed hook
response lets the tool call proceed. That is defensible for a local dev CLI where a human is
watching the terminal. It is the wrong default for an unattended cloud agent holding your Salesforce
session and your inbox.

> **`botroster` cloud policy is fail-closed on the blocking event.** A hook that times out, crashes, or
> returns garbage **denies** the call and surfaces it for review. Per-hook opt-out
> (`fail_open: true`) exists for hooks that are genuinely advisory. **Built.** A non-zero exit
> counts as a failure to answer: a command that does not exist would otherwise exit 1 with an empty
> stdout, and reading that as consent is the fail-open hole in miniature: found by writing the
> test for it.

Rule model, otherwise adopted as-is because it is sound:
- `Require Approval` and `Always Allow`; **deny/require always wins** on conflict
- narrow rules over broad ones: `Bash(git status)` in a named directory, not "allow the browser"
- evaluation order **deny > ask > allow**
- three-state local-execution policy: **Never / Ask every time (default) / Always**
- an org-level *ceiling* where members may go stricter but never looser (documented as "coming
  soon" upstream: ship it in v0)

Two things to fix that upstream gets wrong for this use case:
1. **Sync approval rules to the account, not the desktop.** Upstream stores them per-installation
   and warns you to re-verify on a second machine. That is a footgun; make policy account-scoped.
2. **`allowed-tools` in `SKILL.md` grants and restricts nothing**: it is advisory metadata.
   Either enforce it or rename it. Silently keeping a field that reads like a capability boundary
   but isn't is how people get hurt.

## 7. The credential broker: the security core

The best idea in the whole design, stated plainly in the enterprise docs: *"Sign-in tokens for
hosted MCP servers stay with [the] backend, which runs those tool calls on the computer's behalf.
**The computer never stores those tokens.**"*

`botroster` adopts this as a hard invariant:

```
guest ──"call linear.create_issue"──► hub ──► broker (holds OAuth token) ──► Linear API
                                              ▲
                                    tokens never cross this line
```

- The broker runs in the control plane and is the **only** holder of connector credentials.
  **Built.**
- The guest gets a capability-scoped local endpoint, never a bearer token. **Built differently, and
  more narrowly:** the guest gets no endpoint at all. It asks the hub for a connector tool by name
  and the hub makes the call: so there is no local surface to abuse, and nothing to scope.
- The connection authorises **once at handshake**; per-call credentials do not exist
  (see `PROTOCOL.md` §3: `Principal` with `scopes: ["tool.invoke"]`).
- Secure-secret entry is a masked field that is **excluded from the transcript and never shown to
  the model**: not a general password manager, and documented as such. The second half is built
  and enforced by the type system: `Secret` has no `Serialize`, so it cannot reach a transcript by
  accident. The **masked field is a client concern and does not exist yet**: `botroster secret set`
  reads standard input, and says out loud that a value typed at a terminal is visible.

**The unavoidable caveat, stated loudly in the UI:** browser sessions and shell credentials on the
shared computer *are* accessible to every Bot on that account. Separate Bots are **not** a security
boundary. Upstream says this; `botroster` must say it louder, because users will assume otherwise.

**Where `botroster` can be genuinely better:** upstream rejects `localhost` and RFC1918 MCP URLs
outright, so self-hosted MCP servers need ngrok or a Cloudflare tunnel. A self-hosted control plane
sits inside the user's own network and can reach them directly. That is a real advantage over the
proprietary product, not just parity.

## 8. Routines

```
Routine {
  id, owner_bot_id, name, instructions
  trigger: Schedule { cron, timezone } | Event { source, match_rule }
  approval_boundary, on_missing_data: report | use_stale | skip
  enabled: bool
  runs[]  -- keep the last 20
}
```

- **≤50 per Bot, 20 run records retained**: match the observed limits; they are sensible.
- **Test run performs real work.** Say so in the confirmation dialog, in those words. *(A rule for
  the client at P8: the CLI has no test-run command, so there is no dialog yet. Recorded as a
  requirement rather than left reading like something that exists.)*
- Event triggers use **narrow** match rules. Reject "every new message" style listeners at
  creation time with an explanatory error, rather than letting users build a noise machine.
- **Idle policy:** after a long absence, ask whether to keep routines running and **pause them if
  there is no answer.** An unattended agent that keeps spending money while you are on holiday is a
  bug. **Built:** `botroster routine tick --idle-days` (14 by default). "Somebody is watching" means a
  botroster command run *at a terminal*: a cron tick reading the routine list is not a person looking
  at it, and counting it as one would disable the guard permanently. An account nobody has ever
  looked at is not treated as an absence, or a cron-only deployment would pause itself on its first
  tick. Resuming is explicit, because a person deciding to start it again *is* the answer the
  policy asks for.
- Deleting a Bot deletes its routines. Hiding a Bot does **not** pause them: surface that clearly,
  it is a genuine footgun in the original. **Built:** `botroster bot rm` removes them, and `botroster bot
  hide` lists what will go on running and how to pause it. The warning is silent when the Bot has
  nothing scheduled: a warning that fires when there is nothing to warn about is how a real one
  gets ignored.

## 9. Clients

**Desktop: BOTROSTER** (Tauri: Rust backend, web frontend): the control plane and guest daemon are
Rust, so one toolchain, and Tauri ships far smaller than Electron. "BOTROSTER" is the product name for
the desktop client, chosen by the person paying for the work; the code lives in `crates/botroster-desktop`.

The client must do four things the web cannot:
1. Local command execution under the three-state policy
2. File transfer between local machine and `/workspace`
3. OS notifications
4. (later) WebAuthn/CTAP forwarding from the guest browser to a local security key

**Driving the runtime: ACP (Agent Client Protocol).** This previously read "`xai-acp-lib` exists in
the open repo … ACP is *almost certainly* how the real desktop app drives the agent". That guess is
now checked, and the conclusion survives for a better reason than the one originally given: ACP is
not an inference about somebody's internals at all. It is a **published open protocol with its own
governance**: [agentclientprotocol.com](https://agentclientprotocol.com), JSON-RPC 2.0, an
[official Rust SDK](https://github.com/agentclientprotocol/rust-sdk) (`agent-client-protocol`
2.0.0, Apache-2.0, MSRV 1.88.0: comfortably under our 1.89 floor). Adopting it is not
reverse-engineering a competitor; it is speaking a standard that Zed and other editors already
speak. Use the reference crate rather than hand-rolling the wire types.

The surface, verified against the SDK source rather than prose:

| Direction | Methods |
|---|---|
| Client → Agent | `initialize`, `authenticate`, `session/new`, `session/prompt`, `session/load`, `session/set_mode`, `session/cancel` |
| Agent → Client | `session/update`, `session/request_permission`, `fs/read_text_file`, `fs/write_text_file`, `terminal/*`, `elicitation/*` |

`session/update` carries `plan`, `agent_message_chunk`, `tool_call`, `tool_call_update` and
`usage_update`; `session/prompt` returns a `StopReason` of `end_turn`, `max_tokens`,
`max_turn_requests`, `refusal` or `cancelled`. Send `ProtocolVersion::V1`: v2 exists in the schema
crate but sits behind an `unstable_protocol_v2` feature.

**Botroster is the Agent, and that makes the fs/terminal methods a security decision rather than a
backlog item.** Those are *Client* methods: an editor implementing them is offering the agent **the
user's local disk and shell**, which is the boundary §11.2 and `botroster-guest/tests/isolation.rs`
exist to defend. Botroster does its own file and command work inside the guest, where the policy engine
is.

An earlier draft of this section said BOTROSTER "declines them in its `initialize` capabilities". It
cannot: `fs` and `terminal` are fields of **`ClientCapabilities`**, not the agent's: the client
advertises what it is willing to do, and the schema's own comment says that determines "which file
operations the agent can request". There is no field in which BOTROSTER can refuse. The posture is
therefore stronger and entirely on us: **BOTROSTER never calls `fs/*` or `terminal/*`, whatever the
client offers.** A capability BOTROSTER declines to use is a promise with nothing enforcing it, so the
adapter should be tested for the absence of those call sites the way the guest is tested for the
absence of a path to the credential store.

**Where the approval engine meets the wire.** `session/request_permission` carries
`{ session_id, tool_call, options: Vec<PermissionOption> }`, where each option has a kind of
`allow_once`, `allow_always`, `reject_once` or `reject_always`, and the client answers with
`Selected(option_id)` or `Cancelled`. So ACP's model is *the agent offers the choices and the human
picks one*: which fits §6 exactly, provided the layering is kept straight: **`request_permission`
is how the human is asked; the hub is still what enforces.** A client selecting `allow_always` must
never be able to satisfy a call that a hub `deny` rule forbids, or the fail-closed inversion in §6
is decorative. The options BOTROSTER offers are therefore derived from policy, not the other way round.
V1 has no field for *why* a call needs approval, so our approval reason travels in the reserved
`_meta` object; v2 adds a proper prompt title and should be used when it stabilises.

**The constraint the transport must be built around: handlers run on the event
loop.** The SDK says it plainly: "the connection cannot process new messages
while your handler is running": and for BOTROSTER that is not a performance note,
it is a deadlock. A `session/prompt` handler that awaits the turn inline holds
the loop; the turn asks for approval via `session/request_permission`; the
client answers; and **the answer can never be read, because the thing waiting
for it is the thing blocking the reader.** The agent then sits until the hub's
approval timeout denies the call, and BOTROSTER looks broken in a way that has
nothing to do with botroster.

So `session/prompt` must hand the turn to `ConnectionTo::spawn` and return,
carrying the `Responder` (which takes `self` by value, so it moves) into the
spawned task to answer later. The event drain that today throws `AgentEvent`s
away at `botroster-cli/src/main.rs:2059` is where the `session/update` stream
belongs. Written down because the failure mode is a hang with no error, and a
hang teaches you nothing.

**A suspected integration hazard, which measurement dissolved.** The core crate carries `async-io`,
`async-process` and `blocking`: the smol family: while BOTROSTER is tokio throughout, so this section
first concluded we would need `tokio-util`'s `compat` shim over tokio stdio. Building it says
otherwise: the crate's own example runs under `#[tokio::main]` and hands `Stdio::new()` straight to
`connect_to`, and a probe built that way answered a real `initialize` on this machine , 

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,
 "agentCapabilities":{"loadSession":false,
 "promptCapabilities":{"image":false,"audio":false,"embeddedContext":false}, …}}}
```

,  with no shim and no second runtime of ours. `protocolVersion` goes on the wire as the integer
`1`, and capabilities default to `false`, which is the right direction for something that gates
what an agent may ask of a client.

What remains true is the version trap: the official `agent-client-protocol-tokio` companion depends
on `agent-client-protocol` **0.11.1**, a major generation behind the 2.0.0 core. It looks like the
tokio answer and would pin BOTROSTER to an older ACP. **Do not take it**: the core crate is already
tokio-friendly, which is the thing worth knowing before someone adds the companion to fix a problem
that is not there.

**Computer viewer:** stream the guest's display over WebRTC or a VNC-family protocol, with a
takeover control that transfers input focus from the agent to the human. Takeover must be **modal
and obvious**: the whole 2FA/CAPTCHA/payment flow depends on the user knowing exactly when they
are driving.

## 10. Build order

| Phase | Deliverable | Proves | Status |
|---|---|---|---|
| **P0** | Hub round-trip: harness → hub → guest tool call → result, over real sockets | the protocol works end to end | ✅ **done** |
| **P1** | **Agent loop**: a real harness driving an LLM, executing tool calls through the hub until the task is done | it is an *agent*, not a router | ✅ **done**: both dialects verified against a real HTTP endpoint |
| **P2** | Approval engine (fail-closed) + policy | it is safe to leave alone | ✅ **done** |
| **P3** | Durable volume + update/recover/reset + snapshots | "context compounds": the core promise | ◐ **storage layer done**; hypervisor backend pending |
| **P4** | Browser in the guest, CDP contexts, computer viewer + takeover | it can actually use apps | ✅ **done**: browser, CDP input, `botroster watch`, hub-enforced takeover |
| **P5** | Multiple Bots, per-Bot contexts, messaging, group chats | it is a *team* | ✅ **done** |
| **P6** | Credential broker + MCP connectors | it reaches real systems safely | ✅ **done**: broker, connectors, `botroster secret`/`connector` |
| **P7** | Routines: schedules, then events | it works while you sleep | ✅ **done** |
| **P8** | **ACP agent adapter**: `botroster acp`, speaking the published protocol over stdio | any ACP editor can drive a Bot | ✅ **done**: handshake, sessions bound to Bots, a whole prompt turn streamed to a live client against a real hub |
| **P9** | **BOTROSTER**: desktop client over that same ACP surface | it is a product | ◐ **the window works**: chat streams as the Bot speaks, approvals are answered in a dialog, Stop withdraws pending questions with the turn. **Left:** packaging is unsigned and there is no updater |

Order work by which assumption is riskiest, not by which layer is lowest:

- *"Can we persist a VM across rebuilds?"*: known-solvable ops. Fiddly, not risky.
- *"Does the whole loop produce work a person actually wants?"*: **the real risk**, and it stays
  unfalsified until an LLM is driving the tools.

So the agent loop moves to P1. Approvals follow immediately at P2, because the two are only
testable together: an unattended agent without approvals is unsafe to run, and an approval engine
with no agent has nothing to approve.

**P0-P2 is the MVP**: an agent that does real multi-step work in a confined workspace
and stops for permission before anything consequential. Durable state, the browser, and the
multi-Bot layer are what turn that from a demo into the product.

### The UI deferral, stated rather than buried

Moving the desktop client to P8 puts the interface last, and that deserves to be argued rather than
quietly assumed. The reasoning is that a client is a *view over* a session, an approval queue and a
transcript: none of which exist until P1-P2: so building it first means building against
invented data and rewriting it once real data arrives.

But "last" is wrong too, because an agent product is largely *judged* on its interface, and a
backend developed with no view of itself drifts toward shapes that are awkward to render. So:

> **A single read-only session view lands with P1, not at P8.** One well-made page: live transcript,
> tool calls as they stream, progress, and the approval card. It consumes the same frames the real
> client will, so it doubles as the test of whether the wire format is renderable: and it
> keeps the product visible from the first working agent instead of the eighth milestone.

**Delivered** (`botroster run --html <path>`). What shipped is a *transcript* view: one self-contained file, no network, no
build step, rendered from the same `AgentEvent` stream the terminal consumes. A **live** view
arrived separately in P4 as `botroster watch`, which serves its own loopback HTTP surface rather than
adding one to `botrosterd`: the viewer is a client, and giving the control plane a web server it did
not otherwise need would have been the wrong place to put it. The constraint it
already enforces is the useful one: if something is visible in the terminal and not in the page,
the event is missing, not the surface.

The full BOTROSTER client: with the approval gate open to the person, so the local execution and
file transfer §9 lists can hang off it: is P9. The gate itself shipped first: the client asks and
answers `session/request_permission`, and `botroster acp --demo-tools` plays the tool script so that
surface is exercised without a model.

**Then it was run, which is a different thing from being tested.** Driving the real window through
one turn produced three defects that twelve passing tests and several readings had not: option
kinds arrived JSON-encoded twice so the page could not match on them, the dialog offered four
buttons for three decisions, and *decline* was styled identically to *allow for the rest of this
session*. The last two are only defects if you look at the thing: which is the argument for
building a client at all rather than resting on the adapter. **Takeover did not
wait**: it shipped in P4 with the viewer, because the lock it needs lives in the hub and building
that later would have meant a viewer that could only pretend. What moves early is *seeing the thing
work*, which is cheap and disproportionately valuable.

## 11. Risks, ranked

1. **Durable-volume lifecycle correctness.** Unglamorous; it *is* the product. A `reset` that
   silently eats a week of work destroys trust permanently. Snapshot before every mutation, make
   rollback explicit, never make it the fallback path.
2. **Credential isolation under real conditions.** The invariant is easy to state and easy to
   violate the first time a tool appears to need the token directly. Enforce it structurally: the guest must
   have no code path that can read the credential store.

   *Enforced by:* `crates/botroster-guest/tests/isolation.rs` walks the workspace's own dependency
   graph and fails if the guest can reach `botrosterd` at all. It is aimed at the guest and
   not at the agent because `botrosterd → botroster-bots → botroster-agent` already exists, so an edge back from the
   agent would be a dependency cycle and cargo refuses it: that invariant enforces itself. Nothing
   equivalent protects the guest, because `botrosterd` does not depend on it, so the edge would simply
   compile. "Structurally" was true of one of the two and assumed of both.
3. **Computer-use reliability.** Sites break, block datacenter IPs, and expire sessions. Upstream's
   answer is "pause and ask the human", which is correct. Build the pause path first, not last.
4. **Cost.** One always-on VM per user is expensive. Aggressive idle-suspend with fast resume from
   snapshot is a requirement, not an optimisation.
5. **Prompt injection into an agent with live sessions.** A malicious page can instruct a Bot that
   is logged into your CRM. Approvals are the mitigation, and this is exactly why the fail-closed
   inversion in §6 matters. Say this plainly in the README.

## 12. Licensing and provenance

- Adopted Apache-2.0 code retains `LICENSE` and `NOTICE`, with changes stated per Apache-2.0 §4(b).
- Ship a `PROVENANCE.md` mapping every adopted component to its upstream and licence: including
  the transitive ones: Grok Build's own tools are ports of **openai/codex** (Apache-2.0) and
  **sst/opencode** (MIT), and it bundles **ripgrep**.
- No trademark use. Own name, own branding, and a plain statement of what it derives from.
- Skill/plugin/hook formats are Claude Code compatible: say so, since it means the existing
  ecosystem works on day one, and that is the single biggest adoption lever available.
