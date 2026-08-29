# BOTROSTER product review

The UX loop in `.claude/ux-loop/` reviewed one axis — how the desktop client looks and behaves —
and it worked. This is the same instrument aimed at the whole product: the runtime, the agent, the
tools, the CLI, the client, and the question of whether any of it adds up to something a person
would choose over Grok Bot.

`scripts/review.sh` is the gate. This file is the procedure. Neither is a substitute for the other:
a gate with no procedure gets run once, and a procedure with no gate is a document.

---

## 0. The rule that overrides the others

> **Does this matter to most people who would use BOTROSTER?**

Every finding carries a `reach:` field, and a finding that cannot honestly claim `most users` or
better needs a P0 severity to survive. This is not a nicety. The failure mode of a thorough review
is a hundred true, tiny findings that consume the entire budget while the product stays
uncompetitive, and a reviewer who has just read ten thousand lines is at their least able to tell
the difference. The field forces the question at the moment it is cheapest to answer.

It cuts both ways. "The empty conversation pane wastes 60% of the window" reaches everyone who
opens the app and is worth a day. "The 8.3 short-name form of a Windows path could defeat a
containment check" reaches almost nobody and costs a week — unless it is a security hole, which is
what P0 exists for.

## 1. What is being reviewed, by whom

Six departments, each with a bounded scope so two reviewers never argue about the same file. LOC is
Rust source, and the split is deliberately weighted: the two largest crates get a department each.

| Department | Scope | LOC | Report |
|---|---|---|---|
| Runtime & Security | `botrosterd`, `botroster-store`, `botroster-proto` | ~11.4k | `reports/runtime-security.md` |
| Agent & Model | `botroster-agent`, `botroster-bots` | ~10.7k | `reports/agent-model.md` |
| Guest & Tools | `botroster-guest` | ~3.8k | `reports/guest-tools.md` |
| CLI & DevEx | `botroster-cli`, `README.md`, `CONTRIBUTING.md` | ~14.2k | `reports/cli-devex.md` |
| Design & Client | `botroster-app`, `botroster-desktop`, the UI | ~14.9k | `reports/design-client.md` |
| Product Management | Grok Bot parity, the customization thesis | — | `reports/parity.md` |

A reviewer is a fresh agent with no memory of having written any of it, which is the property that
made the UX loop's critic useful. It reads the code and the bar; it is not told who wrote what.

**Reviewers are read-only.** They write their report and touch nothing else. Review and
implementation stay separable, because a reviewer who can also patch will patch instead of
reporting, and the finding — the part another person can act on — is lost.

## 2. The finding format

```markdown
### F-XX1 — <the defect, stated as what is wrong>
`P0|P1|P2` · `reach: all users | most users | some users | few` · `crates/x/src/y.rs:123`

**What is true now.**   the actual current behaviour, with the code
**Why it matters.**     the user-visible consequence — no consequence means no finding
**The durable fix.**    the structural change, not the patch
**How to prove it.**    the specific test that fails today and passes after
```

The last two lines are the ones that do the work.

**"The durable fix"** exists because the brief was explicit: *"We don't want monkey work, monkey
patches or solutions which are just for today."* A finding that proposes a special case has not
finished thinking. The browser-override bug found on the first run is the shape to copy — the patch
was to add an error variant, and the durable fix was to split the resolver so the override is passed
in rather than read, which is what made it testable at all.

**"How to prove it"** exists because CONTRIBUTING.md says a claim gets a test, and a finding is a
claim. It also converts the report from opinion into work: the test is written first, it fails, and
the finding is closed when it passes.

Severity: **P0** breaks a task or is a security hole. **P1** makes a task slow, ambiguous, or
unrecoverable. **P2** is polish.

## 3. Definition of done

The bar is `CONTRIBUTING.md`, unchanged. This system does not get its own:

- A claim gets a test that would fail if the claim were false.
- After a test passes, break the code and confirm the test notices.
- Anything with a face is checked in the shipped binary, not only in a library test.
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Nothing enters the repository without a row in `PROVENANCE.md`.
- Neither structural invariant weakens: the guest cannot reach `botrosterd`, and the policy gate stays
  in the hub.

Plus, for this loop: `sh scripts/review.sh` passes. `--full` before anything is pushed.

## 4. Relationship to `.claude/ux-loop/`

**That loop is not superseded and its files are not duplicated.** It holds the pinned design system
(`DIRECTION.md`), the approval invariants, the design rubric, and nineteen open backlog items
B01–B19. The Design & Client department reads all of it and is forbidden from re-reporting any of
those nineteen; its job is what a screenshot-driven loop could not see.

Two backlogs would mean two definitions of done and a divergence by the fifth iteration. So:
frontend design work continues to be governed by `DIRECTION.md` and gated by `scripts/ux-verify.sh`,
which `scripts/review.sh --full` calls rather than reimplements.

### Carried forward, still open

Three decisions were recorded before this loop started and are still the operator's to make:

1. **The app icon's origin is unconfirmed.** `PROVENANCE.md` §4 records it honestly rather than
   claiming a licence. It ships in every release. Everything derives from one file, so swapping it
   is a one-command change.
2. **The page suite flakes.** The recorded fix — retry once, and only on
   `chrome-error://chromewebdata/`, which is a navigation that never completed and is
   distinguishable from an assertion failure — **was implemented** and is in `page.rs:227-241`. A
   blanket retry would hide real failures, and that reasoning still holds.

   It is no longer enough. Third occurrence 2026-08-29, on
   `an_hourly_routine_does_not_ask_what_time_it_runs`, in a full `--workspace` run: the retry fired
   (`navigation retried: true`) and the second navigation landed on the error page too. The suite
   then passed 101/101 on its own and 1/1 for that test alone, as both earlier occurrences did.

   What the new data point says: the cause is **contention**, not a one-off. All three failures are
   in the full workspace run, where the page suite shares a machine with the live hub, guest and
   browser suites; none has ever reproduced alone. So the next move is not a wider retry — it is to
   find out what the page suite is contending with. `BOTROSTER_REQUIRE_BROWSER=1` in CI means a
   missing browser fails rather than skips, so this cannot be hidden by a skip either.

   Still the operator's call, and still not worth spending an iteration on while it costs one
   re-run.
3. **`DIRECTION.md`'s neutral-accent derivation was never applied.** The shipped `--accent` is
   `#6f45e0` violet, and the derivation said colour in this app means status and never emphasis.
   One of the two is wrong and nobody has decided which.

## 5. The loop

Branch `product/review-system`. Never commit to `main`.

**Each iteration:**

1. **Read `MEMORY.md` first.** It is the only thing that makes this compound rather than
   random-walk — the same sentence appears in `LOOP.md` and it was true there.
2. **Pick one item** from `BACKLOG.md`: highest severity, then highest reach. One item, one surface.
3. **Write the failing test first.** It is the finding's "How to prove it" line. If it passes
   already, the finding was wrong — record that, delete it, pick the next.
4. **Implement the durable fix.** If it needs more than ~200 lines of structural change, write the
   plan into `BACKLOG.md` as `NEEDS REVIEW` and pick something else.
5. **Verify:** `sh scripts/review.sh`, then the touched crate's tests, then break the fix and confirm
   the new test notices. A gate failure reverts the change; it does not get argued with.
6. **Commit**, naming the finding id. **Append to `MEMORY.md`**: what was tried, whether it held, and
   the generalisable lesson.

### Stop conditions

Written down now, because "until I am satisfied" has no edge and does not converge:

- No open P0 or P1 with reach `most users` or better, **or**
- three consecutive iterations with no finding closed, **or**
- five consecutive gate failures, which means something structural is wrong and the right move is to
  stop and write it up.

### On stop

`REPORT.md`: every finding by department with its disposition, every commit and what it closed,
the gate at first run versus last, everything marked `NEEDS REVIEW`, and the three things worth
doing next. Push the branch. Do not merge without a human reading `REPORT.md` first and running the
app for ten minutes.

## 6. What this loop is honestly not good at

Recorded up front, so a reader does not mistake the report's confidence for coverage.

- **Feel.** Latency, scroll, input responsiveness, how a transition lands. The UX loop said a
  screenshot cannot feel the app; a code review cannot either. Budget human time.
- **Whether anyone wants this.** The parity matrix says what Grok Bot has. It cannot say what users
  choose a product for, and the temptation to treat a filled-in matrix as a strategy is strong.
- **Grok Bot's actual behaviour.** The clean-room boundary in `PROVENANCE.md` §3 restricts sourcing
  to public documentation and observed behaviour, which means the parity matrix will contain honest
  `UNKNOWN` rows. That is the correct answer, and inventing rows to avoid it would trade the
  project's entire legal position for a tidier table.
