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

---

## Iteration 3 — 2026-08-24. T1-2: the history window.

**Closed:** T1-2. `history(id, Some(n))` was a raw line tail, so a window could open on a
`tool_result` whose `tool_use` it had cut off, and both vendors 400 that.

### Measure the blast radius before believing the report

The finding said windows "frequently" land badly. The test said 20 of 60, every third size, and
`DEFAULT_HISTORY = 40` is one of them. **A number changes what the fix is worth and whether the
priority was right**; "frequently" would not have told me the shipped default was itself broken.
Cheap here — the test had to be written anyway, so writing it *first* cost nothing and bought the
number.

### A self-healing failure is worse than a stable one

The window advances by one on the next append, so the next run succeeds. A person retypes and moves
on; nobody files it. A routine has no second try and loses the firing. **Rank a bug by who is
watching when it fires, not by how often it fires** — the unattended path deserves the weight.

### Two passes, and my own test caught why

`repair_window` first computed `asked` and `answered` over the whole window, then filtered once.
That keeps a call whose only answer is a result the same filter is dropping — creating exactly the
unanswered call the second half exists to prevent. Dropping a result can orphan a call, so calls
must be judged against **what survived**, not against the input. The reverse cannot happen, so two
ordered passes reach a fixed point with no loop.

I nearly cut `a_result_may_not_answer_a_call_that_comes_after_it` as unreachable in a well-formed
log. It is the test that found the bug. *Generalises:* **write the awkward ordering case even when
the data cannot currently produce it** — it is testing your algorithm, not your data.

### The anti-vacuity test is the one that matters

Every orphan assertion here is satisfied by a function returning an empty vector, and that repair
would look like a fixed bug while being far worse: a Bot answering every task having forgotten
everything. `a_window_that_is_already_legal_is_returned_untouched` is the only one that fails on it.
**When a fix removes things, the first test to write is the one asserting it does not remove too
much.**

---

## Iteration 3 (continued) — T1-4 and the page-suite flake.

**Closed:** T1-4 (most of it), and carried-forward decision #2 from `CHARTER.md` §4.

### The validation existed; it was filed where nobody looks

`permission ls` refuses every malformed rule and always did. `status` — help text "Is anything
wrong?" — said nothing, because `applied` reads the config with `load(home).unwrap_or_default()`.
*Generalises:* **when a check exists and a user still gets bitten, the bug is usually placement, not
absence.** Look for the swallowed `Result` before writing a new validator.

### A vocabulary split is a product defect, not a docs defect

`--approve ask` everywhere, `action = "require_approval"` in the rules file, and the README's own
example used `ask` and was rejected by the parser that reads it. The fix is a serde alias, not a
README edit: **when the product taught someone a word, accepting that word is not leniency.**

### Two seams caught two of my own bugs again

- `every_action_prints_under_a_name_a_config_may_use` indexed `rules[0]`, but `policy` starts from
  `Policy::default()` and appends the file's rules *after*. It passed for `allow` by coincidence.
  **A test that passes on one enum variant and not the others is usually indexing wrong, not
  finding a real asymmetry.**
- `!shown.contains("config")` failed on a healthy account because "config" is inside "none
  configured". **Substring tests on rendered output answer a different question than the one asked**
  — match the row label.

### The flake was a real defect, and I still cannot prove it is fixed

Three occurrences, three different tests, one signature (`chrome-error://chromewebdata/`). The
harness's loopback server did `let Ok(..) = accept().await else { return }`, so one transient error
killed it permanently — and sixty-nine concurrent Chromiums make `EMFILE` a live possibility.

*Generalises:* **before adding a retry, look for the thing that is genuinely broken.** The recorded
plan was "retry on chrome-error", and that was the right shape, but the accept loop was a cause
rather than a symptom, and only one of those two is worth fixing first.

*And the honesty rule:* **two clean runs is not evidence an intermittent fault is fixed.** An
intermittent seen three times across many runs shows a clean pair most of the time regardless. Say
what is actually established — a real defect that produces the observed symptom is gone — and not
what is merely hoped.

The retry that remains is bounded to a state that cannot be an assertion failure, and the predicate
is split out and tested in both directions, because the danger of a retry is never the retry: it is
the classification that decides what gets one.

---

## Iteration 4 — 2026-08-24. T1-3: perception and action share a coordinate system.

**Closed:** T1-3 (F-GT4). `browser.snapshot` hands out refs; `click`/`fill` take them.

### The defect was a missing correspondence, not a missing feature

`read` returned `innerText`; `click` demanded CSS selectors; nothing emitted one. Both halves
worked. Neither was wrong on its own. *Generalises:* **some of the worst defects are relationships,
not components** — no per-component review finds them, because every component passes. The question
that finds them is "can the output of A be used as the input to B", and it has to be asked
explicitly.

### Two mutations that did not fire, and both were real information

- Removing `isConnected` left the navigation test green, because a navigation replaces the whole JS
  context — refs are *gone*, not stale, so that path reports "no snapshot". My assertion only
  checked that the message mentioned `browser.snapshot`, which both messages do. **When two
  mechanisms produce a similar-looking message, an assertion on the shared part tests neither.**
  The genuinely stale case is an SPA re-render, and it now has its own test.
- Removing the explicit hidden-input filter changed nothing: the visibility filter already covers
  it. **A mutation that does not fire sometimes means the code is redundant, not that the test is
  weak.** Check which before strengthening the test — I deleted dead code that looked load-bearing.

### Separate inputs beat a sniffed string

`ref` and `selector` are distinct fields because `e1` is a legal CSS type selector, so any heuristic
eventually resolves a real selector as a ref and acts on the wrong element. **Ambiguity that only
shows up on someone else's input is the worst kind**, and it is free to avoid at the schema.

### Repo lints worth remembering

- `messages.rs` rejects multi-line string literals whose continuation bakes in the file's
  indentation. Backslash continuations written through a script keep getting lost; **write those
  strings on one line** and let rustfmt leave them alone.
- `every_tool_the_computer_offers_has_a_rule_of_its_own` catches a new tool with no policy rule.
  Adding a tool means adding a rule in `openbotd/src/policy.rs` in the same change.

---

## Iteration 5 — 2026-08-24. T1-3 finished: the click waits.

**Closed:** F-GT5. `click_and_settle` marks the document, clicks, and watches the mark vanish.

### A test that passes half the time is worse than no test

The first immediate-navigation test asserted only the resulting url. With the fix removed it still
passed about half the time, because whether `info()` lands before or after the context swaps *is*
the race being fixed. It would have reported the bug as fixed on exactly the run that mattered.

*Generalises:* **when the defect is a race, assert the thing that cannot happen by luck.**
`navigated` is only true if the swap was observed; the url is true whenever the timing was kind.
Look for the deterministic proxy rather than the visible symptom.

Corollary worth keeping: **a mutation that fails one of three tests is not a pass.** M1 failed the
deferred test and passed the immediate one, and the temptation was to call the mutation caught and
move on. The one that did not fire was the one with something to say.

### Detect the mechanism, not the symptom

The symptom is "url is wrong". The mechanism is "the JavaScript context was replaced" — which is
also exactly what invalidates snapshot refs, so one check answers both questions. Choosing the
mechanism gave `navigated` for free, and `navigated` is what the model actually needs to know.

### When the better mechanism is unavailable, say so at the site

`Page.frameNavigated` is the right way to do this and this CDP connection drops events. The comment
records that, so the next person does not rediscover the token trick and assume it was preferred.

### The same bug in two places

`tests/browser.rs` had the identical `let Ok(..) = accept().await else { return }` defect fixed in
`page.rs` last iteration. **After fixing a defect, grep for its shape** — a harness pattern copied
once is usually copied twice.

---

## Iteration 6 — 2026-08-25. T1-5: every forwarded call ends.

**Closed:** the hub half of T1-5 / F-RS2. Tier 1 is now clear.

### The protocol already had the answer

`WorkspaceUnavailable`, `Disconnect`, `InFlightCancelled` were defined in `openbot-proto` and had
**no uses anywhere outside that crate**. Someone designed the failure vocabulary and nothing ever
spoke it.

*Generalises, and it is the same shape as the routine scheduler:* **an unused type in a protocol
crate is a strong signal, not dead code.** Grep for the types a protocol defines and never uses —
each one is a case somebody thought about and left unwired, which is exactly where the silent
failures live. Two of the six biggest findings this loop have had this shape.

### The good pattern was 350 lines away

`session_bind_server` wraps its request in a 30-second timeout. `tool_call` — the one that runs for
minutes and reaches a browser — had none. **When a file does something carefully in one place and
not in another, that is a finding, and the careful version is the design you should copy** rather
than invent.

### `retain` by one field answers half a question

`disconnect` filtered `calls` and `relays` by `origin`, which is correct and incomplete: a relay has
two ends. The target end had no cleanup at all. *Generalises:* **when a record names two parties,
any cleanup that mentions one of them is worth checking against the other.**

### The anti-vacuity test, again

Both failure tests are satisfied by a hub that fails every call instantly. The third test — a
healthy call that takes 1.2 seconds and must succeed — is the only one that rules that out. This is
the third iteration running where the test that mattered most was the one asserting the fix did not
overreach.

### Bind params vs frame session

`session_bind_server` takes `server_id` in params and the session in the *frame*, not in params.
Putting it in params fails with a bare error. Cost 10 minutes; worth writing down because the test
harness helpers hide it.

---

## Iteration 7 — 2026-08-25. Tier 2: the door, and what walks through it.

**Closed:** the Origin half of T2-1, and the environment half of T2-2.

### Two independent halves beat one clever check

A browser cannot suppress `Origin`, and a browser cannot set request headers. Either fact alone
closes the drive-by path; together they also cover a local process, which neither does alone.
*Generalises:* **when a threat has two structural properties you can test, test both** — the second
costs little and covers the case where the first turns out to be softer than you thought.

The check is on the *presence* of `Origin`, not its value. An allow-list of origins is the weaker
test: it invites `null`, and it invites someone to add an entry later without noticing they have
reopened the door.

### Verify what a security change breaks before shipping it

The Origin check would have broken the product if anything legitimate connected from a page. It does
not: the viewer is `browser → HTTP → openbot watch → WebSocket → hub`, and the Tauri UI has no
`WebSocket` at all. **Checking that took two greps and was the difference between a fix and an
outage.**

### An allow-list is wrong in the safe direction

`shell.exec` needed an environment filter. A deny-list has to be right about every name a credential
might have and is wrong the first time it guesses; an allow-list is wrong by withholding something a
command wanted. **When both designs will be wrong sometimes, pick the one whose failure is a missing
variable rather than a leaked key.**

`sh -lc` → `sh -c` mattered as much as the filter: a login shell sources profiles, which can
re-export what the allow-list withheld and can `cd` out of the only confinement the tool has.

### Correct the claim when you can only fix half

The environment vector is closed; `cat ~/.openbot/secrets.json` still works. `isolation.rs` now says
which half is which instead of claiming both. **A partial fix plus an accurate claim is a good
state; a partial fix under the old claim is worse than no fix**, because it reads as done.

### The flake came back, which was the point of saying it might

Fourth occurrence, fourth test, after two plausible fixes. Rather than guess again, the failure path
now reports whether the retry ran and whether the harness's own server still accepts a plain TCP
connection from the test process. *Generalises:* **after two failed fixes to an intermittent fault,
stop fixing and start instrumenting.** The third guess is worth less than one conclusive
observation, and two of my three data points were unusable because nobody had recorded which
component died.

---

## Iteration 8 — 2026-08-25. One command, one terminal.

**Closed:** T3-1 / F-CD3 / F-CD5. `openbot run` starts a computer when one is not already up.

### The ranking was measuring the wrong thing

`BACKLOG.md` orders by reach × cost-of-living-without, computed *within the product as it is*. That
put "five steps and two terminals" below "the config tools lie" — but the first is what everyone
meets first and the second is what they meet only if they get that far. **A backlog ranked inside
the current product will systematically under-rank the things that stop people entering it**, because
the people it costs are not yet users and so do not appear in "reach". Re-ranked Tier 3 and said so
in the file.

### An implementation detail leaking into the first thing anyone types

The hub/guest split is correct and worth keeping — in a deployment the guest runs elsewhere. It was
never a reason for the split to be the *user's* problem on a laptop. *Generalises:* **a boundary
that is right for the deployed shape is not automatically right for the first-run shape**, and the
fix is usually to make the common case implicit rather than to document the split better.

### Reuse the real thing rather than a lightweight version of it

`hub_or_start` constructs `up::Up` and calls `start()`. Writing a smaller in-process hub for this
path would have been quicker and would have drifted: two ways to bring up a computer, differing in
what they configure. The only differences are deliberate and commented — ephemeral port, no snapshot
timer, no routine timer.

### Place the teardown where the exits are

`?` paths unwind and `kill_on_drop` reaps the browser. `std::process::exit` runs no destructors at
all, so the stop belongs immediately before those calls, not at the end of the happy path.
**Whenever a function ends with `process::exit`, cleanup written above it in the "normal" place is
cleanup that never runs.**

### Two tests that separate "worked" from "worked for the reason claimed"

One asserts a fresh install reaches a result in one command; the other asserts the "starting a
computer" line appeared. Either alone is ambiguous — a hub left running by another test would pass
the first. Together they pin the mechanism.
