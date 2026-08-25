//! The page's own logic, driven in a real browser.
//!
//! `ui/main.js` holds the approval queue, the session filter, the refusal
//! mapping, the permission editor and the credentials form: decisions with
//! security consequences, which need to be checked on every push.
//!
//! ## Why this shape
//!
//! Three alternatives were rejected:
//!
//! * Driving the real window over CDP works, but WebView2 speaks the DevTools
//!   protocol and webkit2gtk does not, so it is a Windows-only tool and CI
//!   would never run it.
//! * jsdom means adding a JavaScript toolchain to a Rust workspace for one
//!   file, plus a provenance row, plus a second engine whose quirks are not
//!   the ones a person's webview has.
//! * A `file://` page loses the origin behaviour the real page has.
//!
//! What is used instead is the browser this project already ships and already
//! tests with: the guest's Chromium, which CI installs and requires. The page
//! is served over loopback exactly as `openbot-guest`'s own browser tests serve
//! theirs.
//!
//! ## What is real and what is doubled
//!
//! The markup and the script are the shipped files, read off disk. Only
//! `window.__TAURI__` is a double: the IPC boundary. Every `invoke` is
//! recorded so a test can assert what the page sent, and `__fire` delivers an
//! event the way Tauri would, so what is under test is the page's response
//! rather than the stub.

use std::sync::Arc;
use std::time::Duration;

use openbot_desktop::roster;
use openbot_guest::browser::{find_browser, Browser};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A stub reply, built from the type the shell really returns.
///
/// Hand-written JS fixtures cannot fail when the Rust shape changes: a stub
/// that drifts from the command's output goes on passing as long as nothing
/// asserts on the part it feeds. Building fixtures from the type makes that
/// drift a compile error.
///
/// Only the group fixtures are built this way. The rest are flat objects of
/// stable types; do not read this helper as evidence the file is covered.
fn stub<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("a fixture the shell could have sent")
}

/// A group the way `openbot group ls --json` reports one.
fn a_group(id: &str, name: &str, members: &[(&str, &str)]) -> roster::Group {
    roster::Group {
        id: id.to_owned(),
        name: name.to_owned(),
        members: members
            .iter()
            .map(|(id, name)| roster::Member {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect(),
        messages: 0,
    }
}

/// Stands in for Tauri's IPC. Inserted immediately before `main.js`, so the
/// page finds it exactly where the real one is.
///
/// `invoke` answers from `window.__replies` when a command has an entry there
/// and returns `null` otherwise, which is what most of these commands do.
const STUB: &str = concat!(
    "<script>\n",
    include_str!("fixture/tauri-stub.js"),
    "</script>\n"
);

/// Serve the shipped `ui/` over loopback, with the stub spliced in.
async fn serve() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let html = std::fs::read_to_string(dir.join("index.html")).expect("index.html");
    let js = std::fs::read_to_string(dir.join("main.js")).expect("main.js");
    let css = std::fs::read_to_string(dir.join("styles.css")).expect("styles.css");

    // One splice, asserted, so a rename of the script tag fails loudly here
    // rather than silently serving a page with no stub and no explanation.
    let marker = r#"<script src="main.js"></script>"#;
    assert!(
        html.contains(marker),
        "index.html no longer loads main.js the way this harness expects"
    );
    let html = html.replace(marker, &format!("{STUB}{marker}"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let files: Arc<Vec<(String, String, String)>> = Arc::new(vec![
        ("/".into(), "text/html".into(), html),
        ("/main.js".into(), "text/javascript".into(), js),
        ("/styles.css".into(), "text/css".into(), css),
    ]);
    tokio::spawn(async move {
        // One failed `accept` does not end the server.
        //
        // This was `let Ok(..) = accept().await else { return }`, so a single
        // transient error shut the server down permanently and every later
        // navigation in that test got Chrome's network error page. The suite
        // runs sixty-nine of these, each with its own Chromium and its own
        // loopback server, so the descriptor limit is a real ceiling and
        // `EMFILE` is exactly the transient error this used to treat as fatal.
        //
        // Not an infinite retry: a listener that is genuinely gone would spin
        // hot forever. Consecutive failures are counted and reset by any
        // success, because the case worth surviving is a burst under load and
        // the case worth giving up on is a socket that will never accept again.
        let mut consecutive = 0;
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(pair) => {
                    consecutive = 0;
                    pair
                }
                Err(e) => {
                    consecutive += 1;
                    if consecutive > 16 {
                        eprintln!("test server giving up after 16 accept failures: {e}");
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
            };
            let files = Arc::clone(&files);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_owned();
                let found = files.iter().find(|(p, _, _)| *p == path);
                let (status, kind, body) = match found {
                    Some((_, kind, body)) => ("200 OK", kind.as_str(), body.as_str()),
                    None => ("404 Not Found", "text/plain", ""),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://{addr}/")
}

/// A browser on the page, or a loud skip.
///
/// The skip says so out loud, and CI sets `OPENBOT_REQUIRE_BROWSER` so a missing
/// browser there is a failure rather than a green run over tests that never
/// executed.
async fn page() -> Option<(Browser, tempfile::TempDir)> {
    if find_browser().is_none() {
        assert!(
            std::env::var("OPENBOT_REQUIRE_BROWSER").is_err(),
            "OPENBOT_REQUIRE_BROWSER is set and no browser was found"
        );
        eprintln!("skipping: no browser installed");
        return None;
    }
    let profile = tempfile::tempdir().expect("a profile");
    let browser = Browser::launch(profile.path()).await.expect("launch");
    let url = serve().await;
    browser.navigate(&url).await.expect("navigate");
    // Retried once, and only when the navigation demonstrably never happened.
    //
    // This suite has flaked three times, on three different tests, always with
    // `chrome-error://chromewebdata/` as the page's location: Chrome's own
    // network error page, meaning the request did not complete. Three different
    // tests with one signature is the harness, not any of them.
    //
    // The narrowness is the whole point. A blanket retry would hide real
    // failures, and a test that passes on the second try is worth less than one
    // that fails honestly. `on_chrome_error_page` is what keeps this to the one
    // case that cannot be an assertion failure, and it is tested below rather
    // than trusted.
    let mut retried = false;
    if on_chrome_error_page(&browser).await {
        retried = true;
        eprintln!("navigation landed on Chrome's error page; retrying once");
        tokio::time::sleep(Duration::from_millis(250)).await;
        browser.navigate(&url).await.expect("navigate (retry)");
    }
    // The page registers its listeners and asks `connected` on load.
    //
    // Five seconds is not a slow-load allowance. Listeners are normally
    // registered on the first poll, within a few milliseconds, so exhausting
    // the budget means something is wrong, and the useful thing this loop can
    // do then is report what. The last error is kept rather than discarded so
    // a dead target, a failed navigation and a page that threw before its
    // listeners ran are distinguishable in the panic.
    let mut last = Ok(String::new());
    for _ in 0..50 {
        last = browser
            .text_of("String(!!window.__listeners['permission-request'])")
            .await;
        if matches!(&last, Ok(v) if v == "true") {
            return Some((browser, profile));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Asked once, at the end: what state is the page actually in? Each of
    // these can fail on its own, and a failure here is itself informative: a
    // browser that cannot evaluate anything is a different problem from a
    // page sitting at `about:blank`.
    let readiness = browser
        .text_of("document.readyState + ' @ ' + location.href")
        .await;
    let listeners = browser
        .text_of("Object.keys(window.__listeners || {}).sort().join(',') || '(none)'")
        .await;
    let title = browser.text_of("document.title || '(no title)'").await;

    // Is the loopback server this page came from still accepting?
    //
    // This is the question the previous three occurrences could not answer, and
    // the reason a fourth was no more informative than the first. Chrome's
    // error page says "the request did not complete" and nothing about why.
    // A plain TCP connect, from this process, separates the two candidates
    // conclusively: the harness's own server having died, or something in
    // Chrome. Guessing between those is what produced two fixes that could not
    // be shown to work.
    let server = match url.trim_start_matches("http://").split('/').next() {
        Some(addr) => match tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(addr.to_owned()),
        )
        .await
        {
            Ok(Ok(_)) => "accepting connections".to_owned(),
            Ok(Err(e)) => format!("REFUSED: {e}"),
            Err(_) => "did not accept within 2s".to_owned(),
        },
        None => "could not parse the address".to_owned(),
    };
    panic!(
        concat!(
            "the page never finished loading after 5s (it normally takes ~2ms).\n",
            "  last check: {0:?}\n",
            "  readyState/url: {1:?}\n",
            "  listeners: {2:?}\n",
            "  title: {3:?}\n",
            "  navigation retried: {4}\n",
            "  test server ({5}): {6}"
        ),
        last, readiness, listeners, title, retried, url, server
    );
}

/// Did the navigation land on Chrome's network error page?
///
/// `chrome-error://chromewebdata/` is what Chrome shows when a request could
/// not be completed at all — connection refused, reset, empty response. It is
/// not a page that loaded and misbehaved, so retrying it cannot mask a broken
/// assertion: there is nothing to assert on.
///
/// Anything else is a real answer and is left alone, including a browser that
/// will not evaluate script, because "cannot ask" is not "navigation failed"
/// and retrying the first would turn a dead target into a slow one.
async fn on_chrome_error_page(browser: &Browser) -> bool {
    matches!(
        browser.text_of("location.href").await,
        Ok(href) if is_chrome_error_page(&href)
    )
}

/// The classification, split out so it can be tested without a browser.
///
/// This predicate is the only thing standing between "retry a navigation that
/// never happened" and "retry until the assertion passes", so it is the part
/// that has to be right.
fn is_chrome_error_page(href: &str) -> bool {
    href.starts_with("chrome-error://")
}

#[test]
fn only_a_navigation_that_never_happened_is_retried() {
    // The failures seen in this suite.
    assert!(is_chrome_error_page("chrome-error://chromewebdata/"));

    // Everything a working or a genuinely failing test looks like. If any of
    // these were retried, a real defect could pass on the second attempt, which
    // is worse than the flake being fixed.
    for href in [
        "http://127.0.0.1:53211/",
        "http://127.0.0.1:53211/#approvals",
        "about:blank",
        "",
        "https://chrome-error.example.com/",
        "data:text/html,<p>hi",
    ] {
        assert!(
            !is_chrome_error_page(href),
            "{href} is a page that loaded; retrying it would hide a real failure"
        );
    }
}

/// An approval the way the shell emits one.
fn ask(id: &str, session: &str, tool: &str) -> String {
    format!(
        r#"{{"id":"{id}","session":"{session}","tool":"{tool}",
            "fields":[{{"name":"path","value":"notes.md","long":false}}],
            "options":[{{"id":"allow-once","name":"Allow once","kind":"allow_once",
                         "danger":false}},
                       {{"id":"reject-once","name":"Not this time","kind":"reject_once",
                         "danger":true}}]}}"#
    )
}

/// Put the page in a state where a conversation is open, without a backend.
async fn open_session(b: &Browser, session: &str) {
    b.text_of(&format!(
        r#"(() => {{ window.__replies.open_bot =
             {{ session: "{session}", name: "Talent Scout", history: [] }};
           window.__replies.roster = [{{ id: "talent-scout", name: "Talent Scout",
             title: "", description: "", hidden: false, messages: 0 }}];
           document.getElementById("connect-btn").click();
           return "ok"; }})()"#
    ))
    .await
    .expect("connect");
    // connect → roster → click the Bot.
    for _ in 0..50 {
        let bots = b
            .text_of("String(document.querySelectorAll('#bots .bot').length)")
            .await
            .unwrap_or_default();
        if bots == "1" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    b.text_of("document.querySelector('#bots .bot').click(); 'ok'")
        .await
        .expect("open the Bot");
    for _ in 0..50 {
        if b.text_of("String(!document.getElementById('composer').classList.contains('hidden'))")
            .await
            .unwrap_or_default()
            == "true"
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the conversation never opened");
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

/// Polls a page-side predicate until it holds or the deadline passes.
///
/// For anything the page does on a timer. A fixed sleep equal to the timer
/// races it: on a loaded runner the timer has not fired when the sleep
/// returns, and the test fails for being run on a slower machine. Waiting for
/// the condition asserts the behaviour and not the clock.
async fn wait_until(b: &Browser, predicate: &str, deadline: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if b.text_of(predicate).await.unwrap_or_default() == "true" {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Two approvals arriving together must both stay answerable. Rendering each
/// ask over the last would discard the first one's buttons (which close over
/// its id), leaving it unanswerable until it timed out with the turn stalled
/// behind it. `--demo-tools` asks serially; a real model with parallel tool
/// calls does not.
#[tokio::test(flavor = "multi_thread")]
async fn two_approvals_at_once_are_queued_not_overwritten() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(&format!(
        "window.__fire('permission-request', {}); \
         window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write"),
        ask("a2", "s1", "shell.exec")
    ))
    .await
    .expect("two asks");
    settle().await;

    assert_eq!(
        b.text_of("document.getElementById('dialog-tool').textContent")
            .await
            .unwrap(),
        "fs.write",
        "the second ask overwrote the first"
    );
    assert_eq!(
        b.text_of("document.getElementById('dialog-queue').textContent")
            .await
            .unwrap(),
        "1 more waiting",
        "the person is not told another decision is behind this one"
    );

    // Answer the first; the second must take its place, still answerable.
    b.text_of("[...document.querySelectorAll('#dialog-options button')].find(x=>x.textContent==='Allow once').click(); 'ok'")
        .await
        .expect("answer");
    settle().await;

    assert_eq!(
        b.text_of("document.getElementById('dialog-tool').textContent")
            .await
            .unwrap(),
        "shell.exec",
        "the queued ask never surfaced"
    );
    let sent = b
        .text_of("JSON.stringify(window.__sent('answer_permission').map(c=>c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("a1") && sent.contains("allow-once"),
        "the first ask was answered with the wrong id or option: {sent}"
    );
    assert!(!sent.contains("a2"), "the second was answered too: {sent}");
}

/// An ask for a conversation this window is not showing has nobody to answer
/// it. It is refused on sight (fail closed, at the speed of a click rather
/// than of a five-minute timeout) and never put in front of a person as
/// though it belonged to what they are looking at.
#[tokio::test(flavor = "multi_thread")]
async fn an_ask_for_another_conversation_is_refused_not_shown() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("other", "a-different-session", "fs.write")
    ))
    .await
    .expect("a foreign ask");
    settle().await;

    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "an ask from another conversation was shown as if it were this one's"
    );
    let sent = b
        .text_of("JSON.stringify(window.__sent('answer_permission').map(c=>c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("other") && sent.contains(r#""optionId":"""#),
        "the foreign ask was not refused: {sent}"
    );
}

/// Allow and deny must not look alike in a security dialog, and the page
/// styles them from the protocol's `kind` rather than from the button's words.
#[tokio::test(flavor = "multi_thread")]
async fn declining_does_not_look_like_allowing() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an ask");
    settle().await;

    let classes = b
        .text_of("JSON.stringify([...document.querySelectorAll('#dialog-options button')].map(x=>[x.textContent,x.className]))")
        .await
        .unwrap();
    assert!(
        classes.contains(r#"["Allow once","primary"]"#),
        "the allow is not the accent action: {classes}"
    );
    assert!(
        classes.contains(r#"["Not this time","danger"]"#),
        "declining is styled as though it were an allow: {classes}"
    );
    // And the page adds no refusal of its own when the agent offered one:
    // two buttons that both mean no, meaning different things, is a defect.
    assert!(
        !classes.contains("Refuse"),
        "a second way to decline was added beside the agent's: {classes}"
    );
}

/// Stopping withdraws the questions with the turn. The ids come back from
/// `cancel`, and a dialog left up afterwards offers a choice that no longer
/// exists; the buttons would answer "no such permission request".
#[tokio::test(flavor = "multi_thread")]
async fn stopping_takes_down_the_dialog_it_withdrew() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__replies.cancel = [\"a1\"]; window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an ask");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "the dialog should be up before it is withdrawn"
    );

    b.text_of("document.getElementById('cancel').click(); 'ok'")
        .await
        .expect("stop");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the dialog stayed up after the question behind it was withdrawn"
    );
}

/// A chunk for another conversation must not land in the transcript on screen.
#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_from_another_conversation_is_not_rendered() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"agent", text:"mine"});
           window.__fire('chunk', {session:"elsewhere", kind:"agent", text:"not mine"}); 'ok'"#,
    )
    .await
    .expect("two chunks");
    settle().await;

    let log = b
        .text_of("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert!(
        log.contains("mine"),
        "the session's own words are missing: {log}"
    );
    assert!(
        !log.contains("not mine"),
        "another conversation's words were rendered into this one: {log}"
    );
}

/// A rule that stops a call needs a reason, and the page must send `null`
/// rather than `""` so the binary's own refusal is what a person sees. An
/// empty string would be stored as a blank explanation nobody can act on.
#[tokio::test(flavor = "multi_thread")]
async fn a_blank_reason_is_sent_as_absent_rather_than_empty() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"document.getElementById('rules-btn').click();
           document.getElementById('rule-action').value = 'deny';
           document.getElementById('rule-tool').value = 'shell.exec';
           document.getElementById('rule-reason').value = '   ';
           document.getElementById('rule-form').dispatchEvent(new Event('submit',{cancelable:true}));
           'ok'"#,
    )
    .await
    .expect("add a rule");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('policy_add').map(c=>c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains(r#""reason":null"#),
        "a blank reason was sent as a value rather than as absent: {sent}"
    );
}

/// Opening a group shows its thread and a live composer. A group session
/// names the group, and the agent picks the answering member per message from
/// the `@mention`.
#[tokio::test(flavor = "multi_thread")]
async fn opening_a_group_shows_the_thread_and_a_live_composer() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let mut launch = a_group(
        "website-launch",
        "Website Launch",
        &[("researcher", "Researcher"), ("writer", "Writer")],
    );
    launch.messages = 2;
    // Injected rather than formatted, so the JS keeps its own braces.
    b.text_of(
        &r#"(() => {
             window.__replies.groups = [__GROUP__];
             window.__replies.open_group = { session: "g1", name: "Website Launch",
               history: [
                 { session: "g1", kind: "user", text: "@Researcher gather the sources" },
                 { session: "g1", kind: "agent", text: "Handing the draft to @Writer" }] };
             window.__replies.group_log = [
               { session: "Website Launch", kind: "user", text: "@Researcher gather the sources" },
               { session: "Website Launch", kind: "agent", text: "Handing the draft to @Writer" }];
             return "ok"; })()"#
            .replace("__GROUP__", &stub(&launch)),
    )
    .await
    .expect("stage a group");
    b.text_of("document.getElementById('toggle-hidden').click(); 'ok'")
        .await
        .expect("force a roster refresh");
    settle().await;

    b.text_of("document.querySelector('#groups .bot').click(); 'ok'")
        .await
        .expect("open the group");
    settle().await;

    let log = b
        .text_of("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert!(
        log.contains("Handing the draft to @Writer"),
        "the handoff, the reason groups exist, is not shown: {log}"
    );

    // Members by the names a person reads. A group stores ids and a rename
    // keeps them, so rendering the list raw would put slugs under an entry
    // the sidebar shows by name.
    let members = b
        .text_of("document.querySelector('#groups .bot-sub').textContent")
        .await
        .unwrap();
    assert_eq!(members, "Researcher, Writer", "{members}");
    // The composer is live: a group session names the group and the agent
    // picks the answering member per message.
    assert_eq!(
        b.text_of("String(document.getElementById('composer').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "a group cannot be spoken to"
    );
    let sent = b
        .text_of("JSON.stringify(window.__sent('open_group').map(c=>c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("Website Launch"),
        "the group was not opened as a session: {sent}"
    );
}

/// The palette. Switching between teammates is the most frequent thing a
/// person does here, and its list comes from the roster the sidebar already
/// rendered: one source, so it can never offer a teammate the sidebar does
/// not have.
#[tokio::test(flavor = "multi_thread")]
async fn the_palette_finds_a_bot_and_opens_it() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"document.dispatchEvent(new KeyboardEvent('keydown',{key:'k',ctrlKey:true,bubbles:true}));
           'ok'"#,
    )
    .await
    .expect("open the palette");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('palette').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "Ctrl+K did not open the palette"
    );

    // Teammates lead: they are what a palette is opened for.
    let first = b
        .text_of("document.querySelector('.palette-item span').textContent")
        .await
        .unwrap();
    assert_eq!(
        first, "Talent Scout",
        "a Bot should lead the list, got {first}"
    );

    // Filtering, then Enter, must open the Bot and close the palette.
    b.text_of(
        r#"(() => { const i = document.getElementById('palette-input');
             i.value = 'talent'; i.dispatchEvent(new Event('input'));
             i.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
             return 'ok'; })()"#,
    )
    .await
    .expect("choose");
    settle().await;

    assert_eq!(
        b.text_of("String(document.getElementById('palette').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the palette stayed open over the thing it opened"
    );
    let sent = b
        .text_of("String(window.__sent('open_bot').length)")
        .await
        .unwrap();
    assert_eq!(sent, "2", "choosing a Bot in the palette did not open it");
}

/// Escape must not dismiss an approval. It is a question the Bot is waiting
/// on, not a dialog to get rid of; closing it would leave the turn stalled
/// with nothing on screen to explain why.
#[tokio::test(flavor = "multi_thread")]
async fn escape_closes_a_panel_but_never_an_approval() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A panel: Escape closes it.
    b.text_of("document.getElementById('rules-btn').click(); 'ok'")
        .await
        .expect("settings");
    settle().await;
    b.text_of(
        "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'ok'",
    )
    .await
    .expect("escape");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('rules-dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "Escape did not close the settings panel"
    );

    // An approval: Escape leaves it alone.
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an ask");
    settle().await;
    b.text_of(
        "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'ok'",
    )
    .await
    .expect("escape");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "Escape dismissed an approval the Bot is still waiting on"
    );
    let sent = b
        .text_of("String(window.__sent('answer_permission').length)")
        .await
        .unwrap();
    assert_eq!(
        sent, "0",
        "Escape answered the approval on the person's behalf"
    );
}

/// Messages arrive in the palette after the names do, and a slow answer to an
/// old query must not land on top of a new one. The race only shows when
/// somebody types fast, which is why it is asserted here.
#[tokio::test(flavor = "multi_thread")]
async fn a_stale_search_answer_does_not_land_on_a_newer_query() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"(() => {
             window.__replies.search = (args) =>
               args.query.includes("renewal")
                 ? [{ kind: "bot", name: "talent-scout", at: 3, role: "user",
                      text: "…the renewal risk for Acme…" }]
                 : [];
             document.dispatchEvent(new KeyboardEvent('keydown',{key:'k',ctrlKey:true,bubbles:true}));
             return "ok"; })()"#,
    )
    .await
    .expect("open the palette");
    settle().await;

    // Type a query, then immediately change it. The first answer resolves
    // against a box that no longer says what it said.
    b.text_of(
        r#"(() => { const i = document.getElementById('palette-input');
             i.value = 'renewal'; i.dispatchEvent(new Event('input'));
             i.value = 'zzzz';    i.dispatchEvent(new Event('input'));
             return 'ok'; })()"#,
    )
    .await
    .expect("type twice");
    settle().await;

    let shown = b
        .text_of(
            "JSON.stringify([...document.querySelectorAll('.palette-item')].map(x=>x.textContent))",
        )
        .await
        .unwrap();
    assert!(
        !shown.contains("renewal risk"),
        "an answer to the previous query landed on the current one: {shown}"
    );

    // And a query that stands still does get its messages.
    b.text_of(
        r#"(() => { const i = document.getElementById('palette-input');
             i.value = 'renewal'; i.dispatchEvent(new Event('input')); return 'ok'; })()"#,
    )
    .await
    .expect("type once");
    // The message hit arrives after the debounce and the round trip; wait for
    // it rather than for a fixed interval that a slow runner can miss.
    wait_until(
        &b,
        "String([...document.querySelectorAll('.palette-item')].some(x => x.textContent.includes('renewal risk')))",
        Duration::from_secs(3),
    )
    .await;
    let shown = b
        .text_of(
            "JSON.stringify([...document.querySelectorAll('.palette-item')].map(x=>x.textContent))",
        )
        .await
        .unwrap();
    assert!(
        shown.contains("renewal risk") && shown.contains("Said"),
        "a message match never reached the palette: {shown}"
    );
}

/// A message that joins a running turn is not echoed locally: it is queued
/// until the next step boundary, and the agent announces it as `Redirected`
/// when it actually arrives. Echoing it here as well would show it twice,
/// once at a point where it had not happened yet.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_joining_a_turn_is_not_echoed_before_it_arrives() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A prompt that never resolves, so the second one joins a running turn.
    b.text_of(
        r#"(() => { window.__replies.prompt = () => new Promise(() => {});
             const i = document.getElementById('input');
             i.value = 'run the demo';
             document.getElementById('composer').dispatchEvent(new Event('submit',{cancelable:true}));
             return 'ok'; })()"#,
    )
    .await
    .expect("first prompt");
    settle().await;

    b.text_of(
        r#"(() => { const i = document.getElementById('input');
             i.value = 'actually check the invoices too';
             document.getElementById('composer').dispatchEvent(new Event('submit',{cancelable:true}));
             return 'ok'; })()"#,
    )
    .await
    .expect("second prompt");
    settle().await;

    let log = b
        .text_of("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert!(
        log.contains("run the demo"),
        "the prompt that started the turn should be echoed: {log}"
    );
    assert!(
        !log.contains("invoices"),
        "a joining message was shown before it reached the turn: {log}"
    );
    assert_eq!(
        b.text_of("document.getElementById('status').textContent")
            .await
            .unwrap(),
        "redirecting…",
        "the person should be told the message is on its way"
    );

    // When the agent announces it, it appears exactly once.
    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"user", text:"actually check the invoices too"}); 'ok'"#,
    )
    .await
    .expect("the agent announces it");
    settle().await;
    let count = b
        .text_of("String([...document.querySelectorAll('#log .msg')].filter(e=>e.textContent.includes('invoices')).length)")
        .await
        .unwrap();
    assert_eq!(count, "1", "the joining message was rendered {count} times");
}

/// A conversation that failed to open must not leave its frame on screen.
///
/// Both open paths clear the transcript and the session id before the call.
/// If the call then fails and the previous Bot's name stays in the header
/// over an empty log with the composer live and `session` null, Send does
/// nothing at all, silently. Either show a conversation or show none.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_that_fails_to_open_leaves_nothing_behind() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A second Bot in the roster, whose open will fail.
    b.text_of(
        r#"(() => {
             window.__replies.roster = [
               { id: "talent-scout", name: "Talent Scout", title: "", description: "",
                 hidden: false, messages: 0 },
               { id: "gone", name: "Gone", title: "", description: "", hidden: false,
                 messages: 0 }];
             window.__throw = { open_bot: "no such Bot" };
             document.getElementById('toggle-hidden').click();
             return "ok"; })()"#,
    )
    .await
    .expect("stage a failing Bot");
    settle().await;

    b.text_of("[...document.querySelectorAll('#bots .bot')].find(x=>x.textContent.includes('Gone')).click(); 'ok'")
        .await
        .expect("open the failing one");
    settle().await;

    let state = b
        .text_of(
            r#"JSON.stringify({
                 header: document.getElementById('bot-name').textContent,
                 composer: !document.getElementById('composer').classList.contains('hidden'),
                 noBot: !document.getElementById('no-bot').classList.contains('hidden'),
                 status: document.getElementById('status').textContent,
                 problem: document.getElementById('problem-raw').textContent })"#,
        )
        .await
        .unwrap();
    assert!(
        state.contains(r#""header":"""#),
        "the header still names a conversation that is not open: {state}"
    );
    assert!(
        state.contains(r#""composer":false"#),
        "a live composer over no conversation swallows what is typed into it: {state}"
    );
    assert!(
        state.contains(r#""noBot":true"#),
        "the window should say to choose a Bot: {state}"
    );
    // The reason moved out of the status pill and into the problem banner,
    // deliberately: the pill is two words of chrome, and a whole sentence in it
    // was unreadable past its width and impossible to copy. Both halves are
    // asserted, because "on screen" is not the same claim as "readable" — the
    // pill has to say something happened, and the record has to be reachable.
    assert!(
        state.contains("could not open that Bot"),
        "the pill should say what failed: {state}"
    );
    assert!(
        state.contains("no such Bot"),
        "the reason should be reachable in the problem banner: {state}"
    );
}

/// Type `@` and the teammates on screen are offered by name.
///
/// The docs' `@` mention. The list is built from the sidebar's own DOM rather
/// than a second fetch, so it can never offer a teammate the sidebar does not
/// show: one list, one source, the same rule the palette follows.
#[tokio::test(flavor = "multi_thread")]
async fn typing_an_at_offers_the_teammates_the_sidebar_shows() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"(() => { const i = document.getElementById('input');
             i.value = 'ask @'; i.setSelectionRange(5, 5);
             i.dispatchEvent(new Event('input'));
             return 'ok'; })()"#,
    )
    .await
    .expect("type an @");
    settle().await;

    assert_eq!(
        b.text_of("String(document.getElementById('mentions').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "`@` did not open the menu"
    );
    let names = b
        .text_of(
            "[...document.querySelectorAll('.mentions-item span:first-child')]\
             .map(e => e.textContent).join(',')",
        )
        .await
        .unwrap();
    assert!(
        names.contains("Talent Scout"),
        "the Bot in the sidebar was not offered: {names}"
    );

    // Enter chooses rather than sending. A half-written message sent because
    // the composer took the key is not recoverable.
    let before = b
        .text_of("String(window.__sent('prompt').length)")
        .await
        .unwrap();
    b.text_of(
        r#"document.getElementById('input')
             .dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true})); 'ok'"#,
    )
    .await
    .expect("choose");
    settle().await;

    assert_eq!(
        b.text_of("document.getElementById('input').value")
            .await
            .unwrap(),
        "ask @talent-scout ",
        "the display name was inserted where the id belongs"
    );
    // The shape the resolver can read. `openbot_bots::mentions` takes
    // characters while they are `[A-Za-z0-9_-]`, so anything with a space in
    // it arrives truncated: `@Talent Scout` reaches `owner_for` as `talent`,
    // matches no member, and the turn is refused for naming somebody who is
    // not in the group. Pinned as a pattern rather than a literal so it keeps
    // meaning something if the fixture Bot is renamed.
    assert_eq!(
        b.text_of("String(/@[A-Za-z0-9_-]+ $/.test(document.getElementById('input').value))")
            .await
            .unwrap(),
        "true",
        "what was inserted cannot survive `openbot_bots::mentions`"
    );
    assert_eq!(
        b.text_of("String(window.__sent('prompt').length)")
            .await
            .unwrap(),
        before,
        "Enter sent the half-written message instead of choosing a name"
    );
}

/// `/` offers saved skills, and says when one could not be loaded.
///
/// A skill that stopped parsing is on disk, was reported created, and is being
/// ignored by every Bot. The menu is where somebody goes looking for it, so a
/// menu that showed only the working ones would be the thing hiding it.
#[tokio::test(flavor = "multi_thread")]
async fn a_slash_offers_skills_and_admits_the_ones_that_failed() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        r#"(() => { window.__replies.skills = { skills: [
             { name: "refund-a-customer", description: "How to issue a refund" }],
             problems: [{ path: "/h/skills/half/SKILL.md", why: "no frontmatter" }] };
           return 'ok'; })()"#,
    )
    .await
    .expect("stub the catalog");
    open_session(&b, "s1").await;

    b.text_of(
        r#"(() => { const i = document.getElementById('input');
             i.focus(); i.value = '/'; i.setSelectionRange(1, 1);
             i.dispatchEvent(new Event('input'));
             return 'ok'; })()"#,
    )
    .await
    .expect("type a slash");
    settle().await;

    let names = b
        .text_of(
            "[...document.querySelectorAll('.mentions-item span:first-child')]\
             .map(e => e.textContent).join(',')",
        )
        .await
        .unwrap();
    assert!(
        names.contains("refund-a-customer"),
        "the skill was not offered: {names}"
    );

    let note = b
        .text_of("document.getElementById('mentions-note').textContent")
        .await
        .unwrap();
    assert!(
        note.contains("1 skill") && note.contains("could not be loaded"),
        "a skill that no Bot can use was not admitted to: {note}"
    );
}

/// A `/` inside a word is arithmetic, not a menu.
///
/// `3/4` and `a@b` are somebody typing, and a list that opens over them is
/// worse than no list: it eats the next Enter.
#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_in_the_middle_of_a_word_is_left_alone() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        r#"(() => { window.__replies.skills = { skills: [
             { name: "refund-a-customer", description: "How to issue a refund" }],
             problems: [] };
           return 'ok'; })()"#,
    )
    .await
    .expect("stub the catalog");
    open_session(&b, "s1").await;

    // The text after the trigger character matches something offerable. A
    // case like `3/4` proves nothing: the menu would stay shut because "4"
    // names no skill, whether or not the word-boundary rule exists. Without
    // the rule, both of these open a menu.
    for typed in ["split 3/refund", "mail a@talent"] {
        b.text_of(&format!(
            r#"(() => {{ const i = document.getElementById('input');
                 i.value = {typed:?}; i.setSelectionRange({}, {});
                 i.dispatchEvent(new Event('input'));
                 return 'ok'; }})()"#,
            typed.len(),
            typed.len()
        ))
        .await
        .expect("type");
        settle().await;
        assert_eq!(
            b.text_of("String(document.getElementById('mentions').classList.contains('hidden'))")
                .await
                .unwrap(),
            "true",
            "a menu opened over `{typed}`"
        );
    }
}

/// Editing a Bot sends only the fields that changed.
///
/// The command treats an absent field as unchanged, which only helps if the
/// page uses it. A form that posted all three would write back whatever was
/// on screen when it opened, so editing a title would overwrite a description
/// somebody changed in the meantime, and the person who lost it would have no
/// way to know.
#[tokio::test(flavor = "multi_thread")]
async fn editing_a_bot_sends_only_what_changed() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    // After `open_session`, which installs a roster stub of its own: a Bot
    // with no title and no description, which is exactly what this test must
    // not be looking at.
    b.text_of(
        r#"(() => { window.__replies.roster = [{ id: "talent-scout",
             name: "Talent Scout", title: "recruiting",
             description: "finds people", hidden: false, messages: 0 }];
           return 'ok'; })()"#,
    )
    .await
    .expect("stub the roster");

    b.text_of("document.getElementById('edit-bot').click(); 'ok'")
        .await
        .expect("open the editor");
    settle().await;

    // The form opens on what the Bot actually is, description included: it is
    // nowhere else on screen, and an empty box would read as "it has none".
    assert_eq!(
        b.text_of("document.getElementById('edit-description').value")
            .await
            .unwrap(),
        "finds people",
        "the description was not loaded, so saving would blank it"
    );

    b.text_of(
        r#"(() => { document.getElementById('edit-title').value = 'hiring';
             document.getElementById('edit-form')
               .dispatchEvent(new Event('submit', {cancelable: true}));
             return 'ok'; })()"#,
    )
    .await
    .expect("save");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('bot_describe').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("\"title\":\"hiring\""),
        "the edited field was not sent: {sent}"
    );
    assert!(
        !sent.contains("description"),
        "a field nobody touched was sent, which is how an edit overwrites one: {sent}"
    );
    assert!(
        !sent.contains("rename"),
        "an unchanged name was sent as a rename: {sent}"
    );
}

/// A group has no profile to edit, and the button says so rather than opening
/// three boxes that go nowhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_has_no_edit_button() {
    let Some((b, _p)) = page().await else { return };
    let launch = a_group(
        "launch",
        "Launch",
        &[("writer", "Writer"), ("researcher", "Researcher")],
    );
    b.text_of(
        &r#"(() => { window.__replies.groups = [__GROUP__];
           window.__replies.open_group = { session: "g1", name: "Launch", history: [] };
           return 'ok'; })()"#
            .replace("__GROUP__", &stub(&launch)),
    )
    .await
    .expect("stub a group");
    open_session(&b, "s1").await;

    // A Bot is open, so the button is there.
    assert_eq!(
        b.text_of("String(!document.getElementById('edit-bot').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "a Bot conversation should offer Edit"
    );

    b.text_of("document.querySelector('#groups .bot').click(); 'ok'")
        .await
        .expect("open the group");
    settle().await;

    assert_eq!(
        b.text_of("String(document.getElementById('edit-bot').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the group conversation kept an Edit button with nothing behind it"
    );
}

/// Duplicating from the window names the copy and opens it.
///
/// A copy left in the sidebar for somebody to go and find is a step nobody
/// wants: duplicating is how you start work as the copy.
#[tokio::test(flavor = "multi_thread")]
async fn duplicating_a_bot_names_the_copy_and_opens_it() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of("document.getElementById('edit-bot').click(); 'ok'")
        .await
        .expect("open the editor");
    settle().await;

    // No name is a refusal, not a copy called "".
    b.text_of(
        r#"(() => { document.getElementById('dup-form')
             .dispatchEvent(new Event('submit', {cancelable: true}));
           return 'ok'; })()"#,
    )
    .await
    .expect("submit empty");
    settle().await;
    assert_eq!(
        b.text_of("String(window.__sent('bot_duplicate').length)")
            .await
            .unwrap(),
        "0",
        "an unnamed copy was sent to the shell"
    );
    assert!(
        !b.text_of("document.getElementById('dup-error').textContent")
            .await
            .unwrap()
            .is_empty(),
        "nothing on screen said why the copy was refused"
    );

    let opened_before = b
        .text_of("String(window.__sent('open_bot').length)")
        .await
        .unwrap();
    b.text_of(
        r#"(() => { document.getElementById('dup-name').value = 'Talent Scout EMEA';
             document.getElementById('dup-form')
               .dispatchEvent(new Event('submit', {cancelable: true}));
           return 'ok'; })()"#,
    )
    .await
    .expect("duplicate");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('bot_duplicate').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("Talent Scout EMEA") && sent.contains("Talent Scout\""),
        "the copy was not asked for by name, from the open Bot: {sent}"
    );

    let opened_after = b
        .text_of("String(window.__sent('open_bot').length)")
        .await
        .unwrap();
    assert_ne!(
        opened_before, opened_after,
        "the copy was made and left for somebody to find"
    );
}

/// The confirm names what is destroyed.
///
/// "Are you sure?" asks somebody to reaffirm a decision without telling them
/// anything they did not already know. The surprise is never the Bot; it is
/// the routine that ran every morning, and the group about to lose its
/// coordinator. Neither is on screen anywhere else.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_bot_says_what_goes_with_it_before_it_goes() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        &r#"(() => { window.__replies.roster = [{ id: "talent-scout",
             name: "Talent Scout", title: "", description: "",
             hidden: false, messages: 47 }];
           window.__replies.routines = [{ bot: "talent-scout", bot_name: "Talent Scout",
             id: "morning", trigger: "every day at 9:00", next: null, enabled: true }];
           window.__replies.groups = [__GROUP__];
           return 'ok'; })()"#
            .replace(
                "__GROUP__",
                &stub(&a_group(
                    "launch",
                    "Launch",
                    &[("talent-scout", "Talent Scout"), ("writer", "Writer")],
                )),
            ),
    )
    .await
    .expect("stub what it would cost");

    b.text_of("document.getElementById('edit-bot').click(); 'ok'")
        .await
        .expect("open the editor");
    settle().await;

    // Arming takes a click: the dialog must not open with the confirm up.
    assert_eq!(
        b.text_of("String(document.getElementById('del-confirm').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the editor opened with Delete already armed"
    );

    b.text_of("document.getElementById('del-start').click(); 'ok'")
        .await
        .expect("arm it");
    settle().await;

    let what = b
        .text_of("document.getElementById('del-what').textContent")
        .await
        .unwrap();
    assert!(what.contains("47 message"), "the conversation: {what}");
    assert!(
        what.contains("routine"),
        "the routine is the thing nobody remembers is attached: {what}"
    );
    assert!(
        what.contains("Launch"),
        "the group it coordinates is not mentioned: {what}"
    );
    assert!(
        what.contains("cannot be undone"),
        "the confirm does not say it is irreversible: {what}"
    );

    // Nothing has been destroyed by arming it.
    assert_eq!(
        b.text_of("String(window.__sent('bot_delete').length)")
            .await
            .unwrap(),
        "0",
        "arming the confirm deleted the Bot"
    );

    // "Keep it" puts it back, rather than leaving a live Delete button.
    b.text_of("document.getElementById('del-cancel').click(); 'ok'")
        .await
        .expect("keep it");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('del-confirm').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "declining left the confirm on screen"
    );

    b.text_of(
        r#"(() => { document.getElementById('del-start').click();
             return 'ok'; })()"#,
    )
    .await
    .expect("arm again");
    settle().await;
    b.text_of("document.getElementById('del-go').click(); 'ok'")
        .await
        .expect("delete");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('bot_delete').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("talent-scout"),
        "deleted by a name rather than the id that identifies it: {sent}"
    );

    // The conversation belonged to a Bot that is gone. Left open it is a
    // header, a transcript and a live composer over nothing.
    assert_eq!(
        b.text_of("String(document.getElementById('composer').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the composer stayed live over a deleted Bot"
    );
}

/// A decision that did not reach the Bot must not close the question.
///
/// If the dialog were taken down first and the shell called second, with a
/// failure reported only as a status pill in the corner, an answer that went
/// nowhere (the request had already settled, by the engine's timeout or by
/// the turn ending) would look exactly like one that worked: the question
/// vanishes, the person believes they allowed it, and nothing happened. This
/// is the surface whose entire job is deciding whether something runs, so
/// the shell is called first and the dialog closes only on success.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_never_arrived_leaves_the_question_up() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(&format!(
        r#"(() => {{ window.__throw = {{ answer_permission:
             "this was already settled without your answer — the Bot did not receive it" }};
           window.__fire('permission-request', {});
           return 'ok'; }})()"#,
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an approval");
    settle().await;

    b.text_of("document.querySelector('#dialog-options button').click(); 'ok'")
        .await
        .expect("allow it");
    settle().await;

    assert_eq!(
        b.text_of("String(!document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the question closed on a decision that never landed"
    );
    let said = b
        .text_of("document.getElementById('dialog-error').textContent")
        .await
        .unwrap();
    assert!(
        said.contains("did not receive it"),
        "the dialog does not say what happened: {said}"
    );

    // The only control left acknowledges it. Offering the original buttons
    // again would invite a second click that cannot work either.
    let buttons = b
        .text_of("[...document.querySelectorAll('#dialog-options button')].map(e=>e.textContent).join('|')")
        .await
        .unwrap();
    assert_eq!(buttons, "Dismiss", "{buttons}");

    b.text_of("document.querySelector('#dialog-options button').click(); 'ok'")
        .await
        .expect("dismiss");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "dismissing left the question on screen"
    );
}

/// A turn that ends takes its unanswered questions with it.
///
/// Nothing is waiting on them once the agent has stopped, so a dialog left up
/// is asking about a call that will never be made, and answering it does
/// nothing at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_ending_takes_down_the_questions_it_left() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an approval");
    settle().await;
    assert_eq!(
        b.text_of("String(!document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the approval never appeared"
    );

    b.text_of("window.__fire('permission-withdrawn', ['a1']); 'ok'")
        .await
        .expect("withdraw");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "a question outlived the turn it belonged to"
    );
}

/// A credential typed into the window must not survive in it.
///
/// This is the longest a secret exists anywhere in this product: in a DOM
/// input, in a process a person leaves open for hours. The shell's side is
/// covered by `shell_live.rs`, which checks the value never comes back out of
/// `secret_list`; this covers the half where it is typed.
///
/// Three properties, and the failure that motivates each: the box is cleared
/// when the store fails (clearing only on success leaves a live secret behind
/// the most likely path to leave it there), it is cleared when the dialog is
/// closed and reopened, and no value ever reaches the list, which is rendered
/// from whatever the shell hands back.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_is_never_left_in_the_window() {
    const VALUE: &str = "sk-live-NEVER-SHOW-THIS-4f2a9c";
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // The shell refuses it (a name already taken, say). The box must still be
    // empty afterwards.
    b.text_of(
        r#"(() => { window.__throw = { secret_set: "that name is already in use" };
           document.getElementById('credentials').click();
           return 'ok'; })()"#,
    )
    .await
    .expect("open credentials");
    settle().await;

    b.text_of(&format!(
        r#"(() => {{ document.getElementById('secret-name').value = 'stripe-token';
             document.getElementById('secret-value').value = {VALUE:?};
             document.getElementById('secret-form')
               .dispatchEvent(new Event('submit', {{cancelable: true}}));
             return 'ok'; }})()"#
    ))
    .await
    .expect("store it");
    settle().await;

    assert_eq!(
        b.text_of("document.getElementById('secret-value').value")
            .await
            .unwrap(),
        "",
        "a store that failed left the credential in the box"
    );
    let said = b
        .text_of("document.getElementById('secret-error').textContent")
        .await
        .unwrap();
    assert!(
        !said.contains(VALUE),
        "the failure put the credential on screen: {said}"
    );

    // Now a store that works, with the shell answering as it really does:
    // names and fingerprints, never values.
    b.text_of(
        r#"(() => { window.__throw = {};
           window.__replies.secret_list = [{ name: "stripe-token",
             fingerprint: "sha256:4f2a…9c" }];
           document.getElementById('secret-value').value = 'another-value';
           document.getElementById('secret-form')
             .dispatchEvent(new Event('submit', {cancelable: true}));
           return 'ok'; })()"#,
    )
    .await
    .expect("store it");
    settle().await;

    assert_eq!(
        b.text_of("document.getElementById('secret-value').value")
            .await
            .unwrap(),
        "",
        "a successful store left the credential in the box"
    );

    // What the list shows, in full. A fingerprint is the only thing there is.
    let shown = b
        .text_of("document.getElementById('secrets-list').textContent")
        .await
        .unwrap();
    assert!(shown.contains("stripe-token"), "{shown}");
    assert!(
        shown.contains("sha256"),
        "the fingerprint is missing: {shown}"
    );

    // Nothing anywhere in the document holds either value. The whole page,
    // not just the field: a value copied into a title, a data attribute or a
    // hidden element is still in the window.
    let anywhere = b
        .text_of(
            "document.documentElement.outerHTML.includes('NEVER-SHOW-THIS') ? 'leaked' : 'clean'",
        )
        .await
        .unwrap();
    assert_eq!(
        anywhere, "clean",
        "the credential is still somewhere in the page"
    );
}

/// Hiding a Bot says what it does not do.
///
/// `openbot bot hide` lists what still runs, because SPEC §8 calls this a
/// genuine footgun: the Bot leaves the sidebar and goes on working, and
/// spending, out of sight. A window offering the same button without the same
/// sentence would be the same act with the safeguard taken off.
#[tokio::test]
async fn hiding_a_bot_warns_that_its_work_keeps_running() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        r#"(() => { window.__replies.roster = [{ id: "talent-scout",
             name: "Talent Scout", title: "", description: "",
             hidden: false, messages: 3 }];
           window.__replies.routines = [
             { bot: "talent-scout", bot_name: "Talent Scout", id: "morning",
               trigger: "every day at 9:00", next: null, enabled: true },
             { bot: "talent-scout", bot_name: "Talent Scout", id: "paused-one",
               trigger: "every hour", next: null, enabled: false },
             { bot: "someone-else", bot_name: "Other", id: "nightly",
               trigger: "every night", next: null, enabled: true }];
           return 'ok'; })()"#,
    )
    .await
    .expect("stub the roster and routines");

    b.text_of("document.getElementById('edit-bot').click(); 'ok'")
        .await
        .expect("open the editor");
    settle().await;

    let said = b
        .text_of("document.getElementById('hide-what').textContent")
        .await
        .unwrap();
    assert!(
        said.contains("morning"),
        "the routine that keeps running is not named: {said}"
    );
    assert!(
        said.contains("does not pause"),
        "the sentence that matters is missing: {said}"
    );
    assert!(
        !said.contains("paused-one"),
        "a paused routine was listed as still running: {said}"
    );
    assert!(
        !said.contains("nightly"),
        "another Bot's routine was attributed to this one: {said}"
    );

    // Hiding sends the id, then takes the conversation off screen: a Bot
    // hidden while open would be a transcript for something the sidebar no
    // longer lists.
    b.text_of("document.getElementById('hide-bot').click(); 'ok'")
        .await
        .expect("hide it");
    settle().await;
    let sent = b
        .text_of("JSON.stringify(window.__sent('bot_hide').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("talent-scout") && sent.contains("true"),
        "hiding did not reach the shell: {sent}"
    );
    assert_eq!(
        b.text_of("String(document.getElementById('composer').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "the conversation stayed open for a Bot no longer in the sidebar"
    );
}

/// The docs' `Cmd/Ctrl+N`, and only where it means something.
#[tokio::test]
async fn control_n_makes_a_bot_only_once_there_is_somewhere_to_put_it() {
    let Some((b, _p)) = page().await else { return };

    // Before connecting there is no workspace, so the key must be left alone
    // rather than opening a dialog over the connect panel.
    b.text_of(
        r#"document.dispatchEvent(new KeyboardEvent('keydown',{key:'n',ctrlKey:true,bubbles:true}));
           'ok'"#,
    )
    .await
    .expect("press it early");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('name-dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "Ctrl+N opened the naming dialog with nowhere to put a Bot"
    );

    open_session(&b, "s1").await;
    b.text_of(
        r#"document.dispatchEvent(new KeyboardEvent('keydown',{key:'n',ctrlKey:true,bubbles:true}));
           'ok'"#,
    )
    .await
    .expect("press it");
    settle().await;
    assert_eq!(
        b.text_of("String(!document.getElementById('name-dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "Ctrl+N did not open the naming dialog"
    );
}

/// A routine can be paused from the window, and the button says which way.
///
/// A routine is the run nobody is watching; when one starts failing every
/// night, the alternatives are deleting it (losing what it was) or reaching
/// for a terminal. It is also the other half of the hide dialog's warning:
/// naming a routine that keeps running is of little use if there is no way
/// here to stop it.
///
/// Pausing keeps the definition and the history, so it needs no confirmation.
/// The button has to read as the act it performs, or the one control that
/// reverses a runaway is the one nobody is sure about pressing.
#[tokio::test]
async fn a_routine_can_be_paused_and_resumed_from_the_window() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        r#"(() => { window.__replies.routines = [
             { bot: "talent-scout", bot_name: "Talent Scout", id: "morning",
               trigger: "every day at 9:00", next: null, enabled: true },
             { bot: "ledger", bot_name: "Ledger", id: "nightly",
               trigger: "every night", next: null, enabled: false }];
           document.getElementById('rules-btn').click();
           return 'ok'; })()"#,
    )
    .await
    .expect("open settings");
    settle().await;

    let labels = b
        .text_of("[...document.querySelectorAll('#routines-list button')].map(e=>e.textContent).join('|')")
        .await
        .unwrap();
    assert_eq!(
        labels, "Pause|Resume",
        "the control must name the act it performs, per row"
    );

    // The running one pauses.
    b.text_of("document.querySelectorAll('#routines-list button')[0].click(); 'ok'")
        .await
        .expect("pause");
    settle().await;
    let sent = b
        .text_of("JSON.stringify(window.__sent('routine_pause').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("\"paused\":true") && sent.contains("morning"),
        "pausing did not reach the shell with the right routine: {sent}"
    );
    assert!(
        sent.contains("talent-scout"),
        "the routine was addressed by display name rather than by the id the \
         binary takes: {sent}"
    );

    // The paused one resumes: the same control, the other direction.
    b.text_of("document.querySelectorAll('#routines-list button')[1].click(); 'ok'")
        .await
        .expect("resume");
    settle().await;
    let sent = b
        .text_of("JSON.stringify(window.__sent('routine_pause').map(c => c.args))")
        .await
        .unwrap();
    assert!(
        sent.contains("\"paused\":false") && sent.contains("nightly"),
        "resuming did not reach the shell: {sent}"
    );
}

/// An ask that is a credential request, not an approval.
fn secret_ask(id: &str, session: &str) -> String {
    format!(
        r#"{{"id":"{id}","session":"{session}","tool":"credential needed: linear-token",
            "fields":[{{"name":"why","value":"to file the issue","long":false}}],
            "options":[{{"id":"provide-secret","name":"Provide credential","kind":"allow_once",
                         "danger":false}},
                       {{"id":"reject-once","name":"Not this time","kind":"reject_once",
                         "danger":true}}],
            "secret":{{"name":"linear-token","why":"to file the issue"}}}}"#
    )
}

/// A credential request is a box to type in, not a button to click.
///
/// Without a handler for this, a `secret.request` times out and is refused:
/// safe, and useless, because the Bot's only remaining move is to ask for
/// the token in conversation, which puts it in the model's context and in
/// the log on disk. That is the exact failure the broker exists to prevent.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_request_asks_for_a_value_and_masks_it() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        secret_ask("s-1", "s1")
    ))
    .await
    .expect("an ask");
    settle().await;

    let shown = b
        .text_of(
            "JSON.stringify({\
               heading: document.getElementById('dialog-heading').textContent,\
               hidden: document.getElementById('dialog-secret').classList.contains('hidden'),\
               label: document.getElementById('dialog-secret-label').textContent,\
               type: document.getElementById('dialog-secret-value').type,\
               note: document.querySelector('.dialog-secret-note').textContent.trim(),\
               buttons: [...document.querySelectorAll('#dialog-options button')]\
                 .map(x=>[x.textContent,x.className]),\
             })",
        )
        .await
        .unwrap();

    assert!(shown.contains("A credential is needed"), "{shown}");
    assert!(
        shown.contains(r#""hidden":false"#),
        "the input is not shown: {shown}"
    );
    assert!(
        shown.contains("linear-token"),
        "it does not say what for: {shown}"
    );
    // Masked. The terminal prompt cannot do this and says so; the window can,
    // and a window that echoed a credential in plain text would be worse than
    // the terminal it replaces.
    assert!(
        shown.contains(r#""type":"password""#),
        "the credential is echoed: {shown}"
    );
    assert!(
        shown.contains("never shown the value"),
        "it does not say the Bot cannot read it: {shown}"
    );
    // Two answers, and only two. "Allow always" has no meaning for a
    // credential, and offering it invites a click that supplies nothing.
    assert!(
        shown.contains(r#"["Store and continue","primary"]"#),
        "{shown}"
    );
    assert!(shown.contains(r#"["Not this time","danger"]"#), "{shown}");
    assert!(
        !shown.contains("Allow once"),
        "an approval button on a credential prompt: {shown}"
    );
}

/// Say the credential's name once, and set the reason as prose.
///
/// If the generic approval rendering ran alongside the purpose-built one, the
/// card would name the credential three times over (the tool line, the
/// argument table, and the input's label) and set the Bot's one-sentence
/// reason in monospace beside `name`, where a sentence reads as data rather
/// than as something somebody is telling you.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_prompt_says_the_name_once_and_the_reason_in_prose() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        secret_ask("s-1", "s1")
    ))
    .await
    .expect("an ask");
    settle().await;

    let card = b
        .text_of(concat!(
            "JSON.stringify({",
            "names:(document.querySelector('#dialog .dialog-card').innerText",
            ".match(/linear-token/g)||[]).length,",
            "tool:document.getElementById('dialog-tool').classList.contains('hidden'),",
            "fields:document.getElementById('dialog-fields').classList.contains('hidden'),",
            "why:document.getElementById('dialog-secret-why').textContent})",
        ))
        .await
        .unwrap();

    assert!(
        card.contains(r#""names":1"#),
        "the credential is named more than once on one card: {card}"
    );
    // The generic approval rendering is where the repetition would come from,
    // so both halves of it stand down for a question that has its own.
    assert!(card.contains(r#""tool":true"#), "{card}");
    assert!(card.contains(r#""fields":true"#), "{card}");
    // The reason still reaches the person: hiding the table must not hide it.
    assert!(
        card.contains("to file the issue"),
        "the reason vanished with the argument table: {card}"
    );
}

/// The value goes to `supply_secret`, and nowhere near `answer_permission`.
///
/// Two different acts: an approval is a choice among offered options, this is
/// a value. Sending a credential as an option id would put it in the tool call
/// the transcript renders.
#[tokio::test(flavor = "multi_thread")]
async fn storing_a_credential_sends_it_as_a_value_not_as_an_option() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        secret_ask("s-1", "s1")
    ))
    .await
    .expect("an ask");
    settle().await;

    let sent = b
        .text_of(
            "(() => { const i = document.getElementById('dialog-secret-value');\
               i.value = 'sk-live-abc';\
               [...document.querySelectorAll('#dialog-options button')]\
                 .find(x=>x.textContent==='Store and continue').click();\
               return 'ok'; })()",
        )
        .await
        .expect("store");
    assert_eq!(sent, "ok");
    settle().await;

    let calls = b.text_of("JSON.stringify(window.__calls)").await.unwrap();
    assert!(
        calls.contains("supply_secret") && calls.contains("sk-live-abc"),
        "the credential did not reach supply_secret: {calls}"
    );
    // Never as an approval. `answer_permission` carries an option id into the
    // tool call, which is rendered in the conversation.
    let as_approval = b
        .text_of("JSON.stringify((window.__calls||[]).filter(c => c[0] === 'answer_permission'))")
        .await
        .unwrap();
    assert!(
        !as_approval.contains("sk-live-abc"),
        "the credential was sent as an approval option: {as_approval}"
    );

    // The box is emptied, so a credential is not left in the DOM.
    let left = b
        .text_of("document.getElementById('dialog-secret-value').value")
        .await
        .unwrap();
    assert_eq!(left, "", "the credential is still in the input");
}

/// Declining a credential is an ordinary refusal, and must not send a value.
#[tokio::test(flavor = "multi_thread")]
async fn declining_a_credential_sends_no_value() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        secret_ask("s-1", "s1")
    ))
    .await
    .expect("an ask");
    settle().await;

    b.text_of(
        "(() => { document.getElementById('dialog-secret-value').value = 'sk-live-abc';\
           [...document.querySelectorAll('#dialog-options button')]\
             .find(x=>x.textContent==='Not this time').click();\
           return 'ok'; })()",
    )
    .await
    .expect("decline");
    settle().await;

    let calls = b.text_of("JSON.stringify(window.__calls)").await.unwrap();
    assert!(
        !calls.contains("supply_secret"),
        "declining supplied the credential anyway: {calls}"
    );
    assert!(
        !calls.contains("sk-live-abc"),
        "a typed-then-declined credential was sent: {calls}"
    );
    assert!(
        calls.contains("answer_permission"),
        "nothing was refused: {calls}"
    );
}

/// The screen a new install opens on must not offer what it cannot do.
///
/// Connecting to a home with no Bots is the first thing every new person
/// sees, and it is a state rather than a transition: every other test here
/// opens a Bot. `closeConversation` hides Edit and `openBot` shows it, so the
/// invariant is easy to hold on every transition and still miss at the start,
/// where the button would sit live over a blank name and clicking it would do
/// nothing at all, silently.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_connection_offers_nothing_that_needs_a_conversation() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        "(() => { window.__replies.roster = [];\
           document.getElementById('connect-btn').click();\
           return 'ok'; })()",
    )
    .await
    .expect("connect");
    settle().await;

    let state = b
        .text_of(concat!(
            "JSON.stringify({",
            "workspace:!document.getElementById('workspace').classList.contains('hidden'),",
            "edit:!document.getElementById('edit-bot').classList.contains('hidden'),",
            "composer:!document.getElementById('composer').classList.contains('hidden'),",
            "log:!document.getElementById('log').classList.contains('hidden'),",
            "noBot:!document.getElementById('no-bot').classList.contains('hidden')})",
        ))
        .await
        .unwrap();

    assert!(
        state.contains(r#""workspace":true"#),
        "never connected: {state}"
    );
    assert!(
        state.contains(r#""edit":false"#),
        "Edit is offered with nothing to edit; clicking it does nothing: {state}"
    );
    // The rest of the conversation frame is the same claim; asserted together
    // so the invariant is stated once, whole.
    assert!(state.contains(r#""composer":false"#), "{state}");
    assert!(state.contains(r#""log":false"#), "{state}");
    assert!(state.contains(r#""noBot":true"#), "{state}");
}

/// The first screen says what this is, and shows what it is asking you to
/// confirm.
///
/// Three properties of the connect panel:
///
/// * It carries the product identity. The roster header has the mark and the
///   wordmark; without them, the screen every new install opens on is three
///   unexplained path fields.
/// * A long binary path stays reachable. A path wider than its box is cut
///   off at the end, which is the part that says which binary. The approval
///   card already holds this rule (every argument shown whole, wrapped rather
///   than truncated) because hiding part of the input defeats the purpose;
///   confirming a path before connecting is the same act.
/// * Both path fields have a picker. The binary is the field more likely to
///   be wrong: a home that does not exist is created, and a binary that does
///   not exist is the failure a new person cannot diagnose.
#[tokio::test(flavor = "multi_thread")]
async fn the_connect_screen_names_the_product_and_hides_nothing_it_asks_about() {
    let Some((b, _p)) = page().await else { return };

    let shown = b
        .text_of(concat!(
            "JSON.stringify({",
            "name:!!document.querySelector('#connect .connect-head h1'),",
            "mark:!!document.querySelector('#connect .connect-head .mark'),",
            "lede:(document.querySelector('#connect .connect-lede')||{}).textContent||'',",
            "pickers:[...document.querySelectorAll('#connect button[id^=pick-]')]",
            ".map(x=>x.id).sort()})",
        ))
        .await
        .unwrap();

    assert!(
        shown.contains(r#""name":true"#),
        "the product does not name itself: {shown}"
    );
    assert!(shown.contains(r#""mark":true"#), "{shown}");
    // The one sentence that says what a openbot is and where the defaults came
    // from. Three path fields assume a person already knows.
    assert!(
        shown.contains("openbot up"),
        "nothing says where the defaults come from: {shown}"
    );
    // Both paths, or neither. The binary is the one more likely to be wrong.
    assert!(
        shown.contains(r#"["pick-home","pick-openbot"]"#),
        "only one of the two path fields can be browsed: {shown}"
    );

    // A value too long for its box is still readable, because the tooltip
    // carries it, and it tracks typing, not just the picker, or it would go
    // stale the moment somebody edited the field.
    let long = "C:\\Users\\somebody\\a\\deliberately\\long\\path\\to\\openbot.exe";
    // The title alone, never an object that also carries the value. Returning
    // `{title, value}` and asserting the blob contains the path would be
    // satisfied by the value by itself, so deleting the tooltip would leave
    // the test green.
    let title = b
        .text_of(&format!(
            "(() => {{ const e = document.getElementById('openbot-path');\
               e.value = '{long}'; e.dispatchEvent(new Event('input'));\
               return e.title; }})()"
        ))
        .await
        .unwrap();
    assert!(
        title.contains("deliberately"),
        "the whole path is not reachable once it overflows: title was {title:?}"
    );
}

/// No field may hide its own placeholder.
///
/// A guard over every dialog at once rather than one assertion per field,
/// because the failure is a layout one and layout breaks in groups: a column
/// ratio changes, a dialog gets a new control, and the field that loses is
/// whichever had least slack. Typical failures: `rule-reason` rendering "why
/// (shown to the perso" because the add-rule form gives equal width to a glob
/// and a sentence, or a rebalanced form clipping the `*` off `shell.exec or
/// fs.*` instead.
///
/// Measured with the page's own font metrics, so it stays true if the type
/// changes rather than pinning pixel numbers that would need updating.
#[tokio::test(flavor = "multi_thread")]
async fn no_dialog_field_is_narrower_than_the_placeholder_in_it() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let clipped = b
        .text_of(concat!(
            "(() => { const c = document.createElement('canvas').getContext('2d');",
            "const bad = [];",
            "for (const d of document.querySelectorAll('.dialog')) {",
            "  const was = d.classList.contains('hidden');",
            "  d.classList.remove('hidden');",
            // Values, not just placeholders. A long binary path in the connect
            // panel is cut off at the end, the part naming which binary, and a
            // guard that only checked hints would miss it. A value present
            // beats an empty placeholder, because that is what a person is
            // reading.
            "  for (const e of d.querySelectorAll('input')) {",
            "    const t = e.value || e.placeholder || ''; if (!t) continue;",
            "    if (e.type === 'password') continue;",
            // Same exemption as the connect-panel scan below, and here for the
            // same reason: a checkbox's `value` is a submission token, not text
            // it draws. Kept in step deliberately — two copies of one scan with
            // different rules is how the two stop meaning the same thing.
            "    if (e.type === 'checkbox' || e.type === 'radio') continue;",
            "    c.font = getComputedStyle(e).font;",
            "    const need = Math.ceil(c.measureText(t).width);",
            "    const have = Math.round(e.clientWidth - 20);",
            "    if (need > have) bad.push(d.id + '#' + e.id + ' ' + need + '>' + have);",
            "  }",
            "  if (was) d.classList.add('hidden');",
            "}",
            "return JSON.stringify(bad); })()",
        ))
        .await
        .unwrap();

    assert_eq!(
        clipped, "[]",
        "a field is too narrow for the text written in it: {clipped}"
    );

    // The connect panel is outside `.dialog`. Checked by the same measurement
    // rather than by a second one that could drift from it.
    let panel = b
        .text_of(concat!(
            "(() => { const c = document.createElement('canvas').getContext('2d');",
            "const p = document.getElementById('connect');",
            "const was = p.classList.contains('hidden'); p.classList.remove('hidden');",
            // Create the condition, then check it. Without this the loop
            // measures whatever the stubs happened to fill in, nothing
            // overflows, and the assertion below can never fire. A guard for
            // long values has to supply a long value.
            "for (const e of p.querySelectorAll('input')) {",
            "  e.value = 'C:' + '\\a-directory-with-a-long-name'.repeat(8) + '\\openbot.exe';",
            "  e.dispatchEvent(new Event('input'));",
            "}",
            "const bad = [];",
            "for (const e of p.querySelectorAll('input')) {",
            // A password field is the one place the rule inverts. The tooltip
            // is what makes a long value readable, and a readable secret is
            // the failure, not the fix: it would sit in a hover for anybody
            // standing behind you. `a_credential_is_never_left_in_the_window`
            // holds the other half of this.
            "  if (e.type === 'password') continue;",
            // A checkbox's `value` is the token a form submission carries, not
            // text it draws — it is "on" whatever the label beside it says.
            // Measuring it asks whether a 13px control can display a word it
            // never displays. The label's text is ordinary flow content and is
            // covered by the overflow checks that apply to everything else.
            "  if (e.type === 'checkbox' || e.type === 'radio') continue;",
            "  const t = e.value || e.placeholder || ''; if (!t) continue;",
            "  c.font = getComputedStyle(e).font;",
            "  const need = Math.ceil(c.measureText(t).width);",
            "  const have = Math.round(e.clientWidth - 20);",
            // A path can always be longer than any box, so the rule for these
            // is not "it fits" but "it is reachable": the tooltip carries the
            // whole value when the box cannot.
            "  if (need > have && e.title !== e.value) bad.push(e.id + ' ' + need + '>' + have);",
            "}",
            "if (was) p.classList.add('hidden');",
            "return JSON.stringify(bad); })()",
        ))
        .await
        .unwrap();
    assert_eq!(
        panel, "[]",
        "a connect field hides part of a value with no way to read it: {panel}"
    );
}

/// A heading belongs with the thing it names.
///
/// The rules list and the add-rule form share one heading and sit together.
/// A layout that listed the rules unlabelled under the panel title, then
/// Connected apps, then Runs on a schedule, and only then a Rules heading
/// introducing the form would put the label four sections after the content
/// it names.
#[tokio::test(flavor = "multi_thread")]
async fn settings_keeps_each_section_with_its_own_heading() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of("document.getElementById('rules-btn').click(); 'ok'")
        .await
        .expect("settings");
    settle().await;

    let order = b
        .text_of(concat!(
            "JSON.stringify([...document.querySelectorAll(",
            "'#rules-dialog h2, #rules-dialog #rules-list, #rules-dialog #rule-form,",
            " #rules-dialog #connectors-list, #rules-dialog #routines-list')]",
            ".map(e => e.id || e.textContent.trim()))",
        ))
        .await
        .unwrap();

    // The rules list and the form that adds to it sit together, under the one
    // heading that names them, and before the next section starts.
    let rules = order.find("Rules").expect("no Rules heading");
    let list = order.find("rules-list").expect("no rules list");
    let form = order.find("rule-form").expect("no add-rule form");
    let apps = order.find("Connected apps").expect("no apps section");
    assert!(
        rules < list && list < form && form < apps,
        "the rules section is not contiguous: {order}"
    );
}

/// A connector is listed with the credential it needs.
///
/// `openbot_desktop::settings` has a unit test that parses `secrets` out of
/// `openbot connector ls --json`, and parsing a field is not rendering it;
/// this pins the rendered row (`linear · linear-token`).
///
/// The credential's name, never its value. A connector list that showed the
/// token would undo the store it is drawn from.
#[tokio::test(flavor = "multi_thread")]
async fn a_connector_is_listed_with_the_credential_it_needs() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        r#"(() => { window.__replies.connectors = [
             { id: "linear", url: "https://mcp.linear.app/mcp",
               secrets: ["linear-token"] },
             { id: "offline", url: "https://example.invalid/mcp", secrets: [] }];
           document.getElementById('rules-btn').click();
           return 'ok'; })()"#,
    )
    .await
    .expect("open settings");
    settle().await;

    let shown = b
        .text_of("document.getElementById('connectors-list').innerText")
        .await
        .unwrap();
    assert!(
        shown.contains("linear"),
        "the connector is not listed: {shown}"
    );
    assert!(
        shown.contains("linear-token"),
        "the credential it needs is not named, so nothing says what to store: {shown}"
    );
    // One with none says so rather than showing an empty gap, or a person
    // cannot tell "needs nothing" from "we did not look".
    assert!(
        shown.contains("no credential"),
        "a connector needing nothing is indistinguishable from an unread one: {shown}"
    );
}

/// An attachment belongs to the message, not to the window.
///
/// Three claims, each one a way the chips can lie about what is going to be
/// sent: the file's own name is shown rather than the path it landed at, the
/// paths travel with the prompt, and opening a different conversation drops
/// them. That last one is the "one conversation eating another's" the inbox
/// exists to prevent, on a much shorter path: pick a file, change your mind,
/// open another Bot, and it would have gone to that one.
#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_named_by_file_sent_by_path_and_dropped_on_leaving() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        "(() => { window.__replies.attach_file =\
           { name: 'notes.md', path: 'attachments/notes-2.md' };\
           document.getElementById('attach').click(); return 'ok'; })()",
    )
    .await
    .expect("attach");
    settle().await;

    let chip = b
        .text_of(concat!(
            "JSON.stringify({",
            "shown:document.querySelector('#attached li span').textContent,",
            "title:document.querySelector('#attached li').title,",
            "visible:!document.getElementById('attached').classList.contains('hidden')})",
        ))
        .await
        .unwrap();
    // The name somebody picked. `attachments/notes-2.md` is what the Bot is
    // told and is not a name they would recognise.
    assert!(chip.contains(r#""shown":"notes.md""#), "{chip}");
    assert!(
        chip.contains("attachments/notes-2.md"),
        "the path is unreachable: {chip}"
    );
    assert!(chip.contains(r#""visible":true"#), "{chip}");

    // Sending carries the path, not the name.
    b.text_of(
        "(() => { const i=document.getElementById('input'); i.value='look at this';\
           i.dispatchEvent(new Event('input'));\
           document.getElementById('send').click(); return 'ok'; })()",
    )
    .await
    .expect("send");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('prompt'))")
        .await
        .unwrap();
    assert!(
        sent.contains("attachments/notes-2.md"),
        "the attachment did not travel with the prompt: {sent}"
    );

    // The chips are gone once it went.
    let after = b
        .text_of("String(document.querySelectorAll('#attached li').length)")
        .await
        .unwrap();
    assert_eq!(after, "0", "the attachment would be sent twice");

    // Two files of one name are told apart on the chip itself, not in a
    // tooltip somebody has to know to hover. Attaching `notes.md` from two
    // folders is the case the store's `-2` suffix exists for, and two
    // identical chips make the remove buttons a coin flip.
    let labels = b
        .text_of(concat!(
            "(() => { attached.length = 0;",
            "attached.push({name:'notes.md',path:'attachments/notes.md'});",
            "attached.push({name:'notes.md',path:'attachments/notes-2.md'});",
            "attached.push({name:'other.md',path:'attachments/other.md'});",
            "renderAttached();",
            "return JSON.stringify([...document.querySelectorAll('#attached li span')]",
            ".map(e=>e.textContent)); })()",
        ))
        .await
        .unwrap();
    assert_eq!(
        labels, r#"["notes.md (1)","notes.md (2)","other.md"]"#,
        "two files of one name are indistinguishable on screen: {labels}"
    );
    b.text_of("attached.length = 0; renderAttached(); 'ok'")
        .await
        .expect("reset");

    // Attach again, then leave the conversation.
    b.text_of("document.getElementById('attach').click(); 'ok'")
        .await
        .expect("attach again");
    settle().await;
    b.text_of("closeConversation(); 'ok'").await.expect("leave");
    settle().await;
    let left = b
        .text_of("String(document.querySelectorAll('#attached li').length)")
        .await
        .unwrap();
    assert_eq!(
        left, "0",
        "a file picked for one Bot was still attached for the next"
    );
}

/// A Bot's mark is keyed on its id, so a rename does not change it.
///
/// `openbot bot set --rename` keeps the id precisely so the conversation, the
/// groups and the routines come with it. A mark derived from the name would
/// be the one thing about a Bot that did not survive being renamed, and it is
/// the part a person recognises fastest in a sidebar.
///
/// The other property is that adjacent ids look different. `bot-1` and `bot-2`
/// landing on the same colour is what a sum over characters gets wrong, and it
/// defeats the only job a mark has.
#[tokio::test(flavor = "multi_thread")]
async fn a_bots_mark_follows_its_id_not_its_name() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let facts = b
        .text_of(concat!(
            "JSON.stringify({",
            // Same id, different names: the mark's colour must not move.
            "renamed: markOf('talent-scout','Recruiting').hue",
            " === markOf('talent-scout','Talent Scout').hue,",
            // Different ids: they must not collide, including adjacent ones.
            "adjacent: markOf('bot-1','A').hue !== markOf('bot-2','A').hue,",
            // The letter comes from the name, not the id.
            "glyph: markOf('talent-scout','Recruiting').glyph,",
            // Leading punctuation or space must not become the mark.
            "punct: markOf('x','  @ledger').glyph,",
            // Never blank: a nameless Bot still gets something.
            "blank: markOf('zz','').glyph.length })",
        ))
        .await
        .unwrap();

    assert!(
        facts.contains(r#""renamed":true"#),
        "a rename moved the mark: {facts}"
    );
    assert!(
        facts.contains(r#""adjacent":true"#),
        "adjacent ids share a colour: {facts}"
    );
    assert!(facts.contains(r#""glyph":"R""#), "{facts}");
    assert!(
        facts.contains(r#""punct":"L""#),
        "punctuation became the mark: {facts}"
    );
    assert!(
        facts.contains(r#""blank":1"#),
        "a nameless Bot has a blank mark: {facts}"
    );

    // The roster draws one per Bot, with the header showing the same.
    let drawn = b
        .text_of(concat!(
            "JSON.stringify({",
            "roster:[...document.querySelectorAll('#bots .bot-mark')].map(e=>e.textContent),",
            "header:document.querySelector('#bot-mark .bot-mark')?.textContent,",
            "sameHue:document.querySelector('#bots .bot-mark')?.style.getPropertyValue('--mark-hue')",
            " === document.querySelector('#bot-mark .bot-mark')?.style.getPropertyValue('--mark-hue')})",
        ))
        .await
        .unwrap();
    assert!(drawn.contains(r#""roster":["T"]"#), "{drawn}");
    assert!(
        drawn.contains(r#""header":"T""#),
        "the header shows no mark: {drawn}"
    );
    assert!(
        drawn.contains(r#""sameHue":true"#),
        "the sidebar and the header disagree about a Bot's colour: {drawn}"
    );

    // The colour a person sees, not the variable that feeds it. Comparing
    // `--mark-hue` would pass while every mark rendered the same accent
    // colour: `.mark` and `.bot-mark` are both one-class selectors, so a
    // `.bot-mark` rule placed above `.mark` loses on source order. Testing
    // the input to the styling is not testing the styling.
    //
    // A mark is a filled coat with an initial on it, so identity is the
    // background; the initial's colour is the same on every coat within a
    // theme by design. Reading `color` here would compare three identical
    // inks and report every Bot painted alike; the right property is the one
    // that carries the coat.
    let painted = b
        .text_of(concat!(
            "(() => { const mk = (id) => { const e = markEl(id, 'X');",
            "document.body.appendChild(e);",
            "const c = getComputedStyle(e).backgroundColor; e.remove(); return c; };",
            "return JSON.stringify([mk('bot-1'), mk('bot-2'), mk('bot-3')]); })()",
        ))
        .await
        .unwrap();
    let seen: Vec<&str> = painted
        .split('"')
        .filter(|p| p.starts_with("rgb"))
        .collect();
    assert_eq!(seen.len(), 3, "marks are not painted at all: {painted}");
    assert!(
        seen[0] != seen[1] && seen[1] != seen[2] && seen[0] != seen[2],
        "every Bot is painted the same colour: {painted}"
    );
}

/// Not carrying `.hidden` must mean "on screen".
///
/// A probe, not a feature test. Most assertions in this file judge
/// visibility by asking whether an element carries the `hidden` class (state
/// the page sets), while a few measure what the browser computes. The gap
/// between the two is where a check can pass while the thing is visibly
/// wrong.
///
/// The invariant is narrower than "everything unhidden is visible", because
/// a child of a closed dialog is legitimately laid out nowhere and no test
/// claims otherwise. It is this: an element whose whole ancestor chain is
/// unhidden must actually be visible. If that ever fails,
/// `!classList.contains('hidden')` has stopped meaning what the rest of the
/// file reads it as.
///
/// `checkVisibility`, not `offsetParent !== null`: `.dialog` is `position:
/// fixed`, and a fixed element has no `offsetParent` even while it fills the
/// screen, so the latter reports an open Settings panel as invisible.
#[tokio::test(flavor = "multi_thread")]
async fn an_element_this_suite_calls_shown_is_one_the_browser_lays_out() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // Every id judged by class anywhere in this file. `concat!` rather than a
    // continued literal: `cargo fmt` collapses those onto one line and bakes
    // this file's indentation into the string, which `messages.rs` then
    // rejects.
    let disagreements = b
        .text_of(concat!(
            "(() => { const bad = [];",
            "const JUDGED = ['attached','composer','del-confirm','dialog','dialog-fields',",
            "'dialog-secret','dialog-tool','edit-bot','log','mentions','name-dialog',",
            "'no-bot','palette','rules-dialog','workspace'];",
            "const openChain = (e) => { for (let n = e; n && n !== document.body;",
            " n = n.parentElement) if (n.classList.contains('hidden')) return false;",
            " return true; };",
            "const check = (where) => { for (const id of JUDGED) {",
            "  const e = document.getElementById(id); if (!e) continue;",
            "  if (!openChain(e)) continue;",
            "  if (!e.checkVisibility({checkOpacity:true, checkVisibilityCSS:true}))",
            "    bad.push(where + ':' + id);",
            "} };",
            "check('workspace');",
            "document.getElementById('rules-btn').click(); check('settings');",
            "document.getElementById('rules-close').click();",
            "document.getElementById('new-bot').click(); check('new-bot');",
            "[...document.querySelectorAll('#name-dialog button')]",
            ".find(x=>x.textContent.trim()==='Cancel').click();",
            "return JSON.stringify(bad); })()",
        ))
        .await
        .unwrap();

    assert_eq!(
        disagreements, "[]",
        concat!(
            "an element with no hidden ancestor is still not visible, so the ",
            "assertions that read `.hidden` as `on screen` do not mean that: {}"
        ),
        disagreements
    );
}

/// The Agent Computer panel has a floor, and it is the viewer's number.
///
/// The panel is `flex: 1.2` against the transcript, and with no minimum it
/// takes whatever is left: in a 760px window that is 288px, of which the
/// iframe gets 209. The viewer cannot draw its own empty state in that, so
/// "Nothing open yet" is cut through the descenders, in the one panel
/// somebody opens in order to look at something.
///
/// The number comes from the viewer as rendered, not from reading its
/// stylesheet: header 58, footer 35, `#empty` 206 and `main` padding 32, so
/// 331px for the iframe, and this panel adds 80px of chrome above it.
///
/// Only the panel is checked here: the iframe is cross-origin and needs a
/// live hub, so its contents cannot be reached from a page test. What this
/// pins is the thing that would starve it.
#[tokio::test(flavor = "multi_thread")]
async fn the_computer_panel_is_tall_enough_for_the_viewer_it_holds() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let size = b
        .text_of(concat!(
            "(() => { const p = document.getElementById('computer-panel');",
            "p.classList.remove('hidden');",
            "const h = Math.round(p.getBoundingClientRect().height);",
            "p.classList.add('hidden');",
            "return String(h); })()",
        ))
        .await
        .unwrap();

    let got: i32 = size.trim().parse().unwrap_or(0);
    assert!(
        got >= 411,
        "the panel is {got}px, so the viewer inside it gets less than the \
         331px it needs and clips its own text"
    );
}

/// Every roster row has the same shape, Bots and groups alike.
///
/// `.bot` is a two-column grid whose first column is the mark. A row built
/// without one puts the name in that column and the subtitle beside it, on
/// the same line, so a sidebar reads `LaunchTalent Scout, Ledger` if
/// `renderRoster` draws marks and `refreshGroups` does not.
///
/// Asserted geometrically (name above subtitle, not merely present), since
/// the failure mode is that both fit on one line.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_row_is_laid_out_like_a_bot_row() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        r#"(() => { window.__replies.roster = [{ id: "talent-scout", name: "Talent Scout",
             title: "", description: "", hidden: false, messages: 0 }];
           window.__replies.groups = [{ id: "launch", name: "Launch",
             members: [{ id: "talent-scout", name: "Talent Scout" },
                       { id: "ledger", name: "Ledger" }], messages: 0 }];
           document.getElementById('connect-btn').click();
           return 'ok'; })()"#,
    )
    .await
    .expect("connect");
    settle().await;

    let shape = b
        .text_of(concat!(
            "(() => { const row = (sel) => { const b = document.querySelector(sel);",
            "if (!b) return null;",
            "const mark = b.querySelector('.bot-mark');",
            "const name = b.querySelector('.bot-name');",
            "const sub = b.querySelector('.bot-sub');",
            "return { mark: !!mark, glyph: mark ? mark.textContent : '',",
            "stacked: !!(name && sub) &&",
            "  Math.round(sub.getBoundingClientRect().top) >",
            "  Math.round(name.getBoundingClientRect().top) }; };",
            "return JSON.stringify({bot: row('#bots .bot'), group: row('#groups .bot')}); })()",
        ))
        .await
        .unwrap();

    assert!(
        shape.contains(r#""glyph":"T""#),
        "the Bot lost its mark: {shape}"
    );
    assert!(
        shape.contains(r#""glyph":"L""#),
        "a group has no mark, so its row collapses onto one line: {shape}"
    );
    // Two rows per entry, both kinds. This is the assertion a presence check
    // cannot make: the name and the subtitle both exist in the broken layout,
    // side by side.
    assert_eq!(
        shape.matches(r#""stacked":true"#).count(),
        2,
        "a roster row put its name and subtitle on one line: {shape}"
    );
}

/// Every roster row, of every kind, in one state, with the invariant
/// quantified over all of them.
///
/// A shared presentation rule can change while one of the element kinds that
/// uses it is rendered nowhere in the suite, and the change ships green:
/// `.bot-mark` above `.mark` painting every mark the same colour, `.bot`'s
/// grid gaining a mark column and group rows collapsing onto one line, the
/// computer panel losing its floor. Tests that name a specific case cover it
/// only after it has broken.
///
/// This one enumerates nothing. It fills the roster with every variant the
/// page can draw (a plain Bot, one with a title, a hidden one, a group) and
/// asserts the row invariants for every `.bot` on screen. A fourth kind
/// added later is covered the day it is added, without anybody remembering to
/// come back here.
#[tokio::test(flavor = "multi_thread")]
async fn every_roster_row_keeps_the_shape_the_grid_was_written_for() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        r#"(() => { window.__replies.roster = [
             { id: "talent-scout", name: "Talent Scout", title: "hiring",
               description: "", hidden: false, messages: 4 },
             { id: "ledger", name: "Ledger", title: "", description: "",
               hidden: false, messages: 0 },
             { id: "archived-one", name: "Archived One", title: "", description: "",
               hidden: true, messages: 2 }];
           window.__replies.groups = [{ id: "launch", name: "Launch",
             members: [{ id: "talent-scout", name: "Talent Scout" },
                       { id: "ledger", name: "Ledger" }], messages: 0 }];
           document.getElementById('connect-btn').click();
           return 'ok'; })()"#,
    )
    .await
    .expect("connect");
    settle().await;
    // Hidden Bots are only drawn when asked for, and a hidden row has a fourth
    // child the grid must still place.
    b.text_of("document.getElementById('toggle-hidden').click(); 'ok'")
        .await
        .expect("show hidden");
    settle().await;

    let rows = b
        .text_of(concat!(
            "(() => { const out = [];",
            "for (const el of document.querySelectorAll('#bots .bot, #groups .bot')) {",
            "  const q = (s) => el.querySelector(s);",
            "  const top = (e) => e ? Math.round(e.getBoundingClientRect().top) : null;",
            "  const left = (e) => e ? Math.round(e.getBoundingClientRect().left) : null;",
            "  const name = q('.bot-name'), sub = q('.bot-sub'), mark = q('.bot-mark');",
            "  const tag = q('.bot-hidden');",
            "  out.push({ name: name ? name.textContent : '(none)',",
            "    mark: !!mark,",
            "    colour: mark ? getComputedStyle(mark).color : '',",
            // The subtitle sits below the name, never beside it.
            "    stacked: !!(name && sub) && top(sub) > top(name),",
            // Everything after the mark shares one column: a child the grid
            // did not place lands back under the mark instead.
            "    aligned: !tag || left(tag) === left(name) });",
            "}",
            "return JSON.stringify(out); })()",
        ))
        .await
        .unwrap();

    assert!(rows.contains("Launch"), "the groups never rendered: {rows}");
    assert!(
        rows.contains("Archived One"),
        "the hidden Bot never rendered: {rows}"
    );
    assert!(
        !rows.contains(r#""mark":false"#),
        "a roster row has no mark, so its grid collapses: {rows}"
    );
    assert!(
        !rows.contains(r#""stacked":false"#),
        "a roster row put its name and subtitle on one line: {rows}"
    );
    assert!(
        !rows.contains(r#""aligned":false"#),
        "a roster row has a child the grid did not place, so it fell into the \
         mark's column: {rows}"
    );
}

/// A button the shell called dangerous is drawn dangerous, whatever its
/// kind.
///
/// If the page decided this itself with `option.kind.startsWith("reject")`,
/// a prefix match on ACP's vocabulary (which is `#[non_exhaustive]`), a kind
/// that is neither `allow_*` nor `reject_*` would come out `primary`: the
/// accent styling this page reserves for the permitted choice, on a control
/// nobody can classify, in the dialog whose own rule is that allow and deny
/// must not look the same.
///
/// `refuses` answers it in Rust and fails closed. This checks the page
/// honours that instead of re-deciding it, using a kind no prefix match
/// would catch.
#[tokio::test(flavor = "multi_thread")]
async fn an_unrecognised_option_kind_is_still_styled_as_a_refusal() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        r#"(() => { window.__fire('permission-request',
             { id: "a1", session: "s1", tool: "shell.exec",
               fields: [{ name: "command", value: "rm -rf /", long: false }],
               options: [
                 { id: "allow-once", name: "Allow once", kind: "allow_once",
                   danger: false },
                 { id: "cancel-it", name: "Cancel", kind: "cancel",
                   danger: true }] });
           return 'ok'; })()"#,
    )
    .await
    .expect("an ask");
    settle().await;

    let styling = b
        .text_of(
            "JSON.stringify([...document.querySelectorAll('#dialog-options button')]\
             .map(x=>[x.textContent,x.className]))",
        )
        .await
        .unwrap();

    assert!(styling.contains(r#"["Allow once","primary"]"#), "{styling}");
    assert!(
        styling.contains(r#"["Cancel","danger"]"#),
        "an option the shell called dangerous was drawn as the permitted \
         choice: {styling}"
    );
    // The page adds no refusal of its own, because one was offered; it must
    // read `danger`, not the kind, to know that.
    assert!(
        !styling.contains("Refuse"),
        "a second way to decline appeared: {styling}"
    );
}

/// Typing a word costs one search, not one per letter.
///
/// `search` shells out to `openbot search`, which reads every conversation in
/// the home (on the order of half a second over 50 Bots and 100k messages, in
/// release). Firing one per keystroke would make typing `renewal` spawn seven
/// processes and scan the same home seven times to answer one question, with
/// only the last answer ever shown.
///
/// Names still filter on the keystroke, because those are already in the
/// page. It is the trip to the binary that waits for a pause.
#[tokio::test(flavor = "multi_thread")]
async fn typing_in_the_palette_searches_once_it_settles() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        "(() => { window.__replies.search = [];\
           document.dispatchEvent(new KeyboardEvent('keydown',\
             { key: 'k', ctrlKey: true, bubbles: true })); return 'ok'; })()",
    )
    .await
    .expect("palette");
    settle().await;

    // Type a word at speed, the way anybody types.
    b.text_of(
        "(() => { const i = document.querySelector('#palette input');\
           for (const ch of 'renewal') { i.value += ch;\
             i.dispatchEvent(new Event('input')); }\
           return 'ok'; })()",
    )
    .await
    .expect("type");

    // Before the pause elapses, nothing has gone to the binary, while the
    // names have already been filtered from what the page holds.
    let immediate = b
        .text_of("String(window.__sent('search').length)")
        .await
        .unwrap();
    assert_eq!(
        immediate, "0",
        "a scan of every conversation went out mid-word"
    );

    // Once the pause elapses, exactly one scan goes out. Waited for, not
    // slept for: the debounce is a timer and a fixed sleep races it.
    wait_until(
        &b,
        "String(window.__sent('search').length >= 1)",
        Duration::from_secs(3),
    )
    .await;
    let after = b
        .text_of("String(window.__sent('search').length)")
        .await
        .unwrap();
    assert_eq!(
        after, "1",
        "typing one word cost {after} full scans of the home"
    );
    // It searched the whole word, not a prefix.
    let sent = b
        .text_of("JSON.stringify(window.__sent('search'))")
        .await
        .unwrap();
    assert!(sent.contains("renewal"), "{sent}");
}

/// The approval box takes the keyboard, and gives it back.
///
/// `role="dialog" aria-modal="true"` is a claim the page makes to assistive
/// technology, and the page has to keep it. An overlay stops a pointer and
/// never stops a keyboard: without focus management, raising an approval
/// leaves focus on the composer, an explicit attempt to focus the composer
/// while the box is up succeeds, and nothing is marked `inert`, so a person
/// working by keyboard is told a decision is needed and left outside the box
/// holding it.
///
/// Three things, in the order they happen to somebody:
///
/// 1. Focus moves into the box, and not onto a button, because options
///    arrive in the agent's order with the permitting one usually first, and
///    a focused button puts one Return between a keyboard and an approval.
/// 2. What is behind the box cannot take focus while it is up.
/// 3. Answering hands focus back to whatever had it.
///
/// Step 3 is worthless on its own: focus ends on the composer both when this
/// works and when nothing ever moved it. Step 1 is what makes it mean
/// anything, so the two are asserted together and never apart.
#[tokio::test]
async fn an_approval_takes_the_keyboard_and_gives_it_back() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A person mid-sentence in the composer, which is where the window puts
    // them.
    b.text_of("document.getElementById('input').focus(); 'ok'")
        .await
        .expect("focus the composer");
    settle().await;
    assert_eq!(
        b.text_of("String(document.activeElement.id)")
            .await
            .unwrap(),
        "input",
        "the composer never had focus, so this test cannot show it being taken"
    );

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an ask");
    settle().await;

    // The box is really up, with real controls in it. Without this, a change
    // that stopped rendering the dialog would satisfy every assertion below by
    // leaving focus somewhere that is merely not the composer.
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "no approval box was shown"
    );
    let buttons = b
        .text_of("String(document.querySelectorAll('#dialog-options button').length)")
        .await
        .unwrap();
    assert_ne!(
        buttons, "0",
        "the approval box offered nothing to answer with"
    );

    assert_eq!(
        b.text_of("String(document.getElementById('dialog').contains(document.activeElement))")
            .await
            .unwrap(),
        "true",
        "an approval opened and the keyboard stayed behind it"
    );
    assert_ne!(
        b.text_of("String(document.activeElement.tagName)")
            .await
            .unwrap(),
        "BUTTON",
        "a button holds focus, so Return alone answers an approval nobody has read"
    );

    // Behind the box: an explicit focus() is the strongest form of the attempt
    // a Tab would make, and it must not land.
    b.text_of("document.getElementById('input').focus(); 'ok'")
        .await
        .unwrap();
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').contains(document.activeElement))")
            .await
            .unwrap(),
        "true",
        "the composer took focus from behind an open approval"
    );

    b.text_of("document.querySelector('#dialog-options button').click(); 'ok'")
        .await
        .unwrap();
    settle().await;
    assert_eq!(
        b.text_of("String(document.activeElement.id)")
            .await
            .unwrap(),
        "input",
        "answering left the keyboard nowhere; the composer has to be tabbed back to"
    );
}

/// WCAG relative luminance and contrast, plus the effective background behind
/// an element: a colour with alpha 0 tells you nothing, so this walks up
/// until it finds something painted.
const CONTRAST_JS: &str = r#"
  const lum = (rgb) => { const [r,g,b] = rgb.map(v => { v/=255; return v <= 0.04045 ? v/12.92 : Math.pow((v+0.055)/1.055, 2.4); }); return 0.2126*r + 0.7152*g + 0.0722*b; };
  const parse = (c) => { const m = c.match(/rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/); return m ? { rgb:[+m[1],+m[2],+m[3]], a: m[4] === undefined ? 1 : +m[4] } : null; };
  // The colour actually behind an element: every ancestor's background,
  // composited from the outside in. Stopping at the first non-transparent
  // layer reads a 14% wash as a solid and reports 1.00 for text sitting on
  // it, as with a semantic pill (green ink on a green wash over the dark
  // page). Alpha has to be blended, not treated as paint.
  const bgOf = (el) => {
    const layers = [];
    for (let e = el; e; e = e.parentElement) {
      const c = parse(getComputedStyle(e).backgroundColor);
      if (c && c.a > 0) { layers.push(c); if (c.a >= 1) break; }
    }
    let out = [0,0,0];
    for (const c of layers.reverse()) out = out.map((v, i) => Math.round(c.rgb[i] * c.a + v * (1 - c.a)));
    return out;
  };
  const ratio = (f,b) => { const L1 = lum(f), L2 = lum(b); const hi = Math.max(L1,L2), lo = Math.min(L1,L2); return (hi+0.05)/(lo+0.05); };
"#;

/// Every coat a Bot can wear is legible, in both themes.
///
/// A mark is a filled coat with the Bot's initial on it, so the pair a person
/// reads is initial on coat, and there are exactly eight coats, not a
/// 360-hue wheel. That is what lets this walk the whole set rather than
/// sample it: a hue wheel can put some Bots at 2.09:1 with no roster able to
/// show it, while eight named coats can be checked in full.
///
/// Both themes, because the ink flips with the theme (dark coats are bright
/// and carry dark ink, light coats are deep and carry white), so a pair that
/// passes in one theme says nothing about the other. The theme is switched
/// by `color-scheme` on the root, which is how the page picks it, and the
/// check waits for a style flush before reading.
///
/// The colours are read back from a rendered clone of a real mark, not
/// recomputed from the tokens: a test that restated `--coat-4` would go on
/// passing after somebody edited the stylesheet, which is what it exists to
/// notice.
#[tokio::test]
async fn every_coat_a_bot_can_wear_is_legible() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    let js = format!(
        r#"(() => {{
        {CONTRAST_JS}
        const marks = Array.from(document.querySelectorAll('.bot-mark'));
        if (!marks.length) throw new Error('no mark is on screen, so nothing here was checked');
        const coats = typeof COATS === 'number' ? COATS : 0;
        if (coats < 2) throw new Error('COATS is not a count the page exposes');
        const probe = marks[0].cloneNode(true);
        marks[0].parentElement.appendChild(probe);
        const out = [];
        for (const scheme of ['dark', 'light']) {{
          document.documentElement.style.colorScheme = scheme;
          for (let c = 0; c < coats; c++) {{
            probe.style.setProperty('--mark-hue', String(c));
            probe.style.setProperty('--coat', `var(--coat-${{c}})`);
            const cs = getComputedStyle(probe);
            const fg = parse(cs.color).rgb;
            const bg = parse(cs.backgroundColor).rgb;
            out.push({{ scheme, coat: c, ratio: +ratio(fg, bg).toFixed(2), fg: fg.join(','), bg: bg.join(',') }});
          }}
        }}
        document.documentElement.style.colorScheme = '';
        probe.remove();
        return JSON.stringify(out);
        }})()"#
    );
    let out = b.text_of(&js).await.expect("the coat sweep runs");
    let rows: Vec<&str> = out.split("},{").collect();
    assert_eq!(
        rows.len(),
        16,
        "expected 8 coats × 2 themes = 16 measurements, got {}: {out}",
        rows.len()
    );
    // The coats are eight *distinguishable* identities, and that is the property
    // worth pinning. This used to assert that coat 0 painted differently in dark
    // than in light, on the reasoning that a page ignoring `color-scheme` would
    // measure the same numbers twice. That reasoning was sound against the old
    // token layer, where every coat was a `light-dark()` pair.
    //
    // DIRECTION.md deliberately removed the second set: the coats now sit
    // between 35 and 55 lightness precisely so one value reads on both themes.
    // Identical-across-themes is the design, so the old assertion would now fail
    // on correct output. What still has to hold - and what a broken sweep would
    // break - is that the eight differ from *each other*.
    let bg_of = |row: &str| {
        row.split("\"bg\":\"")
            .nth(1)
            .and_then(|t| t.split('"').next())
            .unwrap_or("")
            .to_owned()
    };
    let dark: std::collections::BTreeSet<String> = rows[..8].iter().map(|r| bg_of(r)).collect();
    assert_eq!(
        dark.len(),
        8,
        "two Bots wear the same coat, so the roster cannot tell them apart: {out}"
    );
    // And the sweep measured real paint rather than an empty string eight times.
    assert!(
        dark.iter().all(|b| b.matches(',').count() == 2),
        "the coat sweep did not read colours: {out}"
    );
    let failing: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|r| {
            r.split("\"ratio\":")
                .nth(1)
                .and_then(|t| t.split(',').next())
                .and_then(|t| t.trim().parse::<f64>().ok())
                .is_some_and(|v| v < 4.5)
        })
        .collect();
    assert!(
        failing.is_empty(),
        "an initial cannot be read on its coat (below 4.5): {failing:?}"
    );
}

/// No text in the window is below the contrast it needs, in any of the
/// surfaces it has.
///
/// Sweeping one screen and calling it "the window" would be an overclaim:
/// the workspace is a couple of dozen elements, while five dialogs and the
/// computer panel hold the rest. Each surface is opened, proved open, and
/// swept. Without that proof a click that silently stopped working would
/// report a clean sweep of a screen nobody was looking at.
///
/// 4.5 for normal text and 3.0 for large, as WCAG AA defines large: 24px, or
/// 18.66px when bold.
#[tokio::test]
async fn no_text_in_any_surface_falls_below_the_contrast_it_needs() {
    // Label, the expression that raises it, and what must then be on screen
    // for the sweep of it to mean anything.
    // Expressions rather than button ids: the palette opens from a keyboard
    // shortcut, and a table that can only describe buttons quietly means "the
    // surfaces reachable by clicking one" while calling itself every surface.
    const SURFACES: &[(&str, &str, &str)] = &[
        (
            "settings",
            "document.getElementById('rules-btn').click()",
            "rules-dialog",
        ),
        (
            "credentials",
            "document.getElementById('credentials').click()",
            "secrets-dialog",
        ),
        (
            "new bot",
            "document.getElementById('new-bot').click()",
            "name-dialog",
        ),
        (
            "edit bot",
            "document.getElementById('edit-bot').click()",
            "edit-dialog",
        ),
        ("palette", "openPalette()", "palette"),
        (
            "agent computer",
            "document.getElementById('computer').click()",
            "computer-panel",
        ),
    ];

    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // Joined to the page, not to a hand-written list. Every modal the page
    // knows about has to be swept here or named as swept elsewhere, or "any
    // surface" is a claim about the surfaces somebody happened to think of
    // (a hand-written list is how the palette gets missed).
    let listed = b
        .text_of("JSON.stringify(MODAL_IDS)")
        .await
        .expect("the page exposes MODAL_IDS");
    let swept_here: Vec<&str> = SURFACES.iter().map(|(_, _, shown)| *shown).collect();
    for id in listed
        .trim_matches(['[', ']'])
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| !s.is_empty())
    {
        assert!(
            swept_here.contains(&id) || id == "dialog",
            "`{id}` is a modal the page can show and nothing sweeps its text for contrast"
        );
    }

    let mut looked = 0u32;
    let mut bad: Vec<String> = Vec::new();
    let sweep = |out: String, where_: &str, looked: &mut u32, bad: &mut Vec<String>| {
        let n: u32 = out
            .split("\"looked\":")
            .nth(1)
            .and_then(|t| t.split(',').next())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        *looked += n;
        // Per surface, not just in total: one screen contributing everything
        // would otherwise hide five that rendered nothing. The smallest real
        // surface has around 18 text elements.
        assert!(
            n >= 10,
            "`{where_}` yielded only {n} text elements, so it was swept without being looked at"
        );
        if !out.contains("\"bad\":[]") {
            bad.push(format!("{where_}: {out}"));
        }
    };

    // Both themes. The CI runners default to a light scheme and the
    // development machine to dark, so a sweep of one theme holds only for the
    // machines that happen to match it. The theme is switched the way the page
    // switches it: `color-scheme` on the root, which `light-dark()` follows.
    for scheme in ["dark", "light"] {
        b.text_of(&format!(
            "document.documentElement.style.colorScheme = '{scheme}'; 'ok'"
        ))
        .await
        .expect("the scheme can be set");
        settle().await;
        let tag = |where_: &str| format!("{scheme}/{where_}");

        sweep(
            b.text_of(&contrast_sweep()).await.expect("a sweep"),
            &tag("workspace"),
            &mut looked,
            &mut bad,
        );

        for (label, opener, shown) in SURFACES {
            b.text_of(&format!("{opener}; 'ok'"))
                .await
                .unwrap_or_else(|e| panic!("`{label}` could not be opened by `{opener}`: {e}"));
            settle().await;
            assert_eq!(
                b.text_of(&format!(
                    "String(document.getElementById('{shown}').classList.contains('hidden'))"
                ))
                .await
                .unwrap(),
                "false",
                "clicking `{opener}` did not open `{shown}`, so sweeping `{label}` proved nothing"
            );
            sweep(
                b.text_of(&contrast_sweep()).await.expect("a sweep"),
                &tag(label),
                &mut looked,
                &mut bad,
            );
            b.text_of(
                "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'ok'",
            )
            .await
            .unwrap();
            settle().await;
        }

        // The approval box last: it is the one Escape will not close.
        b.text_of(&format!(
            "window.__fire('permission-request', {}); 'ok'",
            ask("a1", "s1", "fs.write")
        ))
        .await
        .expect("an ask");
        settle().await;
        sweep(
            b.text_of(&contrast_sweep()).await.expect("a sweep"),
            &tag("approval"),
            &mut looked,
            &mut bad,
        );

        // Leave the page as the next pass expects it: the approval answered,
        // and the computer panel closed, since its opener is a toggle and a
        // second click would close rather than open it.
        b.text_of(concat!(
            "document.querySelector('#dialog-options button')?.click(); ",
            "document.getElementById('close-computer').click(); 'ok'"
        ))
        .await
        .unwrap();
        settle().await;
    }
    b.text_of("document.documentElement.style.colorScheme = ''; 'ok'")
        .await
        .unwrap();

    assert!(
        looked >= 300,
        "only {looked} text elements across every surface; this swept far less than it claims"
    );
    assert!(
        bad.is_empty(),
        "text below WCAG AA is on screen:\n{}",
        bad.join("\n")
    );
}

/// The sweep itself: every element with its own visible text, its computed
/// colour against whatever is actually painted behind it.
fn contrast_sweep() -> String {
    format!(
        r#"(() => {{
        {CONTRAST_JS}
        const own = (el) => Array.from(el.childNodes).some(n => n.nodeType === 3 && n.textContent.trim());
        const bad = [];
        let looked = 0;
        for (const el of document.querySelectorAll('body *')) {{
          if (!own(el)) continue;
          const cs = getComputedStyle(el);
          if (cs.visibility === 'hidden' || cs.display === 'none' || +cs.opacity === 0) continue;
          if (!el.getClientRects().length) continue;
          looked++;
          const px = parseFloat(cs.fontSize);
          const large = px >= 24 || (px >= 18.66 && +cs.fontWeight >= 700);
          const r = ratio(parse(cs.color).rgb, bgOf(el));
          if (r + 0.005 < (large ? 3 : 4.5)) {{
            bad.push(((el.id || el.className || el.tagName) + ' "' + el.textContent.trim().slice(0,24) + '" ' + cs.fontSize + '/' + cs.fontWeight + ' = ' + r.toFixed(2)));
          }}
        }}
        return JSON.stringify({{ looked, bad }});
        }})()
        "#
    )
}

/// Pull one integer field out of a small JSON object the page returned.
fn field(out: &str, name: &str) -> i64 {
    out.split(&format!("\"{name}\":"))
        .nth(1)
        .and_then(|t| t.split(&[',', '}'][..]).next())
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or_else(|| panic!("no `{name}` in {out}"))
}

/// Every coat gets worn, and neighbours do not share one.
///
/// With eight coats instead of a wheel, "visibly different" is settled by the
/// palette (any two distinct coats are far apart by construction), so the
/// question moves to the distribution: does the hash actually reach all
/// eight, and does it avoid handing `bot-1` and `bot-2` the same one? A hash
/// that folded to two coats would pass every per-Bot check and make a roster
/// of ten Bots read as five.
///
/// The naive character-sum is computed alongside as a control. Under it,
/// sequential ids advance one coat at a time (every eighth pair collides and
/// the pattern is visible in any roster), so a measure that could not tell
/// the two apart would be measuring nothing.
#[tokio::test]
async fn every_coat_gets_worn_and_neighbours_do_not_share_one() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    let out = b
        .text_of(
            r#"(() => {
      if (typeof markOf !== 'function') throw new Error('markOf is not reachable from the page');
      const N = 500;
      const worn = new Set();
      let adjacentSame = 0;
      let prev = null;
      for (let i = 1; i <= N; i++) {
        const h = markOf('bot-' + i, 'x').hue;
        worn.add(h);
        if (prev !== null && prev === h) adjacentSame++;
        prev = h;
      }
      // Control: a sum over characters, folded the same way.
      const sum = (id) => { let t = 0; for (const c of id) t += c.codePointAt(0); return t % COATS; };
      let sumAdjacentSame = 0; let sp = null;
      for (let i = 1; i <= N; i++) { const h = sum('bot-' + i); if (sp !== null && sp === h) sumAdjacentSame++; sp = h; }
      return JSON.stringify({ coats: COATS, worn: worn.size, adjacentSame, pairs: N - 1, sumAdjacentSame });
    })()"#,
        )
        .await
        .expect("the distribution runs");

    assert_eq!(
        field(&out, "worn"),
        field(&out, "coats"),
        "some coats are never worn: {out}"
    );
    // A uniform hash over 8 buckets collides on adjacent draws about 1 in 8
    // of the time; allow up to twice that, which is still a roster where
    // neighbours almost always differ.
    let pairs = field(&out, "pairs") as f64;
    let same = field(&out, "adjacentSame") as f64;
    assert!(
        same / pairs <= 0.25,
        "sequential ids too often wear the same coat: {out}"
    );
    // The control has to look different from the subject or the measure is
    // blind. A character sum steps ids through the coats in order, so it
    // never repeats on adjacent ids: its collision rate is ~0, which marks
    // it as a counter and not a spread.
    let control = field(&out, "sumAdjacentSame") as f64;
    assert!(
        (control / pairs - same / pairs).abs() > 0.05,
        "the character-sum control scored like the shipped hash, so this measure cannot \
         tell a spread from a counter: {out}"
    );
}

/// An approval is drawn above a panel somebody opened, not behind it.
///
/// Both are full-viewport overlays holding a centred card of the same width,
/// so whichever paints last covers the other completely (the approval card
/// is a 560×188 box under a 560×514 one). Geometry cannot separate them;
/// only order can. With every dialog on the same `z-index`, stacking falls
/// to document order, and the approval is declared first, so it needs a
/// higher one.
///
/// The measurement lifts `inert` first. Containment marks everything but the
/// top modal inert, and an inert element is excluded from hit-testing but
/// still painted, so `elementFromPoint` would name the approval either way
/// and this test would pass without the z-index. Lifting it asks the only
/// question that matters here: what is actually on top.
#[tokio::test]
async fn an_approval_is_drawn_above_an_open_panel() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of("document.getElementById('rules-btn').click(); 'ok'")
        .await
        .expect("open settings");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('rules-dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "settings did not open, so nothing was stacked over anything"
    );

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an ask");
    settle().await;
    assert_eq!(
        b.text_of("String(document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "no approval was raised"
    );

    // They really do occupy the same space; otherwise "on top" would be a
    // question about nothing.
    let overlap = b
        .text_of(
            r#"(() => {
      const a = document.querySelector('#dialog .dialog-card').getBoundingClientRect();
      const s = document.querySelector('#rules-dialog .dialog-card').getBoundingClientRect();
      const ox = Math.max(0, Math.min(a.right, s.right) - Math.max(a.left, s.left));
      const oy = Math.max(0, Math.min(a.bottom, s.bottom) - Math.max(a.top, s.top));
      return String(Math.round(100 * ox * oy / (a.width * a.height)));
    })()"#,
        )
        .await
        .unwrap();
    assert_eq!(
        overlap, "100",
        "the two cards no longer overlap, so this is no longer the situation being tested"
    );

    let top = b
        .text_of(
            r#"(() => {
      for (const e of document.querySelectorAll('[inert]')) e.removeAttribute('inert');
      const a = document.querySelector('#dialog .dialog-card').getBoundingClientRect();
      const el = document.elementFromPoint(a.left + a.width / 2, a.top + a.height / 2);
      return String(el && (el.closest('.dialog') || {}).id);
    })()"#,
        )
        .await
        .unwrap();
    assert_eq!(
        top, "dialog",
        "a panel is painted over the approval box, which is invisible behind it"
    );
}

/// Every modal in the window contains the keyboard while it is open.
///
/// All of them carry `role="dialog" aria-modal="true"`, and each has to keep
/// it: a modal that leaves focus on the composer, or moves it to its own
/// field but lets the composer take it straight back, or marks nothing
/// `inert`, has an overlay that stops a pointer and not a keyboard.
///
/// The list comes from the page (`MODAL_IDS`, the same one `show` consults),
/// so a new dialog cannot be added without either appearing here or failing
/// the join below. A list retyped in this file would go stale the moment
/// somebody added one, which is the only moment it matters.
#[tokio::test]
async fn every_modal_contains_the_keyboard_while_it_is_open() {
    // How each is raised. Expressions rather than element ids:
    // the approval box has no button, and the palette has a shortcut instead
    // of one, so an id-only table could not describe the set it claims to
    // cover.
    const OPENS: &[(&str, &str)] = &[
        ("dialog", "window.__fire('permission-request', ASK)"),
        ("palette", "openPalette()"),
        (
            "rules-dialog",
            "document.getElementById('rules-btn').click()",
        ),
        (
            "secrets-dialog",
            "document.getElementById('credentials').click()",
        ),
        ("name-dialog", "document.getElementById('new-bot').click()"),
        ("edit-dialog", "document.getElementById('edit-bot').click()"),
    ];

    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let listed = b
        .text_of("JSON.stringify(MODAL_IDS)")
        .await
        .expect("the page exposes MODAL_IDS");
    for (id, _) in OPENS {
        assert!(
            listed.contains(&format!("\"{id}\"")),
            "`{id}` is opened here but is not in the page's MODAL_IDS: {listed}"
        );
    }
    let counted = listed.matches('"').count() / 2;
    assert_eq!(
        counted,
        OPENS.len(),
        "the page lists {counted} modals and this test opens {}: {listed}",
        OPENS.len()
    );

    for (id, opener) in OPENS {
        b.text_of("document.getElementById('input').focus(); 'ok'")
            .await
            .unwrap();
        settle().await;
        let js = opener.replace("ASK", &ask("a1", "s1", "fs.write"));
        b.text_of(&format!("{js}; 'ok'"))
            .await
            .unwrap_or_else(|e| panic!("`{id}` could not be opened by `{opener}`: {e}"));
        settle().await;

        assert_eq!(
            b.text_of(&format!(
                "String(document.getElementById('{id}').classList.contains('hidden'))"
            ))
            .await
            .unwrap(),
            "false",
            "`{id}` never opened, so nothing about it was tested"
        );
        assert_eq!(
            b.text_of(&format!(
                "String(document.getElementById('{id}').contains(document.activeElement))"
            ))
            .await
            .unwrap(),
            "true",
            "`{id}` opened and the keyboard stayed behind it"
        );
        // The strongest form of the attempt a Tab would make.
        b.text_of("document.getElementById('input').focus(); 'ok'")
            .await
            .unwrap();
        assert_eq!(
            b.text_of(&format!(
                "String(document.getElementById('{id}').contains(document.activeElement))"
            ))
            .await
            .unwrap(),
            "true",
            "the composer took focus from behind `{id}`"
        );

        // The approval box is the one Escape will not close, by design.
        if *id == "dialog" {
            b.text_of("document.querySelector('#dialog-options button').click(); 'ok'")
                .await
                .unwrap();
        } else {
            b.text_of(
                "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'ok'",
            )
            .await
            .unwrap();
        }
        settle().await;
        assert_eq!(
            b.text_of("String(document.activeElement && document.activeElement.id)")
                .await
                .unwrap(),
            "input",
            "closing `{id}` left the keyboard nowhere"
        );
    }
}

/// Every transcript line says who it is from, to somebody not reading the
/// colours.
///
/// Six kinds (the person, the Bot, its reasoning, a tool call, a progress
/// note, a result) that differ by alignment and background.
/// `every_message_kind_is_styled` in `defaults.rs` proves each one is
/// decorated, which is also a proof that the distinction is decoration: WCAG
/// 1.4.1, colour as the only carrier, on a transcript where one of the kinds
/// is the Bot's private reasoning and another is what it actually said. Each
/// line therefore carries a spoken label as well.
///
/// Quantified over `Kind::ALL`, so a seventh kind fails here as well as at
/// `Kind::as_str`. The labels are read back out of the rendered DOM rather
/// than compared against the page's own map, which would only check the map
/// against itself. What is asserted instead are the properties that make the
/// labels useful: every kind gets one, they are all different, and no two
/// share a first word, because a listener commits to "Bot…" before hearing
/// whether it was going to be "Bot thinking".
#[tokio::test]
async fn every_message_kind_says_who_it_is_from() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A result or a progress line that arrives while a tool step is open folds
    // into that step's row (see `appendChunk`), so to measure each kind as its
    // own row the tool is fired last. Standalone rows are a real case: a
    // result whose step is not on screen, after a reload of a long thread.
    let mut order: Vec<openbot_app::Kind> = openbot_app::Kind::ALL.to_vec();
    order.sort_by_key(|k| k.as_str() == "tool");
    let mut labels: Vec<(String, String)> = Vec::new();
    for kind in order {
        let name = kind.as_str();
        let body = format!("body-of-{name}");
        b.text_of(&format!(
            "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"{name}\",\"text\":\"{body}\"}}); 'ok'"
        ))
        .await
        .expect("a chunk");
        settle().await;

        let out = b
            .text_of(&format!(
                r#"(() => {{
          const el = document.querySelector('#log .msg.{name}');
          if (!el) return JSON.stringify({{ missing: true }});
          const tag = el.querySelector('.sr-only');
          const r = tag && tag.getBoundingClientRect();
          return JSON.stringify({{
            label: tag ? tag.textContent : "",
            text: el.textContent,
            drawnWidth: r ? Math.round(r.width) : -1,
            inTree: !!tag && getComputedStyle(tag).display !== 'none' && getComputedStyle(tag).visibility !== 'hidden'
          }});
        }})()"#
            ))
            .await
            .unwrap();
        assert!(
            !out.contains("\"missing\":true"),
            "nothing rendered for kind `{name}`, so it was not tested: {out}"
        );

        let label = out
            .split("\"label\":\"")
            .nth(1)
            .and_then(|t| t.split('"').next())
            .unwrap_or("")
            .to_owned();
        assert!(
            !label.trim().is_empty(),
            "a `{name}` line says nothing about who it is from; to a screen reader it is \
             indistinguishable from the Bot speaking: {out}"
        );
        assert!(
            out.contains(&body),
            "the `{name}` line lost its own text when the speaker was added: {out}"
        );
        // Said, not drawn: it must stay out of the picture and stay in the
        // accessibility tree. `display:none` would do the first and undo the
        // second.
        assert!(
            out.contains("\"drawnWidth\":1") || out.contains("\"drawnWidth\":0"),
            "the speaker label on a `{name}` line is drawn on screen: {out}"
        );
        assert!(
            out.contains("\"inTree\":true"),
            "the speaker label on a `{name}` line is hidden from assistive technology too: {out}"
        );
        labels.push((name.to_owned(), label));
    }

    for (i, (ka, a)) in labels.iter().enumerate() {
        for (kb, b_) in labels.iter().skip(i + 1) {
            assert_ne!(a, b_, "`{ka}` and `{kb}` are announced identically");
            // The colon is punctuation, not a word. Splitting on whitespace
            // alone would compare "Bot:" with "Bot" and never match, so the
            // check would pass for labels a listener cannot separate.
            let first = |s: &str| {
                s.replace(':', " ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_owned()
            };
            assert_ne!(
                first(a),
                first(b_),
                "`{ka}` and `{kb}` are announced as `{a}` and `{b_}`, which a listener \
                 cannot tell apart until the second word"
            );
        }
    }
}

/// Every dialog in the page is in the list that makes dialogs modal.
///
/// `MODAL_IDS` is an explicit list rather than a
/// `querySelectorAll('[role="dialog"]')`, so that a test has something to
/// quantify over. That only works if something joins the list to the page:
/// a dialog present in the markup (`role="dialog" aria-modal="true"`, opened
/// by a shortcut) but absent from the list is measurably not modal, because
/// the composer takes focus straight back out of it, and a join between the
/// list and a table in a test does not catch that. This is the other side of
/// the join: the shipped markup.
#[tokio::test]
async fn every_modal_in_the_page_is_in_the_list() {
    const PAGE: &str = include_str!("../ui/index.html");

    let declared = PAGE.matches(r#"role="dialog""#).count();
    let ids: Vec<&str> = PAGE
        .match_indices(r#"role="dialog""#)
        .filter_map(|(at, _)| {
            // The id is on the same tag, before the role attribute.
            let before = &PAGE[..at];
            let start = before.rfind(r#"id=""#)? + 4;
            let rest = &PAGE[start..];
            Some(&rest[..rest.find('"')?])
        })
        .collect();
    assert_eq!(
        ids.len(),
        declared,
        "the markup declares {declared} dialogs but only {} carried a readable id ({ids:?}); \
         this scan has stopped reading what it says it reads",
        ids.len()
    );
    assert!(
        declared >= 2,
        "only {declared} dialogs found; the scan is not working"
    );

    let Some((b, _p)) = page().await else { return };
    let listed = b
        .text_of("JSON.stringify(MODAL_IDS)")
        .await
        .expect("the page exposes MODAL_IDS");
    for id in &ids {
        assert!(
            listed.contains(&format!("\"{id}\"")),
            "`{id}` is a dialog in the markup but is not in MODAL_IDS, so nothing makes it \
             modal to the keyboard and nothing else in this suite would notice: {listed}"
        );
    }
    let listed_count = listed.matches('"').count() / 2;
    assert_eq!(
        listed_count,
        ids.len(),
        "MODAL_IDS holds {listed_count} ids and the markup declares {}; a list entry with no \
         dialog behind it makes `show` do work for an element that never opens",
        ids.len()
    );
}

/// A search that failed does not say "Nothing matches".
///
/// Names are matched inside the page and appear immediately; only the message
/// hits come from the binary, appended when they arrive. A `search` that
/// fails must not leave the palette showing its empty state (a definite
/// negative answer to a question nothing had answered), or somebody stops
/// looking for a conversation that is there.
///
/// The reset matters as much as the message: the failure sentence lives in the
/// same element as the empty state, so without clearing it on every render one
/// broken search would go on claiming the store is unreadable for the rest of
/// the session. Both directions are asserted here.
#[tokio::test]
async fn a_search_that_failed_does_not_report_nothing_found() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of("window.__throw = { search: 'the store could not be read' }; openPalette(); 'ok'")
        .await
        .expect("the palette opens");
    settle().await;

    let typed = |q: &str| {
        format!(
            "(() => {{ const i = document.getElementById('palette-input'); i.value = '{q}'; \
             i.dispatchEvent(new Event('input',{{bubbles:true}})); return 'ok'; }})()"
        )
    };
    b.text_of(&typed("zzz-nothing-matches-this")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;

    let empty = b
        .text_of("document.getElementById('palette-empty').textContent")
        .await
        .unwrap();
    assert!(
        empty.contains("Could not search"),
        "a failed search reported an answer it never got: {empty:?}"
    );
    assert!(
        empty.contains("the store could not be read"),
        "the reason the search failed was dropped: {empty:?}"
    );

    // A working search afterwards must not inherit the failure.
    b.text_of("window.__throw = {}; 'ok'").await.unwrap();
    b.text_of(&typed("zzz-still-nothing")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        b.text_of("document.getElementById('palette-empty').textContent")
            .await
            .unwrap(),
        "Nothing matches.",
        "one failed search went on claiming the store was unreadable"
    );
}

/// A settings error does not outlive the failure it describes.
///
/// One error line is shared by every control in the panel, so it has to be
/// cleared on success. Otherwise a refused removal followed by a removal that
/// works leaves the panel reading "the hub refused", an error about an action
/// that has since succeeded. Somebody reads that and believes the rule they
/// just removed is still there, which is the wrong belief to leave somebody
/// with about a policy.
///
/// Both directions, because clearing on success is only right if the failure
/// still shows in the first place.
#[tokio::test]
async fn a_settings_error_does_not_outlive_its_failure() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        "(() => { window.__replies.policy_list = [{tool:'fs.write',action:'ask',reason:'r'}]; \
         window.__throw = { policy_remove: 'the hub refused' }; \
         document.getElementById('rules-btn').click(); return 'ok'; })()",
    )
    .await
    .expect("settings opens with a rule in it");
    settle().await;
    assert_eq!(
        b.text_of("String(document.querySelectorAll('#rules-list button').length)")
            .await
            .unwrap(),
        "1",
        "no Remove control was drawn, so nothing below was exercised"
    );

    let click = "document.querySelector('#rules-list button').click(); 'ok'";
    b.text_of(click).await.unwrap();
    settle().await;
    assert_eq!(
        b.text_of("document.getElementById('rule-error').textContent")
            .await
            .unwrap(),
        "the hub refused",
        "a refused removal said nothing"
    );

    b.text_of("window.__throw = {}; 'ok'").await.unwrap();
    b.text_of(click).await.unwrap();
    settle().await;
    assert_eq!(
        b.text_of("document.getElementById('rule-error').textContent")
            .await
            .unwrap(),
        "",
        "the panel still reports a failure that has since succeeded"
    );
}

/// Every transcript line is at least a line tall.
///
/// A row that is styled `white-space: nowrap; overflow: hidden` and laid out
/// as a block flex item can have its line box collapse to nothing, and then
/// it draws as a bar of padding with its text invisible. A content check
/// does not notice: the text is in the DOM, `textContent` is right, and only
/// the height is wrong. So height is what this reads.
///
/// Quantified over `Kind::ALL`, with the body long enough to clip; a short
/// body would fit whatever the layout did.
///
/// What this can and cannot see: the collapse it guards against (8px rows in
/// the shipped WebView2 window) does not reproduce in this harness's headless
/// Chromium, where the same rule with `display: block` put back still
/// measures a full line. Same engine version, different flow. This test
/// holds the property in the runtime it has; the page suite is not the
/// shipped runtime, and for anything that depends on layout rather than
/// logic, the window itself is the instrument.
#[tokio::test]
async fn every_transcript_line_is_at_least_a_line_tall() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    let long = "x".repeat(400);
    let mut short: Vec<String> = Vec::new();
    // Tool last, so a following progress or result renders as its own row
    // rather than folding into the step; see `appendChunk`.
    let mut order: Vec<openbot_app::Kind> = openbot_app::Kind::ALL.to_vec();
    order.sort_by_key(|k| k.as_str() == "tool");
    for kind in order {
        let name = kind.as_str();
        b.text_of(&format!(
            "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"{name}\",\"text\":\"{name} {long}\"}}); 'ok'"
        ))
        .await
        .expect("a chunk");
    }
    settle().await;
    let out = b
        .text_of(
            r#"(() => {
      const rows = [...document.querySelectorAll('#log .msg')];
      return JSON.stringify(rows.map(r => ({
        kind: [...r.classList].find(c => c !== 'msg'),
        h: Math.round(r.getBoundingClientRect().height),
        line: parseFloat(getComputedStyle(r).lineHeight) || 0
      })));
    })()"#,
        )
        .await
        .unwrap();
    for row in out.trim_matches(['[', ']']).split("},{") {
        let kind = row
            .split("\"kind\":\"")
            .nth(1)
            .and_then(|t| t.split('"').next())
            .unwrap_or("?");
        let h = field(row, "h");
        let line = row
            .split("\"line\":")
            .nth(1)
            .and_then(|t| t.split(&[',', '}'][..]).next())
            .and_then(|t| t.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        if h < line as i64 {
            short.push(format!("{kind} is {h}px tall for a {line}px line"));
        }
    }
    assert_eq!(
        out.matches("\"kind\":").count(),
        openbot_app::Kind::ALL.len(),
        "not every kind rendered a row: {out}"
    );
    assert!(
        short.is_empty(),
        "these transcript rows collapsed and their text is invisible: {short:?}"
    );
}

/// A tool call and its result are one row in the thread.
///
/// The shell sends the call and the result as two chunks. A thread that drew
/// both verbatim was a debug log: a mono box of arguments, then a line of
/// JSON under it, four rows per step. A step is now one row, and for a tool
/// the page knows the row reads as a sentence ("Wrote notes.md · 93 bytes")
/// with the raw record on its title. An unknown tool keeps its name and
/// arguments, so a new tool is shown rather than hidden.
///
/// The outcome still reaches a screen reader: the row gains a hidden
/// "Result:" span when the result lands, so the accessible text carries the
/// whole step even though the visible text was tidied.
#[tokio::test]
async fn a_tool_call_and_its_result_are_one_row() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let fire = |kind: &str, text: &str| {
        format!(
            "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"{kind}\",\"text\":{}}}); 'ok'",
            serde_json::to_string(text).unwrap()
        )
    };
    b.text_of(&fire(
        "tool",
        r#"fs.write {"path":"notes.md","contents":"hello"}"#,
    ))
    .await
    .unwrap();
    settle().await;
    let running = b
        .text_of("JSON.stringify({rows: document.querySelectorAll('#log .msg').length, state: document.querySelector('#log .msg.tool .step-mark')?.dataset.state, text: document.querySelector('#log .msg.tool .step-text')?.textContent})")
        .await
        .unwrap();
    assert!(
        running.contains("\"rows\":1"),
        "the call did not make one row: {running}"
    );
    assert!(
        running.contains("\"state\":\"running\""),
        "the step does not show as running: {running}"
    );
    assert!(
        running.contains("Wrote") && running.contains("notes.md"),
        "the step does not read as a sentence: {running}"
    );

    b.text_of(&fire("progress", "starting")).await.unwrap();
    b.text_of(&fire(
        "result",
        r#"✓ {"path":"notes.md","bytes_written":93}"#,
    ))
    .await
    .unwrap();
    settle().await;
    let done = b
        .text_of("JSON.stringify({rows: document.querySelectorAll('#log .msg').length, state: document.querySelector('#log .msg.tool .step-mark')?.dataset.state, text: document.querySelector('#log .msg.tool .step-text')?.textContent, sr: [...document.querySelectorAll('#log .msg.tool .sr-only')].map(e => e.textContent).join('|'), title: document.querySelector('#log .msg.tool')?.title})")
        .await
        .unwrap();
    assert!(
        done.contains("\"rows\":1"),
        "the result and progress added rows instead of completing the step: {done}"
    );
    assert!(
        done.contains("\"state\":\"ok\""),
        "the step did not turn to ok: {done}"
    );
    assert!(
        done.contains("93 bytes"),
        "the result's detail did not reach the row: {done}"
    );
    assert!(
        done.contains("Result:") && done.contains("bytes_written"),
        "a screen reader is not told the outcome: {done}"
    );
    assert!(
        done.contains("bytes_written\\\":93") || done.contains("bytes_written\":93"),
        "the raw record is not on the row's title: {done}"
    );

    // An unknown tool keeps its name and arguments visible.
    b.text_of(&fire("tool", r#"linear__create_issue {"title":"x"}"#))
        .await
        .unwrap();
    settle().await;
    let unknown = b
        .text_of("document.querySelectorAll('#log .msg.tool')[1]?.querySelector('.step-text')?.textContent")
        .await
        .unwrap();
    assert!(
        unknown.contains("linear__create_issue"),
        "an unknown tool lost its name: {unknown}"
    );

    // A failure shows its reason in the row, in the danger ink.
    b.text_of(&fire("result", r#"✗ {"error":"no such project"}"#))
        .await
        .unwrap();
    settle().await;
    let failed = b
        .text_of("JSON.stringify({state: document.querySelectorAll('#log .msg.tool')[1]?.querySelector('.step-mark')?.dataset.state, text: document.querySelectorAll('#log .msg.tool')[1]?.textContent})")
        .await
        .unwrap();
    assert!(
        failed.contains("\"state\":\"failed\""),
        "a failed step does not show as failed: {failed}"
    );
    assert!(
        failed.contains("no such project"),
        "the failure's reason is not in the row: {failed}"
    );
}

/// A tool call with a long argument still reads as a sentence.
///
/// The shell truncates a call's arguments to a readable length for `text`,
/// which leaves the JSON in it unparseable. The page summarised by parsing
/// that string, so an `fs.write` carrying a real file printed raw JSON while a
/// short `fs.read` beside it read as "Read notes.md" — the legibility of a
/// step depended on how much a Bot happened to be writing. The arguments now
/// arrive as data, so length has nothing to do with it.
#[tokio::test]
async fn a_long_argument_does_not_cost_a_step_its_summary() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // Longer than the shell's summary limit, so `text` is certainly truncated.
    let body = "x".repeat(400);
    let args = serde_json::json!({ "path": "notes.md", "contents": body });
    let truncated = format!(
        "fs.write {{\"path\":\"notes.md\",\"contents\":\"{}…",
        &body[..40]
    );
    b.text_of(&format!(
        "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"tool\",\"text\":{},\"args\":{}}}); 'ok'",
        serde_json::to_string(&truncated).unwrap(),
        args
    ))
    .await
    .unwrap();
    settle().await;

    let shown = b
        .text_of("document.querySelector('#log .msg.tool .step-text')?.textContent")
        .await
        .unwrap();
    assert!(
        shown.contains("Wrote") && shown.contains("notes.md"),
        "a long argument cost the step its summary: {shown}"
    );
    assert!(
        !shown.contains('{') && !shown.contains("contents"),
        "raw JSON leaked into a step that should read as a sentence: {shown}"
    );
}

/// A step's detail counts in words that match the number.
///
/// A workspace with one file in it is the ordinary case on a first run, so
/// "1 entries" is visible to everybody who tries the demo, and the root path
/// renders as a bare full stop that reads as a typo rather than as a place.
#[tokio::test]
async fn a_step_counts_in_words_that_match_the_number() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let step = |tool: &str, args: serde_json::Value, result: &str| {
        format!(
            "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"tool\",\"text\":{},\"args\":{}}});\
             window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"result\",\"text\":{}}}); 'ok'",
            serde_json::to_string(&format!("{tool} {args}")).unwrap(),
            args,
            serde_json::to_string(result).unwrap()
        )
    };

    b.text_of(&step(
        "fs.list",
        serde_json::json!({ "path": "." }),
        r#"✓ {"entries":[{"name":"notes.md"}]}"#,
    ))
    .await
    .unwrap();
    settle().await;
    let one = b
        .text_of("document.querySelector('#log .msg.tool .step-text')?.textContent")
        .await
        .unwrap();
    assert!(one.contains("1 entry"), "a count of one is plural: {one}");
    assert!(
        !one.contains("1 entries"),
        "a count of one is plural: {one}"
    );
    assert!(
        one.contains("the workspace"),
        "the root path renders as a bare full stop: {one}"
    );

    b.text_of(&step(
        "fs.write",
        serde_json::json!({ "path": "a.txt" }),
        r#"✓ {"bytes_written":1}"#,
    ))
    .await
    .unwrap();
    settle().await;
    let bytes = b
        .text_of("[...document.querySelectorAll('#log .msg.tool .step-text')].pop()?.textContent")
        .await
        .unwrap();
    assert!(bytes.contains("1 byte"), "{bytes}");
    assert!(!bytes.contains("1 bytes"), "{bytes}");
}

/// A long thread can still be scrolled back to its first line.
///
/// Short threads are pulled down to meet the composer so they do not sit
/// stranded above a screen of nothing. The obvious way to do that,
/// `justify-content: flex-end` on the scrolling column, makes the top of an
/// overflowing thread unreachable: the overflow goes off the top edge and no
/// amount of scrolling brings it back. An auto top margin on the first line
/// has the same effect when there is spare room and none when there is not.
#[tokio::test]
async fn the_start_of_a_long_thread_is_still_reachable() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        "window.__fire('chunk', {\"session\":\"s1\",\"kind\":\"user\",\"text\":\"FIRST LINE\"}); 'ok'",
    )
    .await
    .unwrap();
    for i in 0..40 {
        b.text_of(&format!(
            "window.__fire('chunk', {{\"session\":\"s1\",\"kind\":\"agent\",\"text\":\"filler {i}\"}}); 'ok'"
        ))
        .await
        .unwrap();
    }
    settle().await;

    let state = b
        .text_of(
            "(() => { const log = document.getElementById('log');\
               log.scrollTop = 0;\
               const first = log.querySelector('.msg');\
               const l = log.getBoundingClientRect(), f = first.getBoundingClientRect();\
               return JSON.stringify({\
                 overflows: log.scrollHeight > log.clientHeight + 4,\
                 top: Math.round(f.top - l.top),\
                 text: first.textContent.trim()\
               }); })()",
        )
        .await
        .unwrap();

    assert!(
        state.contains("\"overflows\":true"),
        "the thread did not overflow, so this proves nothing: {state}"
    );
    assert!(
        state.contains("FIRST LINE"),
        "the first line is not the first message: {state}"
    );
    // Scrolled to the very top, the first line has to be inside the box. A
    // negative offset means it is above the visible area with nowhere to
    // scroll to reach it.
    let top: i64 = state
        .split("\"top\":")
        .nth(1)
        .and_then(|t| t.split(',').next())
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(-9999);
    assert!(
        top >= -1,
        "scrolled to the top, the first line still sits {top}px above the thread: {state}"
    );
}

/// A key typed on the connect panel does not stay in the page.
///
/// It has been handed to the runtime by the time the workspace opens, so
/// nothing needs it in the DOM after that, and a password field that keeps its
/// value keeps it for anything that can read the page. The credential dialog
/// holds the same rule in `a_credential_is_never_left_in_the_window`; this is
/// the other field in this window that ever holds a secret.
#[tokio::test(flavor = "multi_thread")]
async fn a_model_key_is_not_left_in_the_connect_panel() {
    const VALUE: &str = "xai-not-a-real-key-9f2a";
    let Some((b, _p)) = page().await else { return };

    b.text_of(&format!(
        r#"(() => {{ window.__replies.connect = {{ computer: true, tools: 3 }};
             window.__replies.open_bot = null;
             window.__replies.roster = [];
             document.getElementById('model-id').value = 'grok-4-5';
             document.getElementById('model-key').value = {VALUE:?};
             document.getElementById('connect-btn').click();
             return 'ok'; }})()"#
    ))
    .await
    .expect("connect with a key");
    settle().await;

    // It reached the shell exactly once, as the value of the connect call.
    let sent = b
        .text_of("JSON.stringify(window.__sent('connect').map(c => c.args.model && c.args.model.apiKey))")
        .await
        .unwrap();
    assert!(
        sent.contains(VALUE),
        "the key should have been handed to the shell: {sent}"
    );

    assert_eq!(
        b.text_of("document.getElementById('model-key').value")
            .await
            .unwrap(),
        "",
        "the key is still sitting in the connect panel after connecting"
    );
}

/// The product wears the logo; a Bot still wears its own initial.
///
/// Both use `.mark`, so the risk in giving the product one an image was giving
/// every Bot the same image — which would delete the only per-Bot identity the
/// roster has, and do it silently, since a roster of identical marks still
/// renders and still lays out. The two halves are asserted together for that
/// reason: `.mark.product` carries artwork and no letter, `.mark.bot-mark`
/// carries a letter and no artwork.
#[tokio::test(flavor = "multi_thread")]
async fn the_product_mark_is_the_logo_and_a_bot_keeps_its_initial() {
    let Some((b, _p)) = page().await else { return };

    let product = b
        .text_of(
            "JSON.stringify([...document.querySelectorAll('.mark.product')].map(e => ({\
               text: e.textContent.trim(),\
               img: getComputedStyle(e).backgroundImage.slice(0, 22) })))",
        )
        .await
        .unwrap();
    assert!(
        product.contains(r#""img":"url(\"data:image/png"#),
        "the product mark is not showing the logo: {product}"
    );
    assert!(
        !product.contains(r#""text":"O""#),
        "the product mark still has the letterform in it: {product}"
    );
    // All three of them, not just whichever one happened to be on screen.
    assert_eq!(
        b.text_of("String(document.querySelectorAll('.mark.product').length)")
            .await
            .unwrap(),
        "3",
        "a product mark was left behind as a letter"
    );

    open_session(&b, "s1").await;
    b.text_of(
        r#"(() => { window.__replies.roster = [
             { id: "talent-scout", name: "Talent Scout", title: "", description: "",
               hidden: false, messages: 0 }];
             return 'ok'; })()"#,
    )
    .await
    .expect("a roster with one Bot");
    settle().await;

    let bot = b
        .text_of(
            "JSON.stringify([...document.querySelectorAll('.mark.bot-mark')].map(e => ({\
               text: e.textContent.trim(),\
               img: getComputedStyle(e).backgroundImage })))",
        )
        .await
        .unwrap();
    // Checked before the interesting assertion, because "no Bot mark carries
    // the logo" is trivially true of no Bot marks at all. Without this the test
    // would keep passing after any change that stopped the roster rendering.
    assert!(
        bot.contains(r#""text":"T""#),
        "no Bot mark with an initial was on screen, so the check below proves nothing: {bot}"
    );
    assert!(
        !bot.contains("data:image"),
        "a Bot's mark picked up the product logo, so every Bot now looks the same: {bot}"
    );
}

/// Keeping a credential is a choice, and the window has to make it only when
/// asked. A box that arrived ticked would decide for somebody on a machine they
/// might be sharing, and the deciding is the whole point of the box.
#[tokio::test]
async fn a_key_is_only_kept_when_the_window_was_asked_to_keep_it() {
    for (tick, want) in [("false", "[false]"), ("true", "[true]")] {
        let Some((b, _p)) = page().await else { return };
        b.text_of(&format!(
            r#"(() => {{ window.__replies.connect = {{ computer: true, tools: 3 }};
                 window.__replies.open_bot = null;
                 window.__replies.roster = [];
                 document.getElementById('model-id').value = 'grok-4-5';
                 document.getElementById('model-key').value = 'not-a-real-key';
                 document.getElementById('model-remember').checked = {tick};
                 document.getElementById('connect-btn').click();
                 return 'ok'; }})()"#
        ))
        .await
        .expect("connect");
        settle().await;

        let sent = b
            .text_of(
                "JSON.stringify(window.__sent('connect')\
                 .map(c => c.args.model && c.args.model.remember))",
            )
            .await
            .unwrap();
        assert_eq!(
            sent, want,
            "the window sent the wrong answer for `keep this key` when the box was {tick}"
        );
    }
}

/// The offer to keep a key closes along with the key box.
///
/// Offering to remember a credential that is not being collected is an offer to
/// store nothing, and a tick left sitting there says the opposite of what is
/// happening.
#[tokio::test]
async fn a_provider_that_wants_no_key_does_not_offer_to_keep_one() {
    let Some((b, _p)) = page().await else { return };
    b.text_of(
        "(() => { document.getElementById('model-remember').checked = true; \
         return 'ok'; })()",
    )
    .await
    .expect("tick it first");
    pick_provider(&b, "ollama").await;

    assert_eq!(
        b.text_of(
            "JSON.stringify([document.getElementById('model-remember').disabled,\
             document.getElementById('model-remember').checked])"
        )
        .await
        .unwrap(),
        "[true,false]",
        "the keep-this-key box is still live for a provider that takes no key"
    );
}

/// Pick a provider from the preset list and let the change handler run.
async fn pick_provider(b: &Browser, which: &str) {
    b.text_of(&format!(
        r#"(() => {{ const s = document.getElementById('model-preset');
             s.value = {which:?};
             s.dispatchEvent(new Event('change'));
             return 'ok'; }})()"#
    ))
    .await
    .expect("pick a provider");
}

/// Choosing a provider fills in the three things a person cannot be expected
/// to know: the dialect, the base URL, and the key variable's conventional
/// name. Getting any of them wrong fails in a way that reads like the product
/// is broken, and the only way to get them right was to already know them.
///
/// The list used to hold four hosted providers, each of which required going
/// away and opening an account before it did anything. A downloaded build now
/// ships with a model, so what is left is the one arrangement the shipped one
/// cannot offer — a model on this machine, where the work never leaves it.
#[tokio::test]
async fn picking_a_provider_fills_in_the_settings_nobody_should_have_to_look_up() {
    let Some((b, _p)) = page().await else { return };
    pick_provider(&b, "ollama").await;

    for (id, want) in [
        ("model-id", "qwen3:1.7b"),
        ("model-dialect", "openai"),
        ("model-base", "http://localhost:11434/v1"),
    ] {
        assert_eq!(
            b.text_of(&format!("document.getElementById({id:?}).value"))
                .await
                .unwrap(),
            want,
            "the preset did not fill {id}"
        );
    }
}

/// Choosing the built-in model clears the fields, rather than leaving them.
///
/// The runtime falls back to the model the build ships with only when nothing
/// is configured. So "built in" has to actually empty the boxes: a half-typed
/// model id left behind would go on overriding the thing the person had just
/// selected, and the window would say one model while the runtime used another.
#[tokio::test]
async fn choosing_the_built_in_model_clears_what_would_override_it() {
    let Some((b, _p)) = page().await else { return };

    // Somewhere else first, so there is something to clear.
    pick_provider(&b, "ollama").await;
    assert_ne!(
        b.text_of("document.getElementById('model-id').value")
            .await
            .unwrap(),
        "",
        "nothing was filled in, so clearing it proves nothing"
    );

    pick_provider(&b, "").await;
    for id in ["model-id", "model-dialect", "model-base", "model-key-env"] {
        assert_eq!(
            b.text_of(&format!("document.getElementById({id:?}).value"))
                .await
                .unwrap(),
            "",
            "{id} still holds a value that would override the built-in model"
        );
    }
}

/// A provider that takes no credential closes the key box rather than leaving
/// it enabled and inert.
///
/// An enabled field next to an endpoint that ignores keys invites somebody to
/// paste a real one where it was never needed, and then to believe it mattered.
#[tokio::test]
async fn a_provider_that_wants_no_key_closes_the_key_box_and_says_why() {
    let Some((b, _p)) = page().await else { return };
    pick_provider(&b, "ollama").await;

    assert_eq!(
        b.text_of("String(document.getElementById('model-key').disabled)")
            .await
            .unwrap(),
        "true",
        "the key box is still accepting a credential the provider will ignore"
    );
    assert_eq!(
        b.text_of(
            "String(document.getElementById('model-hint-keyless')\
             .classList.contains('hidden'))"
        )
        .await
        .unwrap(),
        "false",
        "nothing on screen explains that no key is needed"
    );
    assert_eq!(
        b.text_of(
            "String(document.getElementById('model-hint-key')\
             .classList.contains('hidden'))"
        )
        .await
        .unwrap(),
        "true",
        "the panel still warns about protecting a key that will never exist"
    );
}

/// The empty key variable has to survive the trip to the shell.
///
/// This is the one field where empty is a *meaning* — "this endpoint wants no
/// credential" — rather than "not filled in". Every other layer treats an empty
/// string as absent, so the natural thing for each of them to do is drop it,
/// and dropping it leaves a local model being asked for a key that does not
/// exist. Asserting on the exact value rather than on the connect succeeding,
/// because a connect that keeps the previous key variable also succeeds.
#[tokio::test]
async fn choosing_a_local_provider_tells_the_runtime_that_no_key_is_wanted() {
    let Some((b, _p)) = page().await else { return };
    pick_provider(&b, "ollama").await;

    b.text_of(
        r#"(() => { window.__replies.connect = { computer: true, tools: 3 };
             window.__replies.open_bot = null;
             window.__replies.roster = [];
             document.getElementById('connect-btn').click();
             return 'ok'; })()"#,
    )
    .await
    .expect("connect");
    settle().await;

    let sent = b
        .text_of(
            "JSON.stringify(window.__sent('connect')\
             .map(c => c.args.model && c.args.model.apiKeyEnv))",
        )
        .await
        .unwrap();
    assert_eq!(
        sent, r#"[""]"#,
        "the `no key wanted` choice did not reach the shell: {sent}"
    );
}

/// An ask offering a larger grant than the one it leads with, so a test can
/// tell "took the narrowest" apart from "took the first that worked".
fn ask_with_session_grant(id: &str, session: &str) -> String {
    format!(
        r#"{{"id":"{id}","session":"{session}","tool":"shell.exec: runs a command",
            "fields":[{{"name":"command","value":"rm -rf build","long":false}}],
            "options":[{{"id":"allow-once","name":"Allow once","kind":"allow_once",
                         "danger":false}},
                       {{"id":"allow-session","name":"Allow for the rest of this session",
                         "kind":"allow_always","danger":false}},
                       {{"id":"reject-once","name":"Not this time","kind":"reject_once",
                         "danger":true}}]}}"#
    )
}

/// Bypass is off when the window opens, and the dialog still gates.
///
/// The whole approval suite is written against a window that asks. If bypass
/// ever defaulted on, every one of those tests would still pass — they answer
/// the dialog, and there would be no dialog to answer — while the product
/// silently approved everything. So the default is asserted on its own.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_starts_by_asking_and_not_by_approving() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    assert_eq!(
        b.text_of("document.getElementById('bypass').getAttribute('aria-pressed')")
            .await
            .unwrap(),
        "false",
        "bypass must be off when the window opens"
    );

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "shell.exec")
    ))
    .await
    .expect("fire the ask");
    settle().await;

    assert_eq!(
        b.text_of("String(!document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "with bypass off the request must still reach a person"
    );
    assert_eq!(
        b.text_of("String(window.__sent('answer_permission').length)")
            .await
            .unwrap(),
        "0",
        "nothing may be answered before a person answers it"
    );
}

/// With bypass on, the ask is answered with the *narrowest* grant.
///
/// This is the one way this feature could weaken the gate. `allow_once` is a
/// client-side convenience; `allow_always` is a hub-side session grant that
/// outlives the toggle, so a bypass that reached for it would quietly widen
/// what the hub permits for the rest of the session. The ask here offers both,
/// so taking the wrong one is visible.
#[tokio::test(flavor = "multi_thread")]
async fn bypass_takes_the_narrowest_grant_and_never_the_session_one() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of("document.getElementById('bypass').click(); 'ok'")
        .await
        .expect("turn bypass on");

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask_with_session_grant("a1", "s1")
    ))
    .await
    .expect("fire the ask");
    settle().await;

    let sent = b
        .text_of("JSON.stringify(window.__sent('answer_permission').map(c=>c.args.optionId))")
        .await
        .unwrap();
    assert!(
        sent.contains("allow-once"),
        "bypass should have answered with the narrowest grant: {sent}"
    );
    assert!(
        !sent.contains("allow-session"),
        "bypass must never take the session-wide grant: {sent}"
    );

    assert_eq!(
        b.text_of("String(!document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "false",
        "the dialog should not have been shown at all"
    );

    // The choice is gone; the account of it is not.
    let note = b
        .text_of("document.querySelector('.msg.auto-approved').textContent")
        .await
        .expect("the auto-approval should be recorded in the log");
    for needle in ["Approved without asking", "shell.exec", "rm -rf build"] {
        assert!(
            note.contains(needle),
            "the record should carry {needle:?}: {note}"
        );
    }
}

/// A credential request is never auto-answered.
///
/// It asks for a value, not for a choice. A bypass has no value to give, and
/// answering it with an option id would hand the Bot an empty credential while
/// telling it the person had supplied one.
#[tokio::test(flavor = "multi_thread")]
async fn bypass_does_not_answer_a_request_for_a_credential() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of("document.getElementById('bypass').click(); 'ok'")
        .await
        .expect("turn bypass on");

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        secret_ask("s-1", "s1")
    ))
    .await
    .expect("fire the credential ask");
    settle().await;

    assert_eq!(
        b.text_of("String(!document.getElementById('dialog').classList.contains('hidden'))")
            .await
            .unwrap(),
        "true",
        "a credential request must still stop and ask, even with bypass on"
    );
    assert_eq!(
        b.text_of("String(window.__sent('answer_permission').length)")
            .await
            .unwrap(),
        "0",
        "a credential must not be answered by a bypass"
    );
}

/// A remembered bypass is on from the first frame, and says so.
///
/// This is the condition on persisting it at all. It may be on before anybody
/// clicks; it may never be on without the window saying so. A stored flag that
/// applied silently would be the thing the approval dialog exists to prevent,
/// arrived at from the other direction.
#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_bypass_is_visible_before_anything_is_clicked() {
    let Some((b, _p)) = page().await else { return };

    // Store the choice and reload, which is what a next launch looks like.
    b.text_of("localStorage.setItem('openbot.bypass', '1'); 'ok'")
        .await
        .expect("remember it");
    b.text_of("location.reload(); 'ok'").await.ok();
    // Waited for, not slept through. A reload under a loaded suite takes longer
    // than it does alone, and two `settle()`s passed in isolation and failed in
    // the full run — the exact race MEMORY.md already warns about.
    wait_until(
        &b,
        "String(document.getElementById('bypass').getAttribute('aria-pressed') === 'true')",
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(
        b.text_of("document.getElementById('bypass').getAttribute('aria-pressed')")
            .await
            .unwrap(),
        "true",
        "a remembered bypass should be in force at load"
    );
    assert_eq!(
        b.text_of("document.getElementById('bypass-label').textContent")
            .await
            .unwrap(),
        "Approving everything",
        "and it must say so, not sit on silently"
    );
    assert!(
        b.text_of("document.getElementById('bypass').className")
            .await
            .unwrap()
            .contains("bypassing"),
        "the toggle should be wearing the amber that means a person is being skipped"
    );
}

/// Turning it off is remembered too.
///
/// A one-way memory would be worse than none: somebody who switched it off
/// would find it back on at the next launch, having been told it was off.
#[tokio::test(flavor = "multi_thread")]
async fn turning_bypass_off_is_remembered_as_well() {
    let Some((b, _p)) = page().await else { return };

    b.text_of("localStorage.setItem('openbot.bypass', '1'); 'ok'")
        .await
        .expect("remember it on");
    b.text_of("location.reload(); 'ok'").await.ok();
    wait_until(
        &b,
        "String(document.getElementById('bypass').getAttribute('aria-pressed') === 'true')",
        Duration::from_secs(10),
    )
    .await;

    b.text_of("document.getElementById('bypass').click(); 'ok'")
        .await
        .expect("turn it off");
    wait_until(
        &b,
        "String(localStorage.getItem('openbot.bypass') === '0')",
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(
        b.text_of("localStorage.getItem('openbot.bypass')")
            .await
            .unwrap(),
        "0",
        "switching it off must be what is remembered"
    );
}

/// A finished step says how long it took.
///
/// The first thing anyone watching a Bot work wants to know, and the window had
/// no answer anywhere. A slow step and a stuck one looked identical, which is
/// the worst possible ambiguity on the one surface where a person is deciding
/// whether to intervene.
///
/// The runtime does measure this and puts it on `ToolCallFinished`; the window
/// drives the CLI over ACP, whose tool-call update carries no such field, so
/// the number dies at that boundary. What is asserted here is the window's own
/// observation, which on a local transport differs by well under a millisecond.
#[tokio::test]
async fn a_finished_step_says_how_long_it_took() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"tool", text:"fs.read {}", args:{path:"notes.md"}}); 'ok'"#,
    )
    .await
    .expect("the call opens a step");

    // A gap the assertion can see. Without it a sub-millisecond step would
    // round to "0ms" and pass while proving nothing about the measurement.
    tokio::time::sleep(Duration::from_millis(120)).await;

    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"result", text:"✓ {\"contents\":\"hi\"}"}); 'ok'"#,
    )
    .await
    .expect("the result completes it");

    let shown = b
        .text_of("document.querySelector('.msg.tool .step-dur').textContent")
        .await
        .expect("the step carries a duration");
    assert!(
        shown.ends_with("ms") || shown.ends_with('s'),
        "a duration has to read as a duration, got {shown:?}"
    );
    let ms: f64 = shown
        .trim_end_matches("ms")
        .parse()
        .unwrap_or_else(|_| panic!("expected milliseconds for a step this short, got {shown:?}"));
    assert!(
        (100.0..2000.0).contains(&ms),
        "the window measured {ms}ms for a step it held open for ~120ms, so it is not timing the \
         step at all"
    );
}

/// The log follows new lines only when the reader is already at the bottom.
///
/// It used to jump to the bottom on every arriving line, which is exactly wrong
/// while a Bot is working: the moment somebody scrolls up to read what a step
/// did, the next step yanks them back down. The log became unreadable precisely
/// when it had something worth reading.
#[tokio::test]
async fn the_log_does_not_yank_you_back_down_while_you_are_reading() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // Enough to overflow, so there is somewhere to scroll away to.
    for i in 0..40 {
        b.text_of(&format!(
            r#"window.__fire('chunk', {{session:"s1", kind:"agent", text:"line {i}"}}); 'ok'"#
        ))
        .await
        .expect("fill the log");
    }
    let overflows = b
        .text_of("String(document.getElementById('log').scrollHeight > document.getElementById('log').clientHeight + 100)")
        .await
        .unwrap_or_default();
    assert_eq!(
        overflows, "true",
        "the log does not overflow, so scrolling away from the bottom proves nothing"
    );

    // Deliberately away from the bottom, the way a person reading back is.
    b.text_of("document.getElementById('log').scrollTop = 0; 'ok'")
        .await
        .expect("scroll up");
    b.text_of(r#"window.__fire('chunk', {session:"s1", kind:"agent", text:"new line"}); 'ok'"#)
        .await
        .expect("a line arrives");

    let top = b
        .text_of("String(Math.round(document.getElementById('log').scrollTop))")
        .await
        .unwrap_or_default();
    assert_eq!(
        top, "0",
        "a new line dragged the reader back to the bottom; they were at the top reading"
    );

    // And the anti-vacuity half: a reader who IS at the bottom must still be
    // carried along, or this "fix" is just a broken log.
    b.text_of("document.getElementById('log').scrollTop = document.getElementById('log').scrollHeight; 'ok'")
        .await
        .expect("back to the bottom");
    let before = b
        .text_of("String(Math.round(document.getElementById('log').scrollTop))")
        .await
        .unwrap_or_default();
    b.text_of(r#"window.__fire('chunk', {session:"s1", kind:"agent", text:"another line"}); 'ok'"#)
        .await
        .expect("another line arrives");
    let after = b
        .text_of("String(Math.round(document.getElementById('log').scrollTop))")
        .await
        .unwrap_or_default();
    assert_ne!(
        before, after,
        "a reader already at the bottom stopped being followed, which is not a fix, it is a \
         different bug"
    );
}

/// A step's raw record can be opened, read, and selected.
///
/// It used to live only on the element's `title`. A tooltip cannot be selected,
/// cannot be copied, and disappears the moment the pointer moves - which makes
/// it the wrong home for the one piece of text somebody debugging a run
/// actually needs. The row now opens.
///
/// The head is a real `<button>` with `aria-expanded` rather than a div with a
/// click handler, so the row is reachable by keyboard and says what it does.
#[tokio::test]
async fn a_step_opens_to_show_the_record_behind_it() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"tool", text:"fs.read {\"path\":\"notes.md\"}", args:{path:"notes.md"}}); 'ok'"#,
    )
    .await
    .expect("a call");
    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"result", text:"✓ {\"contents\":\"the-actual-bytes\"}"}); 'ok'"#,
    )
    .await
    .expect("its result");

    // Closed to begin with: the record is context, not the conversation.
    let before = b
        .text_of("String(document.querySelectorAll('#log .step-raw').length)")
        .await
        .unwrap_or_default();
    assert_eq!(
        before, "0",
        "the record was open before anybody asked for it"
    );
    let collapsed = b
        .text_of("document.querySelector('#log .step-head').getAttribute('aria-expanded')")
        .await
        .unwrap_or_default();
    assert_eq!(collapsed, "false", "a closed row must say it is closed");

    b.click(".step-head").await.expect("open the row");

    let raw = b
        .text_of("document.querySelector('#log .step-raw').textContent")
        .await
        .expect("the record is on screen");
    assert!(
        raw.contains("notes.md") && raw.contains("the-actual-bytes"),
        "the record must carry both the call and its result, got {raw:?}"
    );
    let expanded = b
        .text_of("document.querySelector('#log .step-head').getAttribute('aria-expanded')")
        .await
        .unwrap_or_default();
    assert_eq!(expanded, "true", "an open row must say it is open");

    // Selectable, which is the entire point of moving it off `title`.
    let selectable = b
        .text_of("getComputedStyle(document.querySelector('#log .step-raw')).userSelect")
        .await
        .unwrap_or_default();
    assert_ne!(
        selectable, "none",
        "the record cannot be selected, so it cannot be pasted into an issue"
    );

    // And it closes again, or it is a disclosure that only discloses.
    b.click(".step-head").await.expect("close the row");
    let after = b
        .text_of("String(document.querySelectorAll('#log .step-raw').length)")
        .await
        .unwrap_or_default();
    assert_eq!(after, "0", "the row would not close again");
}

/// The row is reachable without a mouse.
///
/// A disclosure that only opens on click is a disclosure half the people using
/// this cannot open at all. Asserted separately from the behaviour above
/// because "it works with a pointer" and "it exists in the tab order" fail
/// independently, and the second is the one that silently regresses.
#[tokio::test]
async fn a_step_row_can_be_opened_from_the_keyboard() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    b.text_of(
        r#"window.__fire('chunk', {session:"s1", kind:"tool", text:"fs.list {}", args:{path:"."}}); 'ok'"#,
    )
    .await
    .expect("a call");

    let tag = b
        .text_of("document.querySelector('#log .step-head').tagName")
        .await
        .unwrap_or_default();
    assert_eq!(
        tag, "BUTTON",
        "the row is not a button, so it is not in the tab order and Enter does nothing"
    );

    b.text_of("document.querySelector('#log .step-head').focus(); 'ok'")
        .await
        .expect("focus it");
    let focused = b
        .text_of("document.activeElement.className")
        .await
        .unwrap_or_default();
    assert!(
        focused.contains("step-head"),
        "the row would not take focus, got {focused:?}"
    );

    b.key("Enter").await.expect("press it");
    let open = b
        .text_of("String(document.querySelectorAll('#log .step-raw').length)")
        .await
        .unwrap_or_default();
    assert_eq!(open, "1", "Enter on a focused row did not open its record");
}

/// The chrome says how many approvals are blocking a person.
///
/// The dialog is only visible on the conversation that raised it, so a person
/// who stepped away, or who is reading a different Bot, has nothing telling
/// them the app is blocked. `DIRECTION.md` pairs the inline gate with a
/// persistent count for exactly that reason.
///
/// Absent rather than zero when nothing is waiting. Amber is the only warm
/// colour in this product and it means "a person is blocking progress"; a
/// standing "0 waiting" would put that colour on screen permanently and drain
/// the meaning out of it, which is the failure `DIRECTION.md` calls a bug by
/// name.
#[tokio::test]
async fn the_chrome_says_how_many_approvals_are_waiting() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let hidden = b
        .text_of("String(document.getElementById('waiting').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        hidden, "true",
        "the count is on screen with nothing waiting, so amber means nothing"
    );

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "shell.exec")
    ))
    .await
    .expect("one approval");
    settle().await;

    let one = b
        .text_of("document.getElementById('waiting').textContent")
        .await
        .unwrap_or_default();
    assert_eq!(
        one, "1 waiting on you",
        "one approval must not be announced as a plural"
    );

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a2", "s1", "fs.write")
    ))
    .await
    .expect("a second approval");
    settle().await;
    let two = b
        .text_of("document.getElementById('waiting').textContent")
        .await
        .unwrap_or_default();
    assert_eq!(
        two, "2 waiting on you",
        "the count did not follow the queue"
    );
}

/// And it goes away again when the queue empties.
///
/// The failure this rules out is worse than not having a count: one that only
/// counts up goes stale pointing at work already done, and the first time
/// somebody looks and finds nothing there, they stop believing it. Answering an
/// approval is the ordinary path, so it is the one asserted.
#[tokio::test]
async fn the_waiting_count_clears_when_the_queue_does() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(&format!(
        "window.__fire('permission-request', {}); 'ok'",
        ask("a1", "s1", "fs.write")
    ))
    .await
    .expect("an approval");
    settle().await;
    let shown = b
        .text_of("String(document.getElementById('waiting').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        shown, "false",
        "the count never appeared, so clearing it proves nothing"
    );

    // Answered the way a person answers it.
    // `.danger` is the refusal in both shapes the dialog takes: the agent's own
    // decline option when it offers one, and the "Refuse" the window adds when
    // it does not. Matching on the word "Refuse" only finds the second.
    b.click("#dialog-options .danger").await.expect("refuse it");
    settle().await;

    let cleared = b
        .text_of("String(document.getElementById('waiting').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        cleared, "true",
        "the count still claims something is waiting after the queue emptied"
    );
}

/// A runtime that has died stops reading as connected.
///
/// `openbot acp` is a child process, and this window keeps its whole side of
/// the connection whether or not anything is still on the other end. Before
/// this, a dead one was indistinguishable from a working one: the pill said
/// connected for as long as the window stayed open, the composer took a
/// message, and the only sign was a protocol error after somebody had written
/// a paragraph and pressed Send.
///
/// The poll is left to fire on its own clock rather than called directly. What
/// is in doubt is not whether the function works — it is whether anything ever
/// calls it, which is exactly what a direct call would paper over.
#[tokio::test]
async fn a_dead_runtime_stops_reading_as_connected() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    let before = b
        .text_of("document.getElementById('status').textContent")
        .await
        .unwrap_or_default();
    assert_eq!(
        before, "connected",
        "the window did not start out connected, so nothing below is about a death"
    );

    b.text_of("window.__replies.runtime_alive = false; 'ok'")
        .await
        .expect("the runtime dies");

    // Generous against the three-second poll: this asserts the behaviour, not
    // the clock, and a loaded runner must not fail it for being slow.
    let noticed = wait_until(
        &b,
        "String(!document.getElementById('runtime-gone').classList.contains('hidden'))",
        Duration::from_secs(15),
    )
    .await;
    assert!(
        noticed,
        "the runtime has been gone for fifteen seconds and the window has not said so"
    );

    let pill = b
        .text_of("document.getElementById('status').textContent")
        .await
        .unwrap_or_default();
    assert_eq!(
        pill, "the runtime stopped",
        "the banner appeared but the pill still claims a connection, which is the lie this fixes"
    );

    let send = b
        .text_of("String(document.getElementById('send').disabled)")
        .await
        .unwrap_or_default();
    assert_eq!(
        send, "true",
        "Send is still live over a runtime that is gone, so pressing it fails after the writing"
    );

    // And the half that keeps the fix from being its own bug: the words
    // somebody was part-way through must survive, which is why the box is not
    // disabled with the button.
    let typing = b
        .text_of(
            "(() => { const i = document.getElementById('input');
                      i.value = 'half a paragraph';
                      return String(!i.disabled && i.value === 'half a paragraph'); })()",
        )
        .await
        .unwrap_or_default();
    assert_eq!(
        typing, "true",
        "the composer was disabled along with Send, which throws away what was being written"
    );

    // And the recovery, because a window that says what went wrong and cannot
    // come back from it has only moved the dead end. Reconnect is the banner's
    // one action; it leads to the connect panel, and connecting again must
    // undo every part of this state rather than the visible part of it.
    b.text_of(
        "window.__replies.runtime_alive = true;
         document.getElementById('runtime-reconnect').click(); 'ok'",
    )
    .await
    .expect("reconnect");
    let back = wait_until(
        &b,
        "String(!document.getElementById('connect').classList.contains('hidden'))",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        back,
        "Reconnect did not lead anywhere a person can reconnect from"
    );

    b.text_of("document.getElementById('connect-btn').click(); 'ok'")
        .await
        .expect("connect again");
    let recovered = wait_until(
        &b,
        "String(!document.getElementById('send').disabled
                 && document.getElementById('runtime-gone').classList.contains('hidden'))",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        recovered,
        "the window reconnected still showing the last runtime's death, with Send dead; one crash          would mute it for the rest of the session"
    );
}

/// Disconnecting is not reported back to you as a crash.
///
/// The backend cannot tell "the runtime died" from "there is no runtime": both
/// answer `runtime_alive` false, which is the right answer to the question
/// asked and the wrong thing to put on screen after somebody clicks
/// Disconnect. The poll is cleared before the call that empties the engine, and
/// this is the test that keeps those two statements in that order.
///
/// It is the more valuable of the pair. The death path announces itself; this
/// one would have shipped as a crash banner nobody could explain, on the most
/// ordinary action in the window.
#[tokio::test]
async fn disconnecting_is_not_reported_as_a_crash() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    // A slow `disconnect`, deliberately, because the bug lives entirely inside
    // the time that call takes. An instant one closes the window the poll
    // would have ticked in, and the test passes with the two statements in
    // either order — which is to say it tests nothing. Four seconds is longer
    // than the three-second poll, so an interval cleared after the call rather
    // than before it has fired at least once by the time this returns.
    //
    // Not a contrivance: `disconnect` drops the viewer and the computer this
    // window started, and waits on the children it is killing.
    b.text_of(
        "window.__replies.runtime_alive = false;
         window.__replies.disconnect = () => new Promise((r) => setTimeout(r, 4000));
         document.getElementById('disconnect').click(); 'ok'",
    )
    .await
    .expect("disconnect");

    // The assertion is that nothing happens, and there is no condition to poll
    // for; a fixed wait is the only shape that has.
    tokio::time::sleep(Duration::from_millis(4500)).await;

    let banner = b
        .text_of("String(document.getElementById('runtime-gone').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        banner, "true",
        "clicking Disconnect put a crash banner on screen, blaming the runtime for the click"
    );

    let pill = b
        .text_of("document.getElementById('status').textContent")
        .await
        .unwrap_or_default();
    assert_ne!(
        pill, "the runtime stopped",
        "the pill reports a deliberate disconnect as a runtime that died"
    );
}

/// A turn that fails because the runtime died says so, not what the wire said.
///
/// A dead runtime reaches the turn as whatever the transport happened to
/// report — the literal string is "openbot acp is gone" — and that went into
/// the problem banner verbatim: a component a person does not have, and
/// nothing they can do about it. The poll would arrive at the truth within
/// three seconds, which is three seconds of somebody reading a protocol error
/// and drawing their own conclusions from it.
///
/// So the failure path asks outright before it reports. No pattern-matching on
/// error text, which would break the first time the transport reworded itself.
#[tokio::test]
async fn a_turn_that_fails_over_a_dead_runtime_says_the_runtime_died() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        "window.__throw = { prompt: 'openbot acp is gone' };
         window.__replies.runtime_alive = false;
         document.getElementById('input').value = 'do the thing';
         document.getElementById('composer').requestSubmit(); 'ok'",
    )
    .await
    .expect("a turn that fails");

    let said = wait_until(
        &b,
        "String(!document.getElementById('runtime-gone').classList.contains('hidden'))",
        Duration::from_secs(10),
    )
    .await;
    assert!(
        said,
        "the turn failed over a runtime that is gone and the window did not say the runtime is gone"
    );

    let generic = b
        .text_of("String(document.getElementById('problem').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        generic, "true",
        "both banners are up, so the window says it twice and disagrees with itself once"
    );
}

/// And a turn that fails for any other reason still says what it was told.
///
/// The overreach half. Routing every failed turn to "the runtime stopped"
/// would be a worse bug than the one above: a model that refused, a workspace
/// that could not be read, a tool that errored — all of them would come back
/// as a runtime crash, and the record of what actually happened would be gone
/// from a window whose whole job is showing it.
#[tokio::test]
async fn a_turn_that_fails_for_any_other_reason_still_says_what_it_was_told() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;

    b.text_of(
        "window.__throw = { prompt: 'the model refused' };
         document.getElementById('input').value = 'do the thing';
         document.getElementById('composer').requestSubmit(); 'ok'",
    )
    .await
    .expect("a turn that fails");

    let said = wait_until(
        &b,
        "String(!document.getElementById('problem').classList.contains('hidden'))",
        Duration::from_secs(10),
    )
    .await;
    assert!(said, "a failed turn was not reported at all");

    let raw = b
        .text_of("document.getElementById('problem-raw').textContent")
        .await
        .unwrap_or_default();
    assert!(
        raw.contains("the model refused"),
        "the record of what went wrong is not in the banner, got {raw:?}"
    );

    let blamed = b
        .text_of("String(document.getElementById('runtime-gone').classList.contains('hidden'))")
        .await
        .unwrap_or_default();
    assert_eq!(
        blamed, "true",
        "a live runtime was blamed for a failure that had nothing to do with it"
    );
}

/// Fills the roster with `n` Bots and waits for them to be on screen.
///
/// Enough of them to overflow the sidebar, which is the only condition under
/// which "the list kept its scroll position" means anything.
async fn roster_of(b: &Browser, n: usize) {
    b.text_of(&format!(
        "window.__replies.roster = Array.from({{ length: {n} }}, (_, i) => ({{
           id: 'bot-' + i, name: 'Bot ' + i, title: '', description: '',
           hidden: false, messages: 0 }}));
         refreshRoster(); 'ok'"
    ))
    .await
    .expect("a big roster");
    let drawn = wait_until(
        b,
        &format!("String(document.querySelectorAll('#bots .bot').length === {n})"),
        Duration::from_secs(10),
    )
    .await;
    assert!(drawn, "the roster never drew {n} Bots");
}

/// The roster does not take focus off the row you were on.
///
/// It redraws at the end of every turn, and it used to redraw by emptying the
/// list and building it again. Destroying the focused element sends focus to
/// `<body>`, so anybody moving through the roster from the keyboard was
/// returned to the start of the document every time any Bot finished
/// speaking — an interruption a pointer user never sees and a keyboard user
/// cannot avoid.
///
/// Asserted by node identity, not by text. The first version of this compared
/// `document.activeElement.textContent`, which on `<body>` is the text of the
/// entire document: it matched the row's name perfectly well after focus had
/// been thrown to the body, and passed against the very code it was written to
/// condemn.
#[tokio::test]
async fn the_roster_does_not_take_focus_off_the_row_you_were_on() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    roster_of(&b, 20).await;

    const ON_THE_ROW: &str =
        "String(document.activeElement === document.querySelectorAll('#bots .bot')[7])";

    b.text_of("document.querySelectorAll('#bots .bot')[7].focus(); 'ok'")
        .await
        .expect("focus a row");
    let held = b.text_of(ON_THE_ROW).await.unwrap_or_default();
    assert_eq!(
        held, "true",
        "focus did not land on the row, so losing it would prove nothing"
    );

    b.text_of("refreshRoster(); 'ok'").await.expect("redraw");
    settle().await;

    let still = b.text_of(ON_THE_ROW).await.unwrap_or_default();
    let landed = b
        .text_of("document.activeElement.tagName")
        .await
        .unwrap_or_default();
    assert_eq!(
        still, "true",
        "the redraw took focus off the row somebody was on; it is on <{landed}> now"
    );
}

/// A roster that keeps its rows still has to change when the roster does.
///
/// The anti-vacuity half, and the one that would catch the reconcile going
/// wrong. A list that never updates keeps its scroll and its focus perfectly,
/// and the two tests above would pass over it: added, removed, renamed and
/// reordered Bots all have to land, and the row that is open has to keep
/// saying so.
#[tokio::test]
async fn a_roster_that_keeps_its_rows_still_shows_what_changed() {
    let Some((b, _p)) = page().await else { return };
    open_session(&b, "s1").await;
    roster_of(&b, 4).await;

    // Reordered, one gone, one renamed, one gaining history.
    b.text_of(
        "window.__replies.roster = [
           { id: 'bot-3', name: 'Bot 3', title: '', description: '', hidden: false, messages: 9 },
           { id: 'bot-1', name: 'Bot 1', title: 'keeps the books', description: '',
             hidden: false, messages: 0 },
           { id: 'bot-0', name: 'Renamed', title: '', description: '', hidden: true, messages: 0 },
         ];
         refreshRoster(); 'ok'",
    )
    .await
    .expect("the roster changes");
    settle().await;

    let order = b
        .text_of(
            "[...document.querySelectorAll('#bots .bot-name')].map(n => n.textContent).join('|')",
        )
        .await
        .unwrap_or_default();
    assert_eq!(
        order, "Bot 3|Bot 1|Renamed",
        "the reconcile did not reorder, remove and rename the rows it was given"
    );

    let subs = b
        .text_of(
            "[...document.querySelectorAll('#bots .bot-sub')].map(n => n.textContent).join('|')",
        )
        .await
        .unwrap_or_default();
    assert_eq!(
        subs, "9 messages|keeps the books|no messages yet",
        "a row kept the line it was drawn with instead of the one it was given"
    );

    let hidden = b
        .text_of("String(document.querySelectorAll('#bots .bot-hidden').length)")
        .await
        .unwrap_or_default();
    assert_eq!(
        hidden, "1",
        "the hidden tag did not follow the Bot that became hidden"
    );

    // And the open Bot still reads as open, which is the one piece of state
    // the row carries that nothing else on screen repeats.
    b.text_of(
        "window.__replies.roster = [
           { id: 'talent-scout', name: 'Talent Scout', title: '', description: '',
             hidden: false, messages: 0 }];
         refreshRoster(); 'ok'",
    )
    .await
    .expect("back to the open Bot");
    settle().await;
    let open = b
        .text_of("String(document.querySelectorAll('#bots .bot.open').length)")
        .await
        .unwrap_or_default();
    assert_eq!(
        open, "1",
        "the row for the conversation that is open stopped saying so, so the sidebar no longer \
         agrees with the pane beside it"
    );
}
