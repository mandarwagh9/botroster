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
