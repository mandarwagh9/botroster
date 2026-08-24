# Grok Bot parity and the customization thesis

**Sourcing note.** Every claim about Grok Bot in this document comes from one of four places and
nowhere else: the public documentation at `docs.x.ai/grok-bot/*` and `docs.x.ai/build/*`, the public
marketing page `x.ai/news/introducing-grok-bot`, the public npm registry **metadata** for
`@xai-official/grok` (names, versions, licence fields — never package contents), and the
Apache-2.0 `github.com/xai-org/grok-build` source tree. No `@xai-official/grok-*` `0.1.x` tarball
was fetched, extracted, `strings`-ed or read; those are published as `Proprietary` and the local
`grokbot-recon/dist/` copies were deliberately left closed. This preserves `PROVENANCE.md` §3.

All ten `docs.x.ai/grok-bot/*` pages cited below were **re-fetched live on 2026-08-24** and the
quoted sentences confirmed against the live page, not against a stale local capture. Where a quoted
sentence comes from `docs.x.ai/build/*` it describes **Grok Build, the coding CLI** — a different
product — and is labelled as such; it is never used as evidence about Grok Bot.

The **"Fixed?" column in §2 required an affirmative source**: the documentation had to state that
you cannot change a thing, or state that only an admin can, or enumerate a closed list. "The docs
never mention changing it" is not evidence and produced an `UNKNOWN`, however confident I was.
That rule is why §2 is more than a third unknown while §1 is barely unknown at all: the docs
describe what the product *does* in detail and are almost silent on what it *forbids*.

The matrix is deliberately asymmetric. The Grok Bot column is limited to what xAI chose to publish;
the OPENBOT column is cited to `path:line` in this repository, so it can be checked exactly. Where
`README.md` and `docs/SPEC.md` disagree about whether something exists, **SPEC's status lines win**
— the README's feature table oversells relative to SPEC's inline "pending"/"not done" notes.

**Row counts**, counted rather than estimated, by the verdict in the Grok Bot column:

| | rows | fully cited | part cited, part UNKNOWN | `UNKNOWN — no public source` |
|---|---|---|---|---|
| §1 Feature parity | 46 | 43 | 1 | 2 |
| §2 Customization | 16 | 7 | 3 | 6 |
| **Total** | **62** | **50** | **4** | **8** |

Section 1 is nearly all cited because the documentation describes what the product does in detail.
Section 2 is more than half unknown or partial because it asks what the product *forbids*, and
almost nothing public says. That asymmetry is the finding, not a shortfall in the research: a
competitor who does not document their constraints is a competitor whose users cannot discover them
either.

---

## 1. Feature parity

| Capability | Grok Bot | Source | OPENBOT today | Gap |
|---|---|---|---|---|
| Persistent cloud VM per user | yes | docs.x.ai/grok-bot/teams-and-enterprises — "Each computer is a managed Linux virtual machine dedicated to one member. The Bot runs as a non-root user." | no VM. The guest is an ordinary process running as you | **missing** — `CLAUDE.md` "What does not exist yet"; `docs/SPEC.md` §10 P3 "hypervisor backend pending" |
| One computer shared by all Bots | yes | docs.x.ai/grok-bot/computer-and-apps — "Every Bot on your account uses the same computer" | one guest workspace, reachable by every Bot | none — `README.md` "Bots share the computer" |
| Fixed durable workspace path | yes, `/workspace` | docs.x.ai/grok-bot/computer-and-apps — "The computer has a shared workspace at `/workspace`." | durable volume at `<home>/volumes/<server-id>/current` | none (path differs) — `docs/SPEC.md` §4; `crates/openbot-store` |
| Snapshots and rollback | yes; "Reset Agent Computer returns to the most recent durable snapshot and can discard recent unsaved work." | docs.x.ai/grok-bot/computer-and-apps | content-addressed snapshots; restore takes a safety snapshot first, so a rollback is itself undoable; auto-snapshot every 30 min keeping 48 | none — **better**. `openbot computer snapshot/restore/prune`; `openbot up --snapshot-every/--snapshot-keep`; `docs/SPEC.md` §4 |
| `update` / `recover` / `kill` computer lifecycle | yes | docs.x.ai/grok-bot/computer-and-apps; teams-and-enterprises — "**Kill** deletes the running virtual machine. Durable storage is kept" | none of the three; they need a guest to replace | **missing** — `docs/SPEC.md` §4 lifecycle table, "pending" |
| Browser on the computer, driven by the agent | yes | docs.x.ai/grok-bot/computer-and-apps | real Chromium over CDP; `browser.*` absent from the catalogue when none is installed | none — `crates/openbot-guest/src/browser.rs`; `README.md` Requirements |
| Live computer view + human takeover for 2FA/CAPTCHA/payment | yes | docs.x.ai/grok-bot/computer-and-apps — "Open the computer, take control, complete only the blocked step, and tell the Bot to continue." | `openbot watch`; taking control locks the Bot out and the **hub** enforces the lock | none — `README.md` "The computer" |
| Per-Bot screens, parallel computer use | yes | docs.x.ai/grok-bot/computer-and-apps — "Each Bot gets its own screen on the shared computer."; faq — "One Bot can run one computer-use task on its screen at a time." | one browser, one page; no screen/context argument on any `browser.*` tool | **partial** — `crates/openbot-guest/src/tools.rs` tool list; `docs/SPEC.md` §5 "Screens: resolved" is a design decision, not built |
| Screens are *not* a security boundary | yes, stated | docs.x.ai/grok-bot/computer-and-apps — "The screens are separate work surfaces, not separate security boundaries." | same position, stated louder | none — `README.md` "Bots are not a security boundary" |
| Named persistent Bot with a standing brief | yes | docs.x.ai/grok-bot/bots — "Use the conversation for task-specific instructions. Use the description for rules that should remain true" | identical split; the brief is appended after the base rules so it reads last | none — `crates/openbot-bots/src/lib.rs:114` |
| Cap of 50 Bots + group chats per account | yes | docs.x.ai/grok-bot/bots — "An account can have up to 50 Bots and group chats combined." | same cap | none — `crates/openbot-bots/src/lib.rs:37` |
| Bot duplication (profile/settings/skills/routines/avatar, not history) | yes | docs.x.ai/grok-bot/bots — "It does not copy conversation history, learned memory, or chat attachments." | `openbot bot dup` copies the brief, not the conversation | **partial** — no avatar, no per-Bot enabled-skill subset to copy |
| Hide vs delete | yes; "Hiding does not pause the Bot or its routines." | docs.x.ai/grok-bot/bots | same, and `bot hide` prints what will go on running and how to pause it | none — **better**; `docs/SPEC.md` §8 |
| Bot-to-bot async messaging | yes | docs.x.ai/grok-bot/chat-and-collaboration — "A Bot can send an asynchronous message to another Bot." | `openbot bot send` / `bot inbox`, plus hub-served `bot.send` / `bot.list` tools | none |
| Group chats of 2–6 Bots with `@mention` routing | yes | docs.x.ai/grok-bot/chat-and-collaboration — "In **New chat**, select two to six Bots." | same bounds | none — `crates/openbot-bots/src/lib.rs:1591,1594` |
| Bot-to-group handoffs are text-only | yes | docs.x.ai/grok-bot/chat-and-collaboration — "Bot-to-group handoff messages are currently text-only" | same limitation | none |
| Skills as reusable saved procedures | yes; `/` in the composer, per-Bot enable, packaged skills from a marketplace | docs.x.ai/grok-bot/skills-routines-and-automations; settings-and-notifications — "Use **Marketplace** to discover connectors and packaged skills." | `openbot skill new/ls/show/rm`, `SKILL.md` files on disk | **partial** — no per-Bot enable, no marketplace, no install-from-anywhere; `crates/openbot-cli/src/main.rs:2310` |
| Routines on a schedule | yes | docs.x.ai/grok-bot/skills-routines-and-automations | `openbot routine new --cron` | none |
| Routines on an event trigger | yes | docs.x.ai/grok-bot/skills-routines-and-automations — "Cursor account integrations can start a routine from an event" | `openbot routine on`, `openbot event post` (point a webhook at it) | none |
| 50 routines per Bot, 20 run records kept | yes | docs.x.ai/grok-bot/skills-routines-and-automations — "A Bot can own up to 50 routines, and the app keeps the 20 most recent run records for each routine." | same numbers | none — `crates/openbot-bots/src/lib.rs:2236` |
| A scheduler that fires routines unattended | yes — "Background routines can run while your laptop is closed." | docs.x.ai/grok-bot/skills-routines-and-automations | **the due-check exists; the loop that calls it does not.** Cron expressions are parsed and evaluated — `routine ls` prints when each next fires and `routine tick` runs everything due, once — but nothing calls `tick` on its own. `openbot up` spawns exactly three tasks: the snapshot interval, the hub, and the guest supervisor | **missing** — `crates/openbot-cli/src/up.rs:241,283,320`; `openbot routine tick --help` |
| Routine test run | yes | docs.x.ai/grok-bot/skills-routines-and-automations — "A test run performs real work." | none | **missing** — `docs/SPEC.md` §8, "the CLI has no test-run command" |
| Idle brake on unattended routines | yes | docs.x.ai/grok-bot/skills-routines-and-automations — "Grok Bot may ask whether to keep routines running after a long period away and pause them if there is no response." | `openbot routine tick --idle-days`, 14 by default; a cron tick does not count as a person looking | none — **better**; `crates/openbot-cli/src/main.rs:842` |
| Teach a workflow by demonstration | yes, where enabled; "Teaching records visible computer interaction for up to ten minutes." | docs.x.ai/grok-bot/skills-routines-and-automations | none | **missing**, deliberately — `docs/SPEC.md` §2 out of scope |
| Approval before a consequential action | yes; Allow once / Always allow / Deny | docs.x.ai/grok-bot/approvals-security-and-privacy | allow once, allow for the session, refuse — and the **hub** decides, not the agent | none — **better**; `docs/SPEC.md` §6.0 |
| Rule engine: Require Approval / Always Allow, restrictive wins | yes | docs.x.ai/grok-bot/approvals-security-and-privacy — "If both kinds of rule match, **Require Approval** wins." | `openbot permission add --action allow\|ask\|deny --tool <glob> [--when-key --when-glob]`; deny > ask > allow | none |
| Approval rules that follow the account | **no** | docs.x.ai/grok-bot/approvals-security-and-privacy — "Personal Auto-review rules are stored on the current desktop and synced to its Grok Bot computer. Verify them separately on another desktop installation." | one `[permission]` table in `$OPENBOT_HOME/config.toml`, read by the hub that enforces it; an unparseable rule stops the hub rather than being skipped | none — **better**; `crates/openbot-cli/src/config.rs:22` |
| Programmable pre-tool veto (hooks) | UNKNOWN — no public source | — (Grok **Build**, a different product, has `PreToolUse` hooks: docs.x.ai/build/features/hooks) | `hooks.json` in the home, Claude-Code-compatible contract, evaluated in the hub | n/a — `crates/openbotd/src/hooks.rs:79` |
| Credential broker: MCP tokens never on the computer | yes | docs.x.ai/grok-bot/teams-and-enterprises — "Sign-in tokens for hosted MCP servers stay with Cursor's backend… The computer never stores those tokens." | same invariant, enforced structurally: the guest has no dependency path to the credential store | none — `crates/openbot-guest/tests/isolation.rs` |
| Secure secret entry: masked, out of transcript, not shown to the model | yes | docs.x.ai/grok-bot/approvals-security-and-privacy — "The value is masked, excluded from the transcript, and not shown to the model." | the last two are enforced by the type system (`Secret` has no `Serialize`); the **masked field does not exist** — `openbot secret set` reads stdin and says a typed value is visible | **partial** — `docs/SPEC.md` §7 |
| Connectors / remote MCP with attached credentials | yes; account-wide; per-tool enable/disable | docs.x.ai/grok-bot/computer-and-apps — "Installed connectors are account-wide."; settings-and-notifications — "Connector tools can be enabled or disabled individually." | `openbot connector add/ls/test/rm`; the definition stores the secret's *name*, never its value | **partial** — no marketplace, no per-tool toggle |
| Self-hosted / `localhost` MCP servers | Grok (the assistant) rejects them: "Servers running on `localhost` or a private network address… are not directly reachable, and Grok will reject these URLs." For **Grok Bot specifically: UNKNOWN** | docs.x.ai/grok/connectors/custom-mcp-tunneling | works directly; the control plane runs inside your network, no tunnel needed | none — **advantage**; `README.md` Design |
| Bot acting on your *local* machine under a separate three-state policy | yes; default Ask | docs.x.ai/grok-bot/approvals-security-and-privacy — "The default is **Ask every time**." | no such split exists: the guest **is** your machine | **missing by construction** — `README.md` Warnings |
| Desktop client | yes, macOS + Windows; "Linux desktop, Android, and iPad are not supported at initial launch." | docs.x.ai/grok-bot/faq | Tauri client with Windows/macOS/Linux installers — but **unsigned, with no updater** | **partial** — `README.md` Install; `docs/SPEC.md` §10 P9 |
| Mobile app | yes, iOS 18+ | docs.x.ai/grok-bot/faq | none | **missing** |
| OS / mobile notifications per Bot | yes | docs.x.ai/grok-bot/settings-and-notifications — "Turn on **Notifications** in a Bot's settings to receive an operating-system or mobile notification" | none | **missing** |
| Attachments in the composer | yes; "up to six attachments at a time. Documents, images, and audio can be up to 25 MB each; videos can be up to 200 MB." | docs.x.ai/grok-bot/files-and-results | `openbot attach` copies **one** file into the workspace and the prompt carries the path, not the bytes | **partial** — no multimodal input; `openbot attach --help` |
| Cross-conversation search | rollout-dependent — "Search availability can vary during rollout." | docs.x.ai/grok-bot/chat-and-collaboration | `openbot search` across every Bot and group, no index | none |
| Editor integration over an open protocol | UNKNOWN — no public source | — | `openbot acp` speaks Agent Client Protocol over stdio; any ACP editor drives a Bot with no plugin | n/a — **advantage**; `docs/SPEC.md` §9 |
| SSO / identity | yes, Cursor account and existing SSO | docs.x.ai/grok-bot/get-started; teams-and-enterprises | none. Single-user local home; identity/OIDC is drawn in the architecture and not built | **missing** — `docs/SPEC.md` §3 |
| Org admin console (enable/disable, MCP allow/denylist, team rules, managed setup, kill a member's computer) | yes | docs.x.ai/grok-bot/teams-and-enterprises | none | **missing** |
| Audit view of what Bots did | **not yet** — "An audit view of Bot actions is coming." | docs.x.ai/grok-bot/teams-and-enterprises | `openbot bot log`, `routine show` run history, `openbot run --html` self-contained transcript | none — **better** |
| Static egress IP addresses | yes | docs.x.ai/grok-bot/teams-and-enterprises — "Computers reach the internet through static egress IP addresses." | none | **missing**, deliberately — `docs/SPEC.md` §2 out of scope |
| Hardware security key (WebAuthn) forwarding from the computer | yes on macOS; Windows in progress | docs.x.ai/grok-bot/teams-and-enterprises | none | **missing**, deliberately — `docs/SPEC.md` §2 out of scope |
| Spend cap | **no** — "There is no Grok Bot-specific spend cap yet." | docs.x.ai/grok-bot/teams-and-enterprises | `--token-budget` per run, or `token_budget` in `config.toml`, checked before each turn | none — **better**; `openbot --help` |
| Automatic model failover | yes — "Each request routes to a fixed set of models for its surface, with automatic failover." | docs.x.ai/grok-bot/teams-and-enterprises | one configured endpoint; if it is down, the run fails | **missing** — `crates/openbot-agent/src/providers/http.rs` |

---

## 2. The customization matrix — where OPENBOT wins

This is the product thesis: everything Grok Bot fixes, OPENBOT can let a user change.

| Thing | Fixed on Grok Bot? | Source | Customizable on OPENBOT? | Worth doing? |
|---|---|---|---|---|
| **Which model runs** | **Yes, emphatically** — "Grok Bot has no model picker, for members or admins. We do not plan to allow admin or user choice for models that are used with Grok Bot. Model choice is fully managed by the product." *(Recorded for completeness: the settings page lists "**Default Model**, when model selection is available" — but that page opens by saying "Some options described below may not appear", so it is the same rollout hedge it applies to every setting, not a contradiction. The admin-facing sentence is the affirmative one and it governs.)* | docs.x.ai/grok-bot/teams-and-enterprises; docs.x.ai/grok-bot/settings-and-notifications | **Yes, completely.** `--model`, `[model] id`, `openbot config set --model`, `OPENBOT_MODEL` | Done, and it is the single loudest difference between the two products. Ship it as the headline, not a footnote. |
| **Model provider / endpoint** | **Yes** — "Grok Bot uses a fixed, published set of models and providers with automatic failover. There is no per-team model list." | docs.x.ai/grok-bot/teams-and-enterprises (FAQ, "Can I restrict which models Grok Bot uses?") | **Yes.** `--base-url` + `--dialect anthropic\|openai`; an empty `api_key_env` means a local endpoint that wants no credential, so Ollama on `localhost:11434` works with nothing leaving the machine | Done. This is the row that makes "runs entirely offline" true, which no amount of open-sourcing alone would. `crates/openbot-agent/src/providers/http.rs:24`; `README.md` "A model on this computer" |
| **System prompt** | **UNKNOWN — no public source.** What *is* cited: standing rules go in the Bot description, and admin "Team rules… are always in the Bot's context". So text can be *prepended*; whether the base prompt can be *replaced* is not stated anywhere public | docs.x.ai/grok-bot/bots; docs.x.ai/grok-bot/teams-and-enterprises | **Only partly, and this is a real gap.** The Bot's brief is appended (`crates/openbot-bots/src/lib.rs:114`), but the base prompt is a hard-coded Rust string with no config field — `config.toml` has exactly two tables, `[model]` and `[permission]` | **Yes — highest ratio of value to effort in this document.** One `[agent] system_prompt` field. Today an OSS agent's personality is only changeable by recompiling it, which is the opposite of the reason people pick OSS. `crates/openbot-agent/src/agent.rs:277`; `crates/openbot-cli/src/config.rs:22` |
| **Tool set** | **Partly configurable, cited** — "Connector tools can be enabled or disabled individually" and "Installed private skills can be enabled per Bot". Whether the built-in browser/shell/fs tools can be removed: **UNKNOWN** | docs.x.ai/grok-bot/settings-and-notifications; docs.x.ai/grok-bot/skills-routines-and-automations | **Yes, through a stronger knob.** Any tool id can be denied by policy — `openbot permission add --action deny --tool 'browser.*'` — and a denied tool cannot be re-enabled by the agent or the client. The guest also drops `browser.*` from the catalogue rather than serving it broken | Partial. Add **per-Bot** tool enablement: today the policy is per-home, so "the research Bot may browse, the finance Bot may not" is unexpressible. `crates/openbot-guest/src/tools.rs`; `openbot permission add --help` |
| **Approval policy** | **Configurable but pinned to one desktop** — "Personal Auto-review rules are stored on the current desktop and synced to its Grok Bot computer. Verify them separately on another desktop installation." An org ceiling is "Coming soon we will support a team-level ceiling on local execution" | docs.x.ai/grok-bot/approvals-security-and-privacy; docs.x.ai/grok-bot/teams-and-enterprises | **Yes, in one place that is also the enforcement point.** `[permission] rules` in `$OPENBOT_HOME/config.toml`, evaluated in the hub; a rule that cannot be parsed stops the hub instead of being silently dropped | Done and genuinely ahead. Remaining piece: the org ceiling, which upstream has documented but not shipped — a rare chance to be first. `crates/openbot-cli/src/config.rs:22`; `docs/SPEC.md` §6 |
| **Storage location** | **Yes, fixed** — "Grok Bot requires data storage and does not support Legacy Privacy Mode"; state lives on the managed VM at the fixed path `/workspace` | docs.x.ai/grok-bot/approvals-security-and-privacy; docs.x.ai/grok-bot/computer-and-apps | **Yes, four ways.** `$OPENBOT_HOME` / `--home` for Bots, secrets and connectors; `--workspace` for the computer's files; `--store` / `--volume` for the durable volume; default `~/.openbot` | Done. This is the self-host thesis in one row. `openbot --help`; `openbot up --help`; `openbot computer --help` |
| **Data residency** | **UNKNOWN — no public source** names a region control. The nearest cited sentence assumes you cannot self-serve it: "If your contract limits which subprocessors can handle your data, contact your account team before rolling out Grok Bot." | docs.x.ai/grok-bot/teams-and-enterprises | **Yes, absolutely** — it runs where you run it, and with a local model endpoint nothing leaves the host | Done, and it is the enterprise wedge: the buyer who cannot use Grok Bot at all is the buyer who has a residency clause. `README.md` "A model on this computer" |
| **Sandbox / runtime** | **Yes, fixed** — "Each computer is a managed Linux virtual machine dedicated to one member. The Bot runs as a non-root user." No choice of runtime is offered | docs.x.ai/grok-bot/teams-and-enterprises | **No — and worse, there is no sandbox to customize.** The guest is an ordinary process running as you. `fs.*` refuses paths escaping the workspace root, but that is enforced by this code and not by the operating system, and `--confine-fs-to-workspace-root=false` turns it off | **Yes — the most important item in this document.** Not "make it configurable"; make it *exist*, then make it configurable (container today, microVM later). `CLAUDE.md` "What does not exist yet"; `README.md` Warnings; `docs/SPEC.md` §4 runtime table |
| **Pricing / limits** | **Yes, plan-gated** — "Eligible plans include SuperGrok Plus, SuperGrok Heavy, Cursor Pro+, Cursor Ultra, and Cursor Teams Standard and Premium"; "There is no Grok Bot-specific spend cap yet"; hard caps of 50 Bots+groups, 50 routines/Bot, 20 run records, 6 attachments, 25 MB / 200 MB | docs.x.ai/grok-bot/faq; teams-and-enterprises; bots; skills-routines-and-automations; files-and-results | **No plan and no metering — but the caps were copied and hard-coded.** `MAX_BOTS = 50`, `MAX_ROUTINES = 50`, `MIN_GROUP = 2`, `MAX_GROUP = 6` are Rust constants with no config field | **Yes, and it is cheap.** We inherited a competitor's commercial limits into an unmetered self-hosted product for no reason. A `[limits]` table costs an afternoon and removes an argument we cannot win ("the open one has the same caps"). `crates/openbot-bots/src/lib.rs:37,1591,1594,2236` |
| **Branding** | **UNKNOWN — no public source** on white-labelling or renaming | — | **Source-level only.** The client UI is vanilla `index.html` / `main.js` / `styles.css` with no bundler and no build step, and the product mark is a data URI in `styles.css:566`; the name is hard-coded in the markup | **Yes, low cost.** A `[brand]` table (name, mark, accent) turns "an OSS Grok Bot clone" into "our company's internal agent", which is how this gets deployed inside organisations. `CLAUDE.md` "Where things are"; `crates/openbot-app/ui/styles.css:566` |
| **The client itself** | **Partly fixed, cited** — "the desktop app ships for macOS and Windows"; "Linux desktop, Android, and iPad are not supported at initial launch." Whether the client's source is available: **UNKNOWN** *(npm metadata shows `@xai-official/grok` `0.1.x` published as `Proprietary` and `1.0.x` as `Apache-2.0`, but that package is the coding CLI, not the Grok Bot desktop client)* | docs.x.ai/grok-bot/faq; registry.npmjs.org/@xai-official/grok (metadata only) | **Yes, entirely replaceable.** The runtime speaks ACP over stdio, so any ACP editor — Zed and others already — drives a Bot with no plugin; the shipped Tauri client is Apache-2.0 and uses that same surface, so it is an example rather than a dependency | Done, and it is the second-loudest difference. It also means we never have to win the desktop-app race to be useful. `docs/SPEC.md` §9; `README.md` "Editors and the desktop client" |
| **Extensions / plugins** | **Governed, not fixed** — a Marketplace exists; team policy governs it and "There are no separate Grok Bot plugin controls"; the platform-level custom-MCP path rejects private addresses (cited for Grok, **UNKNOWN for Grok Bot**) | docs.x.ai/grok-bot/settings-and-notifications; teams-and-enterprises; docs.x.ai/grok/connectors/custom-mcp-tunneling | **Partly.** Remote MCP connectors (private-range URLs work with no tunnel), `SKILL.md` skills on disk, and `hooks.json` in the Claude-Code-compatible format so existing hooks run unchanged. No marketplace, no packaged-skill install, no stdio/local MCP servers | **Yes, narrowly.** "Install a skill from a git URL" is the cheapest ecosystem move available and makes the existing Claude Code skill ecosystem reachable on day one. A marketplace is not worth building (see §4). `crates/openbotd/src/hooks.rs:1`; `openbot connector add --help` |
| **Telemetry** | **UNKNOWN** whether the app emits telemetry. What is cited is adjacent and still damning for a privacy-sensitive buyer: "Grok Bot requires data storage and does not support Legacy Privacy Mode", and "Data training follows your team's privacy settings" | docs.x.ai/grok-bot/approvals-security-and-privacy; teams-and-enterprises | **None to configure, because none exists.** A grep of the workspace for telemetry / analytics / Sentry / PostHog / phone-home finds exactly one hit, an unrelated protocol doc comment | **No new code — but write it down and test it.** "No telemetry" is a feature only when it is stated in the README and guarded by a test that fails if an outbound analytics call ever appears. `crates/openbot-proto/src/lib.rs:268` is the only match |
| Where the policy gate runs | UNKNOWN — no public source | — | In the hub, not the agent; the thing being gated cannot remove the gate, and a client answering `allow_always` still cannot pass a hub `deny` | n/a — this is architecture, not a setting, and it is the reason every other approval row above holds. `docs/SPEC.md` §6.0 |
| Hook-failure default | UNKNOWN for Grok Bot. Grok **Build** (the coding CLI, different product) is fail-open: "Everything else — timeouts, crashes, malformed output — is fail-open: the failure is recorded in the session but the tool call proceeds." | docs.x.ai/build/features/hooks | **Fail-closed, with per-hook `fail_open` opt-in.** A hook that times out, crashes, exits non-zero or answers garbage **denies** the call | Done. Correct for an unattended agent holding live sessions; keep the opt-in for advisory hooks. `crates/openbotd/src/hooks.rs:59` |
| Bot / group / routine caps | Yes, fixed at 50 / 2–6 / 50 / 20 | docs.x.ai/grok-bot/bots; skills-routines-and-automations | Hard-coded to the same numbers | Folded into the pricing/limits row above — same fix. |

---

## 3. Ranked gap list

Ordered by **(how many users care) × (how hard it is to live without)**, not by how interesting the
work is. The first two are ordered above everything else because they are the two places where a
person tries OPENBOT, hits the wall, and stops.

1. **A real isolation boundary for the guest.**
   *Blocked today:* anyone who would run this on a machine that matters. The Bot is a process
   running as you; `shell.exec` sits behind one approval and answering "allow for the session" hands
   it your shell for the rest of that session. The README's own advice is "run it on something you
   would hand to a contractor" — which is an honest sentence and also a description of a product
   most people cannot install.

2. **A supervisor loop, so routines fire without you.**
   *Blocked today:* everyone who created a routine and did not also write a system cron entry. The
   scheduling itself works — the cron expression is parsed, `routine ls` prints when each next
   fires, and `routine tick` runs everything due — but nothing calls `tick`, so the user has to
   supply the loop themselves. `openbot up` already runs an interval task for snapshots, so this is
   a second timer beside an existing one, not new machinery. It ranks this high because it is the
   *only* missing piece between "an agent you talk to" and Grok Bot's headline claim — "Background
   routines can run while your laptop is closed" — and because the desktop client is the primary
   install path, where telling a user to add a crontab entry is not an answer at all.

3. **Model failover, or at least a fallback endpoint.**
   *Blocked today:* anyone running unattended work. One 503 from one provider ends the run, and the
   whole point of an overnight routine is that nobody is watching. Grok Bot routes with automatic
   failover; we route to exactly one URL.

4. **An editable system prompt, and a per-Bot model.**
   *Blocked today:* the user who chose an open-source agent specifically to change how it behaves,
   and discovered the persona is a string literal in `agent.rs` and the model is global. This is the
   customization thesis failing on its own home ground; it is also the cheapest row in §2.

5. **Routine test run.**
   *Blocked today:* anyone deciding whether to trust a routine with real access. Upstream ships a
   test run and warns in the dialog that it performs real work. We ship neither, so the first real
   execution of an automation is also its first execution.

6. **Signing and an updater for the desktop client.**
   *Blocked today:* every non-technical installer, at the SmartScreen warning, before they see the
   product at all. The current answer — "right-click → Open" — is a conversion funnel with a
   security warning in the middle of it.

7. **Per-Bot browser screens.**
   *Blocked today:* anyone running two Bots at once, which the product's own roster UI invites.
   One browser means the second Bot waits, and the parallelism the sidebar implies is not real.

8. **Per-Bot tool and skill enablement.**
   *Blocked today:* anyone building a roster with different trust levels. Policy is per-home, so
   "the finance Bot may not browse" cannot be said.

9. **Identity and a multi-user control plane.**
   *Blocked today:* teams. Also the precondition for the org ceiling that upstream has documented
   and not shipped — the one place we could be first rather than second.

10. **OS notifications.**
    *Blocked today:* anyone who alt-tabs away. A teammate you have to poll is not a teammate.

Mobile is deliberately not on this list. See §4.

---

## 4. What we should NOT build

1. **Teach-by-demonstration.** This is Grok Bot's most quotable feature and we should skip it
   anyway. The docs describe a ten-minute browser recording that produces a *draft* skill which the
   user must then review, add decision rules to, add failure handling to, and test on a safe example
   — that is most of the work of writing the skill, after building a recorder, a DOM-event
   serialiser, and a generaliser that turns one concrete path into a robust procedure. It is
   quarters of work whose output the docs themselves describe as a first draft, and its whole
   audience is users who will not write a skill by hand — who are also the users least equipped to
   review the draft it produces. `openbot skill new` plus "save what we just did as a skill" reaches
   90% of the value for none of the cost.

2. **A mobile app.** An iOS client means a hosted, authenticated, internet-reachable control plane —
   identity, push infrastructure, App Store review, a second UI to keep in step forever. It is the
   most expensive thing on the board and it is worth nothing until §3's items 1, 2 and 9 exist. A
   self-hosted product's mobile story is a URL, later.

3. **A plugin marketplace.** Upstream's marketplace exists because it is a closed platform that must
   curate. We are Apache-2.0 with a file-on-disk skill format and remote MCP; the "marketplace" is
   git. Build `openbot skill install <git-url>` — a day of work — and let GitHub be the registry.
   Building a catalogue, a submission flow and a trust model for an ecosystem that does not exist
   yet is inventing demand.

4. **Static egress IP pools.** Real value to a genuinely small group: enterprises that allowlist by
   source IP. It requires operating network infrastructure, which a self-hosted product by
   definition does not do — and a self-hoster already controls their own egress, so for our actual
   users the feature is *already true* and needs no code.

5. **WebAuthn / CTAP forwarding.** Upstream is still shipping this (Windows support in progress) and
   it exists to solve a problem we do not have: their computer is a remote Linux VM far from your
   security key. When the guest runs on your own machine, the key is already there.

6. **An org audit console and admin dashboard.** Upstream's own audit view is "coming"; ours would
   serve zero users until identity exists, and by then `openbot bot log` plus `run --html`
   transcripts already answer "what did it do" better than most dashboards do.

---

## 5. The honest competitive position

OPENBOT genuinely wins on the things a locked platform structurally cannot concede: any model at any
endpoint including one on localhost, state that lives wherever you point `$OPENBOT_HOME`, an
approval policy that lives in one place and is enforced by the hub rather than by the agent asking
itself for permission, a fail-closed hook contract where upstream's is fail-open, private-network
MCP servers that need no tunnel, a rollback that takes a safety snapshot first so it cannot eat a
week of work, and a runtime that speaks a published protocol so the client is replaceable — none of
which xAI can match without abandoning a managed product, and the model row is the one they have
explicitly said they will not move on. Where we are not close is everything that makes a cloud
product a product: there is no VM, no scheduler firing the routines we let people create, no
identity, no signed installer, no mobile, no failover. The single feature whose absence loses the
most users is the isolation boundary — not because users audit sandboxes, but because "the Bot is a
process running as you with a shell" is the sentence that ends the evaluation for every buyer whose
machine has anything on it, and it is also the sentence our own `CLAUDE.md` forbids us to soften.
The closest second is the missing supervisor loop: the routine machinery is all there and correct —
the cron parses, the due-check works, `tick` runs what is due — and nothing calls it, so a user who
sets a 9am routine and closes the laptop gets nothing unless they wired up system cron themselves.
That is a smaller defect than it first looks and a larger product problem than it sounds, because
it is the last inch of the one behaviour this entire category is sold on. Read plainly, this is a
credible control plane with a missing floor and a missing clock: the architecture decisions are
better than the competitor's in several places that matter and were arrived at deliberately, but
the product is currently a very well-designed thing you should not leave running. Nothing in §1
suggests we are behind on judgment; §3's top two say we are behind on the two pieces of plumbing
that let anyone act on that judgment.
