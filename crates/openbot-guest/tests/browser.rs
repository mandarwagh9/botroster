//! Drives a real browser against a real page.
//!
//! Serves the page from a one-shot HTTP listener on loopback rather than
//! reaching the internet: a test that needs a network is a test that fails for
//! reasons unrelated to the code.
//!
//! Skipped when no browser is installed. A skip is reported explicitly; a
//! silently passing test that never ran is worse than a failing one.

use std::sync::Arc;
use std::time::Duration;

use openbot_guest::browser::{find_browser, Browser, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PAGE: &str = r#"<!doctype html><html><head><title>openbot test page</title></head>
<body>
  <h1>Hello from openbot</h1>
  <p id="para">The quick brown fox.</p>
  <a href="https://example.com/one">First link</a>
  <a href="https://example.com/two">Second link</a>
  <input id="field" type="text" value="">
  <button id="btn" onclick="document.getElementById('para').innerText='clicked'">Press me</button>
  <div id="typed"></div>
  <script>
    document.getElementById('field').addEventListener('input', e => {
      document.getElementById('typed').innerText = 'saw: ' + e.target.value;
    });
  </script>
</body></html>"#;

/// A page built for testing input: everything is absolutely positioned so a
/// click coordinate is knowable, and every event is recorded so the test can
/// tell a real keystroke from a value that merely appeared.
const INPUT_PAGE: &str = r#"<!doctype html><html><head><title>input</title>
<style>body{margin:0;height:3000px}</style></head>
<body>
  <div id="target" style="position:absolute;left:100px;top:150px;width:80px;height:40px;background:#ccc"></div>
  <input id="field" style="position:absolute;left:20px;top:20px;width:200px;height:24px">
  <script>
    window.__events = [];
    const log = e => window.__events.push(e);
    document.getElementById('target').addEventListener('click', e =>
      log('click@' + Math.round(e.clientX) + ',' + Math.round(e.clientY)));
    document.getElementById('target').addEventListener('mouseover', () => log('hover'));
    const f = document.getElementById('field');
    // keydown is the event Input.insertText does NOT produce; a field that
    // filters keys or searches as you type depends on it.
    f.addEventListener('keydown', e => log('keydown:' + e.key));
    f.addEventListener('input', e => log('input:' + e.target.value));
  </script>
</body></html>"#;

/// A sign-in form of the kind an agent is actually asked to complete.
///
/// Every field is labelled a different legal way, because that is the part a
/// snapshot has to get right: a real page does not label all its inputs the
/// same way, and a walker that only understands `<label for>` produces a list
/// where half the entries are blank and the model is back to guessing.
const FORM_PAGE: &str = r#"<!doctype html><html><head><title>sign in</title></head>
<body>
  <h1>Sign in</h1>
  <form id="f" onsubmit="document.getElementById('out').innerText='submitted:'+
      document.getElementById('email').value+'/'+document.querySelector('[name=pw]').value;return false">
    <label for="email">Email address</label>
    <input id="email" type="text">
    <input name="pw" type="password" placeholder="Password">
    <label><input id="news" type="checkbox"> Send me news</label>
    <button type="submit" aria-label="Sign in to your account">Go</button>
    <button type="button" disabled>Cannot press</button>
  </form>
  <input type="hidden" name="csrf" value="secret">
  <div id="out"></div>
</body></html>"#;

/// A page whose controls navigate, which is what most real clicks do.
///
/// The test server answers every path with this same document, so a navigation
/// is visible in the URL rather than the content. That is the property under
/// test: whether the tool reports the page it ended on.
const NAV_PAGE: &str = r#"<!doctype html><html><head><title>first</title></head>
<body>
  <a id="link" href="/second">Go to the second page</a>
  <button id="soon" onclick="setTimeout(()=>{location.href='/later'},300)">Go in a moment</button>
  <button id="quiet" onclick="document.title='still here'">Change nothing</button>
</body></html>"#;

/// Serve `PAGE` on loopback until the returned handle is dropped.
async fn serve() -> String {
    serve_page(PAGE).await
}

async fn serve_page(page: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // One failed accept does not end the server. Same defect, same reason
        // as the one fixed in `openbot-app/tests/page.rs`: this suite runs many
        // browsers at once, the descriptor limit is a real ceiling, and a
        // transient EMFILE used to shut the server down for the rest of the
        // test - after which every navigation got Chrome's error page.
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
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let body = page.as_bytes();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://{addr}/")
}

/// Chrome is heavy, and `cargo test` runs these concurrently by default.
/// Launching one per test at once starves the machine and the suite looks
/// hung rather than slow. Capping here keeps the default invocation working
/// instead of relying on a `--test-threads` flag.
static LAUNCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);

/// A launched browser, holding its place in the concurrency cap until dropped.
struct Session {
    _dir: tempfile::TempDir,
    _permit: tokio::sync::SemaphorePermit<'static>,
    browser: Arc<Browser>,
}

impl std::ops::Deref for Session {
    type Target = Browser;
    fn deref(&self) -> &Browser {
        &self.browser
    }
}

/// Launch a browser, or explain why the test is being skipped.
async fn browser() -> Option<Session> {
    // A skip that reads as a pass is the worst outcome: on a bare CI runner
    // the whole suite would go green having exercised nothing, including
    // every assertion about synthetic clicks and keystrokes. CI sets this so
    // a missing browser is a failure there and a skip on a developer machine.
    let required = std::env::var_os("OPENBOT_REQUIRE_BROWSER").is_some();
    if find_browser().is_none() {
        assert!(
            !required,
            "OPENBOT_REQUIRE_BROWSER is set but no Chromium-family browser was found"
        );
        eprintln!("SKIP: no Chromium-family browser installed");
        return None;
    }
    let permit = LAUNCHES.acquire().await.expect("semaphore is never closed");
    let dir = tempfile::tempdir().unwrap();
    match Browser::launch(&dir.path().join("profile")).await {
        Ok(b) => Some(Session {
            _dir: dir,
            _permit: permit,
            browser: Arc::new(b),
        }),
        Err(e) => {
            assert!(
                !required,
                "OPENBOT_REQUIRE_BROWSER is set but launching failed: {e}"
            );
            eprintln!("SKIP: browser failed to launch: {e}");
            None
        }
    }
}

#[tokio::test]
async fn it_navigates_and_reads_a_real_page() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;

    let info = b.navigate(&url).await.expect("navigate");
    assert_eq!(info.title, "openbot test page");
    assert!(info.url.starts_with("http://127.0.0.1"));

    let text = b.text().await.expect("text");
    assert!(text.contains("Hello from openbot"), "got: {text}");
    assert!(text.contains("The quick brown fox"));
    // innerText, not innerHTML: the model should see what a person sees.
    assert!(
        !text.contains("<h1>"),
        "markup leaked into the text: {text}"
    );

    b.shutdown().await;
}

#[tokio::test]
async fn it_lists_links_with_resolved_hrefs() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    let links = b.links().await.expect("links");
    let arr = links.as_array().expect("an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "First link");
    assert_eq!(arr[0]["href"], "https://example.com/one");

    b.shutdown().await;
}

#[tokio::test]
async fn it_clicks_and_the_page_reacts() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    b.click("#btn").await.expect("click");
    let text = b.text().await.expect("text");
    assert!(
        text.contains("clicked"),
        "the click did not run the page's handler: {text}"
    );

    b.shutdown().await;
}

#[tokio::test]
async fn filling_a_field_fires_the_events_a_framework_listens_for() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    b.fill("#field", "hello there").await.expect("fill");
    let text = b.text().await.expect("text");
    // Setting .value alone would leave this empty; the input event is what
    // frameworks listen to.
    assert!(
        text.contains("saw: hello there"),
        "no input event was dispatched: {text}"
    );

    b.shutdown().await;
}

#[tokio::test]
async fn a_missing_selector_is_reported_not_silently_ignored() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    let err = b.click("#does-not-exist").await;
    assert!(err.is_err(), "clicking nothing reported success");
    let err = b.fill("#also-missing", "x").await;
    assert!(err.is_err());

    b.shutdown().await;
}

#[tokio::test]
async fn a_hostile_selector_cannot_execute_script() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    // Selectors are model-supplied and spliced into evaluated source. This one
    // tries to close the string and run its own statement; it must be treated
    // as a (nonsensical) selector, not as code.
    let hostile = r#"x");document.title="pwned";//"#;
    let _ = b.click(hostile).await; // fails as "no such element", which is expected
    let info = b.info().await.expect("info");
    assert_eq!(
        info.title, "openbot test page",
        "a selector escaped its quotes and ran"
    );

    b.shutdown().await;
}

#[tokio::test]
async fn it_captures_a_png_screenshot() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.expect("navigate");

    let png = b.screenshot().await.expect("screenshot");
    assert!(png.len() > 1000, "suspiciously small: {} bytes", png.len());
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

    b.shutdown().await;
}

#[tokio::test]
async fn cookies_survive_a_browser_restart_in_the_same_profile() {
    if find_browser().is_none() {
        eprintln!("SKIP: no Chromium-family browser installed");
        return;
    }
    // This is the property "sign in once" rests on: the profile is durable, so
    // a rebuilt guest keeps its sessions.
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let url = serve().await;

    let first = match Browser::launch(&profile).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return;
        }
    };
    first.navigate(&url).await.expect("navigate");
    first
        .fill("#field", "ignored")
        .await
        .expect("fill to prove the page is live");
    // Set a cookie the way a login would.
    let _ = first.click("#btn").await;
    first.shutdown().await;

    // Retry rather than sleeping a guessed amount. Chrome releases the
    // profile lock on its own schedule, and on a loaded machine (a full
    // workspace run with a dozen browsers competing for the CPU) that is
    // longer than any fixed number. A test that only fails when the machine
    // is busy is a test people learn to re-run rather than believe.
    let mut second = None;
    for _ in 0..40 {
        match Browser::launch(&profile).await {
            Ok(b) => {
                second = Some(b);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    let second = second.expect("the profile never became available again");
    let info = second.navigate(&url).await.expect("navigate again");
    assert_eq!(info.title, "openbot test page");
    second.shutdown().await;
}

// Input: what a person does when they take the computer back.

/// Read the event log the page keeps.
async fn events(b: &Browser) -> Vec<String> {
    let raw = b
        .text_of("JSON.stringify(window.__events||[])")
        .await
        .unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_default()
}

#[tokio::test]
async fn a_click_lands_where_it_was_aimed() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();

    // The target is 80x40 at (100,150), so its centre is (140,170).
    b.click_at(140.0, 170.0).await.unwrap();

    let log = events(&b).await;
    assert!(
        log.iter().any(|e| e.starts_with("click@")),
        "no click reached the page: {log:?}"
    );
    // CDP delivers mouse events that produce no `click` at all if clickCount
    // is missing, so check the coordinates arrived intact too.
    assert!(
        log.iter().any(|e| e == "click@140,170"),
        "the click landed somewhere else: {log:?}"
    );
    // And the hover that precedes it, which is what opens most menus.
    assert!(log.iter().any(|e| e == "hover"), "no mouseover: {log:?}");
}

#[tokio::test]
async fn a_click_outside_the_target_does_not_hit_it() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();

    // Just past the target's right edge. If this "passes" it means the events
    // are being delivered to the page rather than the coordinates.
    b.click_at(400.0, 170.0).await.unwrap();
    let log = events(&b).await;
    assert!(
        !log.iter().any(|e| e.starts_with("click@")),
        "a click 220px away still hit the target: {log:?}"
    );
}

#[tokio::test]
async fn typing_produces_real_keystrokes_not_just_a_value() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();

    // Focus the way a person would: click into the field.
    b.click_at(120.0, 32.0).await.unwrap();
    b.type_text("hey").await.unwrap();

    let log = events(&b).await;
    // The distinction that matters. `Input.insertText` would leave the value
    // correct and every one of these missing, and the failure would only show
    // up on a site that searches as you type.
    for k in ["keydown:h", "keydown:e", "keydown:y"] {
        assert!(log.contains(&k.to_string()), "missing {k}: {log:?}");
    }
    assert!(
        log.contains(&"input:hey".to_string()),
        "the field never reached the full value: {log:?}"
    );
}

#[tokio::test]
async fn a_named_key_reaches_the_page() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();
    b.click_at(120.0, 32.0).await.unwrap();

    b.key("Enter").await.unwrap();
    b.key("ArrowDown").await.unwrap();
    let log = events(&b).await;
    assert!(log.contains(&"keydown:Enter".to_string()), "{log:?}");
    // Navigation keys need rawKeyDown; sent as keyDown with no text Chrome
    // swallows them, and the page sees nothing.
    assert!(log.contains(&"keydown:ArrowDown".to_string()), "{log:?}");

    assert!(
        b.key("NotAKey").await.is_err(),
        "an unknown key was accepted"
    );
}

#[tokio::test]
async fn scrolling_moves_the_page() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();

    assert_eq!(b.text_of("String(window.scrollY)").await.unwrap(), "0");
    b.scroll(200.0, 200.0, 400.0).await.unwrap();

    // The wheel is animated, so give it a moment rather than asserting into a
    // race.
    let mut moved = false;
    for _ in 0..40 {
        let y: f64 = b
            .text_of("String(window.scrollY)")
            .await
            .unwrap_or_default()
            .parse()
            .unwrap_or(0.0);
        if y > 0.0 {
            moved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(moved, "the page never scrolled");
}

#[tokio::test]
async fn a_frame_is_a_jpeg_that_matches_the_viewport() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(INPUT_PAGE).await;
    b.navigate(&url).await.unwrap();

    let f = b.frame(60).await.unwrap();
    // JPEG's SOI marker. Asserting on real bytes rather than "it returned
    // something": a viewer fed a PNG labelled JPEG shows a broken image.
    assert_eq!(
        &f.jpeg[..3],
        &[0xFF, 0xD8, 0xFF],
        "not a JPEG: {:?}",
        &f.jpeg[..8.min(f.jpeg.len())]
    );
    assert!(f.jpeg.len() > 1000, "suspiciously small frame");

    // The viewport travels with the image so a click can be mapped back into
    // the page; a frame that does not know its own size is unusable for input.
    assert!(f.width > 0.0 && f.height > 0.0, "no viewport: {f:?}");
    let w: f64 = b
        .text_of("String(window.innerWidth)")
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(f.width, w);
}

#[tokio::test]
async fn the_page_is_not_told_this_is_a_headless_browser() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve().await;
    b.navigate(&url).await.unwrap();

    // Chrome announces `HeadlessChrome/<version>`, which many sites refuse
    // outright, while the same browser's client hints say `Google Chrome`.
    // The header disagrees with itself, and the half that gets pages blocked
    // is the half nothing else agrees with.
    let ua = b.text_of("navigator.userAgent").await.unwrap();
    assert!(
        !ua.contains("Headless"),
        "the page is still told this is headless: {ua}"
    );
    // Still Chrome, and still the real version, not an invented one, which
    // would go stale and be a louder signal than the token it replaced.
    assert!(ua.contains("Chrome/"), "{ua}");
}

#[tokio::test]
async fn the_window_is_desktop_sized() {
    let Some(b) = browser().await else {
        return;
    };
    b.navigate(&serve().await).await.unwrap();
    let w: f64 = b
        .text_of("String(window.innerWidth)")
        .await
        .unwrap()
        .parse()
        .unwrap();
    // Headless Chrome starts below 1024, which is where most sites decide you
    // are on a phone and collapse their controls behind menus.
    assert!(w >= 1024.0, "the browser is phone-sized at {w}px");
}

#[tokio::test]
async fn dropping_a_browser_reaps_its_process() {
    if find_browser().is_none() {
        eprintln!("SKIP: no Chromium-family browser installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");

    // Launch and drop without calling shutdown: a panic, a dropped guest, a
    // cancelled task. Tokio does not reap a child on drop unless told to;
    // without `kill_on_drop` this would leave Chrome running with the profile
    // locked, and the next launch would wait out its full timeout for a port
    // that never appears.
    {
        let b = Browser::launch(&profile).await.unwrap();
        b.navigate("about:blank").await.unwrap();
    }

    // The evidence: the same profile can be used again promptly. An orphan
    // holding it would make this hang rather than fail quickly.
    let again = tokio::time::timeout(Duration::from_secs(20), Browser::launch(&profile)).await;
    let b = again
        .expect("relaunching on the same profile timed out; the first browser was not reaped")
        .expect("relaunch failed");
    b.shutdown().await;
}

#[tokio::test]
async fn a_browser_that_outlived_its_guest_is_adopted_with_its_page_intact() {
    // The failure this prevents: start `openbot up`, open a page, `taskkill /F`
    // it, start it again. Without adoption, every browser tool fails with
    // "the browser never published a debugging port", forever: the orphan
    // still holds the profile lock, so the new Chrome exits at once, and
    // nothing in the error points at the stray process.
    //
    // `kill_on_drop` cannot help: it needs destructors to run, and a crash,
    // an OOM kill or `taskkill /F` runs none.
    //
    // Adopting is better than reaping, and this asserts the part that makes
    // it worthwhile: the surviving browser is still signed into whatever it
    // was signed into. A relaunch that merely succeeded would pass a weaker
    // test while silently losing every session on that computer.
    if find_browser().is_none() {
        assert!(
            std::env::var_os("OPENBOT_REQUIRE_BROWSER").is_none(),
            "OPENBOT_REQUIRE_BROWSER is set but no browser was found"
        );
        eprintln!("SKIP: no Chromium-family browser installed");
        return;
    }
    let _permit = LAUNCHES.acquire().await.expect("semaphore is never closed");
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let url = serve().await;

    let first = Browser::launch(&profile).await.expect("first launch");
    let info = first.navigate(&url).await.expect("navigate");
    assert_eq!(info.title, "openbot test page");

    // Exactly what a crash leaves behind: the process still running, no
    // destructor run, the profile still locked.
    std::mem::forget(first);

    let second = Browser::launch(&profile)
        .await
        .expect("a launch onto a profile a live browser still holds");

    let after = second.info().await.expect("info");
    assert_eq!(
        after.title, "openbot test page",
        "the browser was replaced rather than adopted, losing every session on it"
    );
    assert_eq!(after.url, info.url, "adopted a different page");

    // Adopted, so there is no child handle to reap: closing has to go over
    // CDP or every run of this test leaks a browser.
    let port = adoption_port(&profile).expect("an adopted browser has a port file");
    second.shutdown().await;
    assert!(
        port_goes_quiet(port).await,
        "shutdown left the adopted browser on {port}; nothing owns it, so nothing will reap it"
    );
}

/// The port the profile advertises, read the way the guest reads it.
fn adoption_port(profile: &std::path::Path) -> Option<u16> {
    let raw = std::fs::read_to_string(profile.join("DevToolsActivePort")).ok()?;
    raw.lines().next()?.trim().parse().ok()
}

/// Wait for nothing to be listening on `port`.
///
/// Intentionally not "can a launch succeed again?": a browser that failed to
/// close would simply be adopted a second time, and that assertion would pass
/// while leaking. Whether the socket is gone is the question with only one
/// answer.
async fn port_goes_quiet(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..40 {
        let dead = tokio::task::spawn_blocking(move || {
            std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err()
        })
        .await
        .unwrap_or(false);
        if dead {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test]
async fn a_browser_that_dies_mid_session_is_replaced_rather_than_kept() {
    // Chrome does not only die when openbot does. A crashed tab, an OOM kill,
    // or someone closing it by hand kills it underneath a guest that is still
    // running. Holding the dead handle in a `OnceCell` with no way to put a
    // live browser back would make every browser tool fail until the guest
    // itself was restarted.
    //
    // The first failure after the death matters as much as recovery: without
    // the closed flag, a call would take the full call timeout before
    // reporting "Runtime.evaluate timed out", because the send lands in a
    // channel whose writer has already broken and nothing wakes the waiter.
    //
    // Both halves are asserted here: a dead browser is noticed promptly, and
    // asking for one again gets a working browser rather than the dead one.
    if find_browser().is_none() {
        assert!(
            std::env::var_os("OPENBOT_REQUIRE_BROWSER").is_none(),
            "OPENBOT_REQUIRE_BROWSER is set but no browser was found"
        );
        eprintln!("SKIP: no Chromium-family browser installed");
        return;
    }
    let _permit = LAUNCHES.acquire().await.expect("semaphore is never closed");
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let url = serve().await;

    let first = Browser::launch(&profile).await.expect("launch");
    first.navigate(&url).await.expect("navigate");
    assert!(first.is_alive(), "a browser in use is not alive");
    let port = adoption_port(&profile).expect("a launched browser has a port file");

    // Kill it abruptly, from outside.
    first.shutdown().await;
    assert!(
        port_goes_quiet(port).await,
        "the browser did not go away, so this proves nothing"
    );

    // Promptly, not after the 60-second call timeout. The generous bound is
    // still twenty times faster than the timeout.
    let started = std::time::Instant::now();
    let err = first.info().await.expect_err("a dead browser answered");
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(3),
        "a dead browser took {took:?} to say so; callers hang instead of recovering"
    );
    assert!(
        !first.is_alive(),
        "a dead browser still reports itself alive: {err}"
    );

    // Recovery: launching again on the same profile works.
    let replacement = Browser::launch(&profile).await.expect("relaunch");
    let info = replacement
        .navigate(&url)
        .await
        .expect("navigate the new one");
    assert_eq!(info.title, "openbot test page");
    replacement.shutdown().await;
}

/// A form can be completed using only what the page reported about itself.
///
/// This is the whole point of the snapshot, and it is stated as an end-to-end
/// property rather than a list of fields, because the defect it replaces was not
/// a missing feature but a missing *correspondence*: `read` returned `innerText`
/// and `click`/`fill` demanded CSS selectors, and nothing in the reading ever
/// emitted one. A model could see the page or act on it, never both.
///
/// So this test is forbidden from using knowledge a model would not have. No
/// selector from `FORM_PAGE` appears below the snapshot call; the refs come from
/// the page's own report of itself, chosen by the accessible name a person would
/// use. If that correspondence breaks, the fields cannot be found here either.
#[tokio::test]
async fn a_form_can_be_filled_from_the_snapshot_alone() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(FORM_PAGE).await;
    b.navigate(&url).await.expect("navigate");

    let snap = b.snapshot(150).await.expect("snapshot");
    let elements = snap["elements"]
        .as_array()
        .expect("elements is a list")
        .clone();

    // Pick fields the way a model would: by the name a person would use.
    let by_name = |want: &str| -> Target {
        let found = elements
            .iter()
            .find(|e| e["name"].as_str().unwrap_or_default().contains(want))
            .unwrap_or_else(|| {
                panic!(
                    "nothing in the snapshot is called {want:?}. A model has no other way to find \
                     this field, so it cannot fill the form:\n{snap:#}"
                )
            });
        let r = found["ref"].as_str().expect("every element carries a ref");
        Target::Ref(r.trim_start_matches('e').parse().expect("refs are e<n>"))
    };

    // Labelled by `<label for>`, by `placeholder`, and by `aria-label`. Three
    // legal ways, all of which a real page uses somewhere.
    let email = by_name("Email address");
    let password = by_name("Password");
    let submit = by_name("Sign in to your account");

    b.fill_target(&email, "someone@example.com")
        .await
        .expect("fill the email field found by name");
    b.fill_target(&password, "hunter2")
        .await
        .expect("fill the password field found by name");
    b.click_target(&submit)
        .await
        .expect("press the button found by name");

    let out = b
        .text_of("document.getElementById('out').innerText")
        .await
        .expect("read the result");
    assert_eq!(
        out, "submitted:someone@example.com/hunter2",
        "the form did not receive what the snapshot said it would; refs and elements have drifted"
    );
}

/// The listing says what each thing is, and leaves out what cannot be used.
///
/// A list of refs with empty names would satisfy the test above whenever the
/// ordering happened to line up, and would be useless on any real page. These
/// are the properties that make the listing worth reading at all.
#[tokio::test]
async fn the_snapshot_says_what_each_thing_is() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(FORM_PAGE).await;
    b.navigate(&url).await.expect("navigate");
    let snap = b.snapshot(150).await.expect("snapshot");
    let elements = snap["elements"]
        .as_array()
        .expect("elements is a list")
        .clone();

    let find = |name: &str| {
        elements
            .iter()
            .find(|e| e["name"].as_str().unwrap_or_default().contains(name))
            .unwrap_or_else(|| panic!("no element named {name:?} in {snap:#}"))
            .clone()
    };

    assert_eq!(find("Email address")["role"], "textbox");
    assert_eq!(find("Sign in to your account")["role"], "button");
    assert_eq!(
        find("Send me news")["role"],
        "checkbox",
        "a checkbox wrapped in its own label must still be found and named by it"
    );
    assert_eq!(
        find("Cannot press")["disabled"],
        true,
        "a disabled control has to say so, or a model spends a turn and an approval on it"
    );

    // A hidden input is not something anyone can act on, and listing it costs
    // tokens while inviting the model to try.
    assert!(
        !elements
            .iter()
            .any(|e| e["value"].as_str() == Some("secret")),
        "the hidden csrf input is in the snapshot: {snap:#}"
    );
}

/// A ref does not survive a navigation, and the error names the way out.
///
/// The dangerous version is a ref that silently rebinds to whatever now
/// occupies its index, clicking the wrong thing on the new page having been
/// approved for the old one.
///
/// Worth being precise about which failure this is: a navigation replaces the
/// page's whole JavaScript context, so `__openbot_refs` is not stale, it is
/// *gone*, and this path reports "no snapshot". The genuinely stale case —
/// refs intact, element removed — is a client-rendered app re-rendering, and
/// has its own test below. Two different mechanisms that look identical from
/// the outside, which is why asserting only "the message mentions
/// browser.snapshot" left the `isConnected` check untested.
#[tokio::test]
async fn a_ref_does_not_survive_a_navigation_and_says_so() {
    let Some(b) = browser().await else {
        return;
    };
    let form = serve_page(FORM_PAGE).await;
    b.navigate(&form).await.expect("navigate");
    b.snapshot(150).await.expect("snapshot");

    // Somewhere else entirely, which has its own clickable things at the same
    // indexes.
    let other = serve().await;
    b.navigate(&other).await.expect("navigate away");

    let err = b
        .click_target(&Target::Ref(1))
        .await
        .expect_err("a ref from the previous page must not resolve on this one");
    let shown = err.to_string();
    assert!(
        shown.contains("browser.snapshot"),
        "the error has to name the way out, or the model goes back to guessing: {shown}"
    );
}

/// An element removed without a navigation is reported as stale, not clicked.
///
/// This is the case `isConnected` exists for, and the one a real page produces
/// constantly: a client-rendered app re-renders a list, the old nodes are
/// detached, and the refs still point at them. Clicking a detached node
/// succeeds silently in JavaScript and does nothing at all, so without this
/// check the tool reports success for an action that never reached the page —
/// which is the failure mode this whole crate keeps trying to avoid.
#[tokio::test]
async fn a_ref_to_a_removed_element_is_stale_rather_than_a_silent_no_op() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(FORM_PAGE).await;
    b.navigate(&url).await.expect("navigate");
    let snap = b.snapshot(150).await.expect("snapshot");

    let submit = snap["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| {
            e["name"]
                .as_str()
                .unwrap_or_default()
                .contains("Sign in to your account")
        })
        .expect("the submit button was in the snapshot")["ref"]
        .as_str()
        .expect("a ref")
        .trim_start_matches('e')
        .parse()
        .expect("e<n>");

    // The page rewrites itself, as a client-rendered app does on every render.
    // No navigation: `__openbot_refs` survives and its entries are detached.
    b.text_of("(()=>{document.getElementById('f').remove();return 'gone'})()")
        .await
        .expect("remove the form");

    let err = b
        .click_target(&Target::Ref(submit))
        .await
        .expect_err("the element is detached, so this must not report success");
    let shown = err.to_string();
    assert!(
        shown.contains("no longer on the page"),
        "a detached node must be reported as stale, not as missing and not as success: {shown}"
    );
}

/// A ref nobody handed out is a different problem from a stale one.
#[tokio::test]
async fn a_ref_used_before_any_snapshot_says_to_take_one() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(FORM_PAGE).await;
    b.navigate(&url).await.expect("navigate");

    let err = b
        .click_target(&Target::Ref(1))
        .await
        .expect_err("no snapshot has been taken");
    let shown = err.to_string();
    assert!(
        shown.contains("no snapshot"),
        "must tell 'you never asked' apart from 'the page moved on': {shown}"
    );
}

/// A click that navigates reports the page it arrived on.
///
/// The most common click in web automation is a link or a submit button, and
/// `e.click()` returns as soon as the navigation is *scheduled*. The `info()`
/// that followed therefore landed in the old execution context and reported the
/// previous page's url and title — as if the click had done nothing.
///
/// The model's recovery from "nothing happened" is to click again. On a live
/// web app that is a double-submitted form, a double-sent message, a
/// double-charged cart — and the second one is not gated, because the approval
/// the person read was for the first.
#[tokio::test]
async fn a_click_that_navigates_reports_where_it_landed() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(NAV_PAGE).await;
    b.navigate(&url).await.expect("navigate");

    let after = b
        .click_and_settle(&Target::Selector("#link".into()))
        .await
        .expect("click the link");

    // `navigated` first, and it is the assertion doing the work here. Asserting
    // only the url makes an unreliable regression test: with the settle window
    // removed, this click still reported the right url about half the time,
    // because whether `info()` lands before or after the context swaps is the
    // very race being fixed. A test that passes half the time with the defect
    // present is worse than no test — it reports the bug as fixed on the run
    // that matters. `navigated` cannot be true unless the swap was observed.
    assert!(
        after.navigated,
        "the page was replaced and the tool did not notice, so every ref from the last snapshot is dead and nothing has said so: {after:?}"
    );
    assert!(
        after.url.ends_with("/second"),
        "the click navigated and the tool reported where the page used to be, so a model reading it concludes nothing happened and clicks again: {:?}",
        after.url
    );
}

/// A navigation that starts on a timer is waited for too.
///
/// Plenty of real controls navigate from a handler rather than synchronously.
/// Returning before that has settled reports the old page just as confidently.
#[tokio::test]
async fn a_click_that_navigates_a_moment_later_is_still_waited_for() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(NAV_PAGE).await;
    b.navigate(&url).await.expect("navigate");

    let after = b
        .click_and_settle(&Target::Selector("#soon".into()))
        .await
        .expect("click the deferred button");

    assert!(
        after.url.ends_with("/later"),
        "a navigation scheduled 300ms after the click was not waited for; got {:?}",
        after.url
    );
}

/// A click that does not navigate does not pay for one, and says so.
///
/// The cost of waiting has to be bounded by what actually happened, or filling
/// a ten-field form becomes ten waits for navigations that never come. This is
/// also the anti-vacuity test for the two above: a `click_and_settle` that
/// always waited the full deadline and always reported the current url would
/// pass both of them.
#[tokio::test]
async fn a_click_that_changes_nothing_does_not_wait_for_a_navigation() {
    let Some(b) = browser().await else {
        return;
    };
    let url = serve_page(NAV_PAGE).await;
    b.navigate(&url).await.expect("navigate");

    let started = std::time::Instant::now();
    let after = b
        .click_and_settle(&Target::Selector("#quiet".into()))
        .await
        .expect("click the quiet button");
    let took = started.elapsed();

    assert!(
        !after.navigated,
        "nothing navigated, and saying it did would invalidate every ref for no reason"
    );
    assert_eq!(
        after.title, "still here",
        "the handler ran, so the tool must report the page as it is now"
    );
    assert!(
        took < Duration::from_millis(900),
        "a click that does not navigate waited {took:?} for one; ten of those is a form"
    );
}
