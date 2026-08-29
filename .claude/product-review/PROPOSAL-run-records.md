# Proposal: run records — a Bot's own history, as its test suite

**Status:** proposed 2026-08-29. Original work; no upstream, so no `PROVENANCE.md` row is owed
beyond §4. Written against the product as of `14a96db` (0.4.1).

`CHARTER.md` §0 asks "does this matter to most people who would use BOTROSTER?" — the right filter
for a defect. This document answers the other question, asked directly by the operator: **what
should BOTROSTER have that Grok Bot and every open-source alternative does not, and could not
easily copy?**

`reports/parity.md` §6 warns that a filled-in feature matrix is not a strategy. This proposal is
deliberately not a matrix row. It is an axis nobody in the category is competing on.

**Sourcing.** Every claim about a competitor here comes from that project's own public README,
documentation or published paper, listed in §9, and was read on 2026-08-29. The `PROVENANCE.md` §3
clean-room boundary is unchanged: nothing here was learned from a proprietary artefact, and the two
Grok Bot sentences quoted are from `docs.x.ai/grok-bot/*`, which `reports/parity.md` already cites.
Claims about this repository are cited to `path:line` so they can be checked exactly.

---

## 0. What it looks like, in one screen

The thing to demo, written first so the rest of this document can be judged against whether it
delivers it.

```sh
$ botroster bot test scout

  replaying 4 golden runs against the current brief. nothing is written, opened or sent.

  ✓ 2026-08-14 screen the Rust applications        31 steps, identical
  ✓ 2026-08-19 screen the Rust applications        27 steps, identical
  ✗ 2026-08-21 chase the three shortlisted         diverged at step 6
        was   browser.open   https://github.com/…/pulls
        now   shell.exec     gh pr list --author …
  ✓ 2026-08-24 weekly summary                      12 steps, identical

  1 of 4 diverged. `botroster replay 2026-08-21 --from 6` to watch it happen.
```

You changed one sentence in a Bot's brief. Thirty seconds later you know what it changed about the
Bot, on real history, without the Bot touching anything. That is the whole pitch, and today the
honest answer to "did I break it?" is *run it and see*.

## 1. The thesis

Every product in this category sells the same safety story: **prevention.** Grok Bot's is a managed
Linux VM per member. Rakazo's and OpenMausBot's is Docker on your own machine. Ours is a hub-side
policy gate and an approval dialog. All three are prevention, and prevention has one failure mode
that is now well measured: people click through it. Camunda's 2026 report puts 71% of enterprises
running agents somewhere and **11% in live production**, and names the blocker as trust —
specifically that the people responsible "can't say how it will behave, what it's doing right now,
or why it did what it did."

That is not a sandbox problem. No sandbox answers any of those three questions.

The unclaimed axis is **accountability after the fact**: what did it actually do, would it do it
again, and what of it can be taken back. It is unclaimed because it is expensive for everyone else
and nearly free for us — for two reasons that are already written into `CLAUDE.md` as invariants we
refuse to weaken:

- **Every tool call passes through the hub** (`crates/botrosterd/src/hub.rs:1046`). One place to
  record. It is also the place that evaluates policy, so the record includes *the approval
  decisions* — not just what the agent did, but what a person let it do.
- **The workspace is content-addressed** (`crates/botroster-store`). History is a manifest of
  hashes; identical content is stored once. Keeping many points in time is cheap by construction.

We built both for other reasons. This proposal is the interest on them.

## 2. Why nobody else can simply copy it

| | Why not |
|---|---|
| **Rakazo**, **OpenMausBot** | Both delegate the agent loop to an external CLI — Claude Code, Codex, Grok. Those harnesses own their own tool loop, so the tool calls never pass through the host product. It can record a subprocess's output; it cannot record the calls, because it never sees them. Recording them means rewriting the thing they were built to wrap. |
| **Grok Bot** | Architecturally could. Structurally will not: it is a managed product, and a run transcript you can hold, diff and replay locally is the opposite of a managed product. Their one rollback primitive is "Reset Agent Computer returns to the most recent durable snapshot" — whole-machine, one level, and their own docs say it "can discard recent unsaved work." |
| **LangGraph time travel**, **Adaline replay**, **Causal Agent Replay** (arXiv 2606.08275) | The concept is not new and we should not pretend it is. All of it replays **the transcript** — graph state, message lists, prompts. None of it replays **the world**, because none of those systems own a filesystem. Prior art is in the ops/observability layer; nobody has put it inside a persistent-teammate product, where it stops being a debugging tool and becomes the thing that lets you change your Bot without fear. |
| **OpenHands**, **Goose** | Have trajectories and logs. No workspace state bound to them, so a trajectory can be read and not re-entered. |

The honest summary: **replay is prior art, replaying the world is not, and shipping it as a product
surface in this category is not.**

## 3. The feature

> Every run is recorded: the model exchanges, every tool call and its result, every approval
> decision, and what changed on the computer at each step. A recorded run can be **replayed with
> the world disconnected** — no file written, no page opened, no outbound call — so replaying is
> safe on the machine you actually work on. And a Bot's recorded runs are **its test suite**:
> change its brief, replay its history, and see exactly where it now behaves differently.

Three surfaces, in the order they should be built.

### 3.1 The record

`<home>/bots/<id>/runs/<run-id>.jsonl`, append-only, beside the `conversation.jsonl` that already
works this way. One line per event, written by the hub as it already handles the event:

- `model` — request and response, with the dialect and the model id
- `tool` — call, arguments, result, `elapsed_ms` (the runtime already measures it), and the policy
  verdict with who answered it
- `step` — the checkpoint id of the workspace after the step (§3.4)

The hub is the only writer. The agent is not asked to report on itself, for the same reason the
policy gate is not in the agent: a record the recorded thing can edit is not a record.

### 3.2 `botroster replay <run>` — with the world disconnected

Replay re-runs the agent loop live and answers every `tool.call` from the recording instead of
forwarding it to the guest. The stub sits at the hub's forwarding point, which means it is enforced
by the same code path that enforces policy — not by asking the agent to behave.

**This is the property that makes the feature usable rather than clever.** Replay touches nothing:
no file is written, no browser opens, no connector fires, no work is done on the guest side. You
can replay a routine that deletes things, on your own laptop, at any time, with real work on disk.

A call the recording does not contain — because the agent diverged — is a **divergence**, not an
error. Replay stops there and says what was expected and what was asked for. That is the output.

### 3.3 `botroster bot test <bot>` — the diff

Mark recorded runs as golden. Then:

```sh
botroster bot test scout          # replay every golden run, report divergences
```

Change the brief, change the model, change `context_budget`, and find out what you broke in
seconds, without touching the world.

This is the surface that pays for the other two, and it closes three open backlog items rather than
adding a fourth. **T5-2** (the persona is a string literal) and **T5-3** (`context_budget` is
unreachable) are both blocked on the same unstated thing: the reason it is frightening to expose
those knobs is that nobody can tell whether turning one broke the Bot. `reports/parity.md` calls
customization "the gap that most directly contradicts the pitch" — someone chose an open product
precisely to change this. Shipping the knobs without a way to check them ships the contradiction.

*Unit tests for your teammate* is the line. It is true, and no competitor can say it.

### 3.4 Per-step checkpoints, and what can be taken back

Each step gets a cheap checkpoint of the workspace, and the run log gains two things per row: what
changed on disk, and **whether it can be taken back**.

The hub knows the tool name, so the classification is exact and needs no guessing:

| Tool | Badge | Why |
|---|---|---|
| `fs.read`, `fs.list`, `browser.read`, `browser.links`, `browser.snapshot`, `browser.scroll`, `browser.frame` | *nothing to take back* | it read something |
| `fs.write`, `fs.delete_everything`, `browser.screenshot` | **can be undone** | writes land in the workspace, and the workspace is snapshotted |
| `shell.exec` | **partly** | the workspace rolls back. `botroster-guest/src/lib.rs:8` is explicit that `shell.exec` is *not* bounded by the workspace: the command may `cd` anywhere the user can reach. Whatever it did out there stays done |
| `browser.open`, `browser.click`, `browser.click_at`, `browser.fill`, `browser.type`, `browser.key` | **cannot be undone** | it acted on a live site. A submitted form stays submitted, and `Volume::browser_profile` is deliberately outside the snapshot, so a rewind never restores or discards a login |
| connector calls through the broker | **cannot be undone** | an authenticated outbound call to somebody else's system |

**No competitor tells you which of its actions it can take back.** That table is a feature.

## 4. What this is not, stated before anyone writes the copy

`CLAUDE.md`: *"Do not write documentation, comments or UI copy that implies isolation the project
does not have."* The same rule binds here, and the first draft of this proposal broke it.

- **It is not undo.** "The only agent that can undo itself" is false and we will not say it. A
  rewind restores the workspace. It does not un-send an email, un-submit a form, un-push a commit,
  or reach outside the workspace after `shell.exec` went there.
- **It is not a substitute for the isolation boundary.** `reports/parity.md` §5 says the missing
  boundary is the sentence that ends the evaluation, and it still is. Reversibility reduces the
  cost of a mistake; it does not prevent one. Both sentences go in the README together or neither
  does.
- **Replay is not proof the Bot is deterministic.** Models are not, at any temperature. Replay
  proves that *this* run reproduced, or names the step where it did not. That is the honest and
  more useful claim: the divergence is the finding.
- **A checkpoint is not a backup.** It is inside the same home. If the disk goes, so does it.

## 5. Candidates rejected, and why

Recorded so the next person does not re-derive them.

1. **A verifier model auditing each step.** Anyone can bolt a second model on in an afternoon; it
   doubles cost and defends nothing. Not structural.
2. **Ship the isolation boundary** (Landlock / seccomp / AppContainer). Genuinely valuable and
   already the top of `parity.md` §3 — but it is parity-chasing. It makes us *equal* to a managed
   VM, three platforms at a time. It belongs on the roadmap; it is not the thing that puts us ahead.
3. **Teach-by-demonstration.** Already rejected with reasons in `parity.md` §4.1. Unchanged.
4. **Branch a run** — fork the world and the transcript at step N, change one thing, run forward,
   diff the branches. This is the most exciting item here and it is deliberately **not** in this
   proposal's scope. It needs a new volume seeded from a manifest, which is well past `CHARTER.md`
   §5's ~200-line rule. It goes to `BACKLOG.md` as `NEEDS REVIEW` with this plan attached, which is
   what that rule is for. It is also strictly easier once §3.1–3.4 exist.

## 6. Build order

Ordered so each step ships something usable and none of it is wasted if the next is dropped.

| # | Work | Why here |
|---|---|---|
| 0 | **Finish T2-1: the hub requires the per-home token.** | It already gates everything. A hub that authenticates nobody, plus a feature that lets a caller replay or rewind your computer, is strictly worse than either alone. This is not competing with the work; it is a precondition for it. |
| 1 | **The record** (§3.1) | Nothing else exists without it. Also immediately useful on its own: the approval decisions become part of what a run can show. |
| 2 | **Replay** (§3.2) | The stub at `hub.rs:1046`. No snapshot machinery at all. |
| 3 | **`bot test`** (§3.3) | The product surface. Unblocks T5-2 and T5-3, which then ship behind it. |
| 4 | **Checkpoints + badges** (§3.4) | Needs the fast path in §7. Highest visual payoff, highest cost. |
| 5 | Branch → `BACKLOG.md`, `NEEDS REVIEW` | Per `CHARTER.md` §5. |

Steps 1–3 need no change to `botroster-store` at all. That is the point of the ordering.

## 7. Risks, and what has already been checked

- **Snapshot cost is O(whole workspace), not O(changed).** Checked, and it is real:
  `Volume::ingest` copies every file to a temp blob and hashes the copy, on every snapshot. The
  copy-then-hash order is deliberate and correct — the doc comment explains the TOCTOU bug that
  hashing-then-copying causes — so the fast path must not touch it. A per-step checkpoint needs a
  *separate* function that skips unchanged files by stat, using git's racily-clean rule: any file
  whose mtime is at or after the checkpoint's start time is re-hashed regardless. The existing
  durable path, and the two tests that guard it —
  `blobs_always_match_their_own_hash_under_concurrent_writes` and
  `a_snapshot_is_immutable_once_taken` — stay exactly as they are.
- **`restore` refuses while a guest is attached.** Checked: `StoreError::Attached`, with a good
  cross-platform reason recorded at the refusal. So "rewind" is stop → restore → restart, not a
  button that acts mid-run. The UI must say so rather than appear to fail.
- **`snapshot` holds a mutation lock with a 30s wait.** A per-step checkpoint on a large workspace
  can return `Busy` mid-run. A missed checkpoint must degrade to a step with no rewind offered —
  never to a failed run.
- **The record contains what the model was sent.** That includes tool results, which can contain
  workspace file contents. It must never contain a credential: the broker attaches tokens inside
  the hub at the moment of the call, and the scrubber that already cleans upstream errors has to
  cover this writer too. This is the one part of the design that can leak something, and it gets a
  test that fails if a known secret reaches the file.
- **Nobody wants this.** The real risk. Mitigation is the ordering: step 3 is the one to demo, and
  if it does not land, steps 4 and 5 are not built.

## 8. How we will know it worked

- `botroster bot test` catches a real regression that the author did not predict — the first time
  that happens on an unplanned change, the feature has paid for itself.
- T5-2 and T5-3 ship, with the knobs exposed and no fear attached.
- A README row that no competitor's README can carry, and that survives `scripts/review.sh`'s
  honesty gates unedited.

## 9. Sources

Read 2026-08-29 unless noted.

| Claim | Source |
|---|---|
| 71% of enterprises running agents somewhere, 11% in live production, 85% citing process maturity; the blocker stated as trust — "can't say how it will behave, what it's doing right now, or why it did what it did" | Camunda, *2026 State of Agentic Orchestration and Automation*, via [their write-up](https://camunda.com/blog/2026/07/ai-agents-dont-have-a-capability-problem-they-have-a-trust-problem/). Both figures confirmed against the live page, not a search summary |
| Rakazo: self-hosted, sandboxed browser and shell per bot, "you own everything" | [github.com/elie222/rakazo](https://github.com/elie222/rakazo) |
| OpenMausBot: wraps Claude, Codex or Grok as persistent bots, Apache-2.0, desktop | [github.com/milind-soni/OpenMausBot](https://github.com/milind-soni/OpenMausBot) |
| Grok Bot: managed Linux VM per member; "Reset Agent Computer returns to the most recent durable snapshot and can discard recent unsaved work" | `docs.x.ai/grok-bot/computer-and-apps`, `docs.x.ai/grok-bot/teams-and-enterprises`, already cited in `reports/parity.md` §1 |
| Checkpoint-based state replay and forking from a checkpoint — the prior art we are **not** claiming to have invented | LangGraph time travel; [Adaline, *Agent Replay Is A Product Surface*](https://labs.adaline.ai/p/agent-replay-product-surface) (2026-07) |
| Counterfactual re-execution of an agent trajectory as a research method | *Causal Agent Replay: Counterfactual Attribution for LLM-Agent Failures*, [arXiv:2606.08275](https://arxiv.org/abs/2606.08275) (2026-06) |
| Goose: Rust, Apache-2.0, 25+ providers, on-machine; OpenHands: self-hosted coding-agent control centre | the projects' own documentation |

The distinction the whole proposal rests on — **prior art replays the transcript; none of it replays
the world** — is a claim about what those systems own, not about what they intend. LangGraph
checkpoints graph state, Adaline forks a prompt or a tool response, and neither holds a filesystem.
If one of them ships a content-addressed workspace, this advantage is gone and the honest response
is to say so here rather than to keep the row.
