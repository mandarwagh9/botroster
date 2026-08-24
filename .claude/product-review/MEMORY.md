# Memory

Read this first, every iteration. `LOOP.md` in the UX loop says memory is the only thing that makes
a loop compound instead of random-walk, and that was true there. Append; do not rewrite history.
A lesson that only describes what happened is half a lesson — write the part that generalises.

---

## Iteration 1 — 2026-08-24. Standing up the gate.

**Built:** `scripts/review.sh` (eight checks), `CHARTER.md`, `isolation-allowlist.txt`. Dispatched
six department reviews. Two findings closed.

### Four of the eight gates were wrong on their first run

Every one was caught by planting the defect the gate exists to catch. None would have been caught by
reading the script. **A gate is a claim, so CONTRIBUTING.md's rule applies to it unchanged: break
the thing it guards and confirm it notices.** Writing the gate and watching it pass on a clean tree
proves nothing at all — a gate that always passes and a gate that works look identical there.

1. **`cargo tree` includes dev-dependencies.** So the isolation pre-check reported that
   `openbot-agent` depends on `openbotd` — true of its test profile, false of everything that ships.
   `-e normal,build` is the definition `isolation.rs` already uses, and matching an existing test's
   definition beats inventing a second one. *Generalises:* the first false alarm is what decides
   whether a gate survives its first month.

2. **A brace counter that has not been told about string literals is wrong on exactly the files
   that test a parser.** The assertion-free-test scan reported eleven tests with no assertions, all
   of which had assertions: a fixture containing `"this is not toml {{{"` left the depth counter
   three deep, so every test after it in that file was measured against the wrong closing brace.
   Literals, chars and comments are now blanked to spaces — length preserved, so offsets still
   address the original.

3. **Substring matching on a path answers a question about prefixes, not about coverage.** The
   PROVENANCE check accepted a bare directory match, so one recorded file silently vouched for every
   future sibling. Then the fix over-corrected and demanded a literal `dir/*`, which could not read a
   row globbing by filename. Both wrong, opposite directions. Now: extract the paths the table
   quotes in backticks and glob-match with `case`, which is the only glob matcher POSIX sh has —
   and the pattern must be **unquoted** in the `case` arm or `*` becomes a literal asterisk.

4. **A word scan for a policy violation is unusable and an allowlist is the shape that survives.**
   The rule is CLAUDE.md's "do not write copy that implies isolation the project does not have".
   Scanning for sandbox/isolat/VM/container found five hits, all legitimate — prose about the
   *reference* product's sandbox, a config format named "sandbox", "credential isolation" (a real
   and different property), and a Bot's coat going on a DOM container. **A gate that cries wolf five
   times out of five gets deleted inside a week.** So: every occurrence reviewed once and recorded
   in `isolation-allowlist.txt` *with the reason*, and anything not on the list fails. Cost is a
   line of upkeep when the text legitimately changes; benefit is that a new overclaim cannot arrive
   quietly. The reasons matter as much as the list — a mute allowlist is a list nobody can audit.

### `grep -c` prints 0 and exits 1

So the obvious `count=$(grep -c . file || echo 0)` yields the two-line string `"0\n0"` and every
later arithmetic test on it is a syntax error. `|| true` keeps the printed count. Cost 15 minutes
twice, in two different gates, in the same run.

### The finding under the finding

`an_explicit_browser_path_is_honoured_when_it_exists` was flagged as assertion-free. It was — but
the interesting part was underneath. It called the search twice, discarded both results, and set an
override to a path that does *not* exist, so the one case its name promises was never exercised.
Following the name into the code found the real defect: a missing `OPENBOT_BROWSER` returned `None`,
indistinguishable from nothing-installed, and the caller rendered that as *"no Chromium-family
browser found; set OPENBOT_BROWSER to its path"* — advice to do the thing the user had just done,
with no hint the variable was even read. CLAUDE.md sends people to that variable when their browser
is somewhere unusual, so it landed on the users already having the hardest time.

Three sources disagreed and nothing asserted: the function returned `None`, its comment said "a
mistake to report", and the test's comment said "falls through to discovery". All three cannot be
right, and with no assertion anywhere nothing forced the question.

***Generalises, and this is the one to remember:*** **a test with no assertion is worth following,
not just fixing.** The absence of an assertion is where nobody was forced to say what the behaviour
should be — so it is where a disagreement between the code, its comment and its name can sit
undisturbed for as long as it likes. Grade the finding by what is underneath it, not by the lint
that surfaced it.

Secondary: the old test mutated the process environment with `set_var`, racing every other test in
the binary. It asserted nothing, so the race could never surface as a failure. **Passing an input in
rather than reading it from global state is usually what makes a thing testable at all** — the error
variant was the patch, the resolver split was the fix.

### Screenshots had no PROVENANCE row

Seven first-party captures of our own client. Dull answer, and the rule still applies: the one asset
that arrived with an unconfirmed origin arrived precisely because nobody thought a picture counted.
**A rule with an unwritten exception for the boring cases is not a rule**, and it fails on exactly
the case it was written for.

---

## Iteration 2 — 2026-08-24. Six departments, and two of the top items shipped.

**Reviews in:** 78 findings across six reports, triaged into one ranked order in `BACKLOG.md`.
**Closed:** T2-4 (Chrome's own sandbox), T1-1 (routines never fired).

### The pattern across all six reports

Three of the top five findings are **features that are complete and never invoked**. Not
half-built — the routine scheduler parses cron, computes due-ness and executes, all correctly, and
nothing called it. `DIRECTION.md` was adopted as launch decision #1 and never implemented.
`context_budget` has one caller in the workspace and it is a test.

*Generalises:* **a component with passing unit tests and no caller looks exactly like a working
feature from inside the code.** Every test of the routine machinery passed, forever, while the
feature did nothing at all. The question that finds this class is not "is it correct" but "who calls
it", and no test here asked it. The live test now added asks it the only way that works — start the
shipped binary, wait on a wall clock, assert something was recorded.

### A false comment became a live vulnerability

`browser.rs` said "the guest is already a sandbox" and used that as the reason to pass
`--no-sandbox` to Chrome. The premise contradicted `CLAUDE.md` outright, and the consequence was
that the renderer parsing model-chosen pages ran with its own boundary switched off.

*Generalises:* **a wrong comment is not a documentation problem, it is a latent code problem.** The
next person to touch that line reads the comment and writes code consistent with it. Fifteen places
across five crates called the guest a sandbox; nobody lied, each author matched their neighbours.
Correct the vocabulary, not just the sentence.

The gate had the matching blind spot: G5 scanned prose only, reasoning that shipped text is what a
user believes. The defect lived one floor below, in a comment. **Aim an honesty gate at what a
future author will read, not only at what a user will read.**

### The end-to-end run caught a bug in my own fix

The timer built `openbot --home H routine tick`. `--home` is not a global argument, so clap rejects
it in the root position and the child would have errored once a minute forever. **The unit test
passed** — it asserted the joined argument list contained `"--home H"`, which was true and
irrelevant.

*Generalises, and it is iteration 1's lesson arriving from a different direction:* **a test that
string-matches what the code just produced tests nothing** — it restates the implementation. The fix
was to hand the arguments to the real `clap` parser and destructure the result, so it fails on
anything the binary would reject. Cheap rule: if the assertion would still pass when the production
code is replaced by the literal it emits, the test is a mirror.

### A test can pass through the wrong seam

`routines_line` was unit tested for both states, and deleting the one line in `banner` that *calls*
it left the test green. Unit-testing a renderer proves the renderer and says nothing about whether
anyone renders it.

*Generalises:* extracting a function to make it testable **creates a new untested seam — the call
site.** Test the seam, not only the extracted half. Covered now by a live test reading the banner
`up` actually printed.

### A run that fails is still proof

The live test asserts a run was **recorded**, not that it succeeded. No model is configured, so it
records a failure — and a recorded failure proves the clock ticked, which is the property that did
not exist. Asserting success would have needed a model and would have been testing the agent.
**Pick the weakest assertion that still separates the bug from its absence.** It is cheaper, it is
faster, and it does not drift into testing something else.

### Housekeeping

A `git commit -F-` heredoc containing apostrophes failed to parse twice in this session. Write the
message to a file and use `git commit -F <file>`. The second time it silently took a `python`
block with it, so `MEMORY.md` was not written when I thought it had been — **check that a combined
command actually ran before trusting its side effects.**
