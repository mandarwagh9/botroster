//! A real browser in the guest, driven over the Chrome DevTools Protocol.
//!
//! Most systems that need automating have no clean API, so the agent works the
//! website the way a person does.
//!
//! # Why hand-rolled
//!
//! CDP is JSON-RPC over a WebSocket, which this codebase already speaks. A
//! browser-automation crate would bring a large dependency tree to wrap a
//! protocol of which only a handful of methods are needed. The whole client
//! is in this file.
//!
//! # The profile lives in the durable volume
//!
//! Chrome is launched against a `user-data-dir` inside the workspace's volume,
//! so cookies and signed-in sessions survive the guest being rebuilt. "Sign in
//! once" is not a browser feature; it is a consequence of putting the profile
//! somewhere durable.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

use std::collections::HashMap;

/// How long to wait for Chrome to publish its debugging port.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait on a recorded port before deciding nothing is there.
const ADOPT_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long to wait for one CDP call.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for a navigation to settle.
const NAV_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chromium-family browser found; set OPENBOT_BROWSER to its path")]
    NotFound,
    /// `OPENBOT_BROWSER` was set and there is nothing at that path.
    ///
    /// Separate from [`BrowserError::NotFound`] because the two need opposite
    /// advice and used to give the same. Someone whose browser is somewhere
    /// unusual sets this variable, mistypes the path, and is told "no browser
    /// found; set OPENBOT_BROWSER to its path" — which is the thing they just
    /// did, so the message reads as if the variable were being ignored. The
    /// path is quoted back because the mistake is almost always visible in it.
    #[error("OPENBOT_BROWSER points at {0}, and there is nothing there; correct it or unset it to search the usual install locations")]
    OverrideMissing(PathBuf),
    #[error("launching the browser failed: {0}")]
    Launch(String),
    #[error("the browser connection closed")]
    Closed,
    #[error("devtools: {0}")]
    Protocol(String),
    #[error("nothing on the page matches `{0}`")]
    NoSuchElement(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, BrowserError>;

/// Locate a browser, or say why not: an explicit override first, then the usual
/// installs.
///
/// [`find_browser`] is the same search with the reason discarded, kept because
/// most callers are skip-guards that only want to know whether a browser exists.
/// Anything that reports to a person should use this one.
pub fn locate_browser() -> Result<PathBuf> {
    resolve_browser(std::env::var_os("OPENBOT_BROWSER").map(PathBuf::from))
}

/// The search itself, with the override passed in rather than read.
///
/// Split out so the behaviour can be tested without `set_var`. The test that
/// used to cover this mutated the process environment, which races every other
/// test in the binary — and it asserted nothing, so the race never surfaced as
/// a failure. A pure function is testable from several threads at once.
fn resolve_browser(override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        // An explicit override that does not exist is a mistake to report, not
        // a reason to silently run a different browser than the one asked for.
        // It used to return None here, which the caller could only turn into
        // NotFound — silently, and with advice to set the variable that was
        // already set.
        return if p.exists() {
            Ok(p)
        } else {
            Err(BrowserError::OverrideMissing(p))
        };
    }
    const CANDIDATES: &[&str] = &[
        // Linux, where a deployed guest runs.
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        // macOS and Windows, for local development.
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .ok_or(BrowserError::NotFound)
}

/// Locate a browser: an explicit override first, then the usual installs.
///
/// Returns `None` for both "nothing installed" and "the override points
/// nowhere". Callers that show the outcome to a person want
/// [`locate_browser`], which distinguishes them.
pub fn find_browser() -> Option<PathBuf> {
    locate_browser().ok()
}

/// Whether the operator has asked for Chrome's own sandbox to be turned off.
///
/// Opt-in, and deliberately awkward to set by accident: anything but `1` or
/// `true` leaves the sandbox on.
fn no_sandbox_requested() -> bool {
    parse_no_sandbox(std::env::var("OPENBOT_BROWSER_NO_SANDBOX").ok().as_deref())
}

/// The reading of that variable, with the value passed in rather than read.
///
/// Same split as [`resolve_browser`], for the same reason: a test that reads
/// the process environment has to write it first, which races every other test
/// in the binary. Passing the value in also stops the test from becoming a
/// restatement of the implementation — the first version of it re-derived the
/// comparison inline and would have passed against any function at all.
fn parse_no_sandbox(value: Option<&str>) -> bool {
    let Some(v) = value else { return false };
    let v = v.trim().to_ascii_lowercase();
    v == "1" || v == "true"
}

/// The command line for a headless browser.
///
/// Split out from the spawn so the flags can be asserted without launching
/// anything. The flag that matters is the one that is *absent*, and an absence
/// is unobservable in an integration test that only checks the browser came up.
///
/// # Why `--no-sandbox` is not passed by default
///
/// It used to be, unconditionally, under a comment reading "the guest is
/// already a sandbox; Chrome's own sandbox needs privileges a container usually
/// will not have". The second clause is true and the first is not:
/// `CLAUDE.md` states plainly that today's guest is an ordinary process running
/// as the user, not a VM and not a container. So the premise was false, and it
/// was being used to switch off a real security boundary.
///
/// That boundary is the one that matters most here. The renderer is the process
/// that parses HTML, CSS, images and JavaScript from pages this agent was
/// pointed at by a model, which may itself have been steered by a page it read
/// earlier. Chrome's sandbox is what stops a bug in that parsing from becoming
/// code execution. Without it, a single renderer exploit runs as the user, with
/// the user's files, the user's SSH keys and — because `shell.exec` inherits
/// this process's environment — the model credential. The flag traded the
/// product's strongest defence for a convenience in a deployment shape that
/// does not exist yet.
///
/// It stays available, because the case the old comment described is real: a
/// container running as root cannot initialise the sandbox and Chrome will
/// refuse to start. That is what `OPENBOT_BROWSER_NO_SANDBOX=1` is for. Making
/// it opt-in means the person who needs it is the person who sets it, and knows
/// what they gave up.
fn chrome_args(profile: &Path, no_sandbox: bool) -> Vec<String> {
    let mut args = vec![
        "--headless=new".to_owned(),
        // A desktop-sized window, because the default is not one.
        //
        // Headless Chrome starts around 800x600, of which roughly 764x429 is
        // usable, below the 1024px breakpoint most sites use to select a mobile
        // layout. An agent working a real web app would get the cramped layout,
        // with controls collapsed behind menus it then has to discover.
        "--window-size=1280,900".to_owned(),
        // Port 0 lets the OS choose; Chrome writes the real one to
        // DevToolsActivePort. Hard-coding a port collides with any other guest
        // on the host.
        "--remote-debugging-port=0".to_owned(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-gpu".to_owned(),
        "--disable-dev-shm-usage".to_owned(),
    ];
    if no_sandbox {
        args.push("--no-sandbox".to_owned());
    }
    args.push("about:blank".to_owned());
    args
}

pub struct Browser {
    /// `None` when the browser was adopted rather than spawned; see
    /// [`Browser::launch`]. Nothing here owns it, so nothing here may kill it.
    child: Mutex<Option<tokio::process::Child>>,
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    /// Set once the connection is gone, by whichever side notices first.
    ///
    /// Without it a browser that dies mid-session is slowly unusable rather
    /// than immediately so: the send succeeds into a channel whose writer has
    /// already broken, and the caller waits the full call timeout for a reply
    /// that cannot arrive.
    closed: Arc<AtomicBool>,
}

impl Browser {
    /// Launch a browser against a persistent profile, or adopt one already on
    /// it.
    ///
    /// `kill_on_drop` only runs when destructors run, and a crash, an OOM kill
    /// or `taskkill /F` runs none of them. The browser then outlives the guest
    /// that started it and keeps the profile locked, so a fresh launch would
    /// fail with "the browser never published a debugging port", and every
    /// launch after that, until the stray process is found by hand.
    ///
    /// Deleting the port file before probing it would make that unrecoverable.
    /// The survivor is still signed into everything, which is the state the
    /// profile exists to keep, so it is looked for before anything is spawned.
    pub async fn launch(profile: &Path) -> Result<Self> {
        std::fs::create_dir_all(profile)?;
        let profile = &profile_dir(profile);
        let port_file = profile.join("DevToolsActivePort");

        if let Some(port) = adoptable(&port_file).await {
            // Adopted, not owned: this guest did not start it and must not
            // reap it on exit.
            //
            // A browser that is dying can still answer that probe:
            // `kill_on_drop` terminates the root and returns, and Chrome's
            // other processes take a moment. `attach` ends with real CDP
            // calls, which fail against a dead browser so `launch` returns
            // `Err` rather than handing back a dead handle. One that dies
            // after attaching is caught by [`Self::is_alive`], which the guest
            // checks before reusing what it is holding.
            return Self::attach(port, None).await;
        }

        // `locate_browser`, not `find_browser`: this is the path whose error a
        // person reads, and the two failures need opposite advice.
        let exe = locate_browser()?;
        // Nothing answered, so whatever is in there is stale and would be read
        // as this run's port.
        let _ = std::fs::remove_file(&port_file);

        let child = tokio::process::Command::new(&exe)
            .args(chrome_args(profile, no_sandbox_requested()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Reap the browser when this handle goes away. Tokio leaves a
            // child running by default; without this, every guest restart
            // would leave an orphan Chrome holding the profile lock, and the
            // next launch would hang until its timeout.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| BrowserError::Launch(e.to_string()))?;

        let port = wait_for_port(&port_file).await?;
        Self::attach(port, Some(child)).await
    }

    /// Wire a CDP session onto a browser already listening on `port`.
    ///
    /// Shared by both launch paths so an adopted browser is set up exactly
    /// like a spawned one, including the user-agent override, which a page
    /// would otherwise see differ depending on whether the guest had crashed.
    async fn attach(port: u16, child: Option<tokio::process::Child>) -> Result<Self> {
        let ws_url = page_target(port).await?;

        let (stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
        let (mut sink, mut source) = stream.split();

        let closed = Arc::new(AtomicBool::new(false));

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        {
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                while let Some(m) = rx.recv().await {
                    if sink.send(Message::Text(m)).await.is_err() {
                        break;
                    }
                }
                closed.store(true, Ordering::Relaxed);
            });
        }

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::default();
        {
            let pending = Arc::clone(&pending);
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                while let Some(Ok(Message::Text(t))) = source.next().await {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else {
                        continue;
                    };
                    // Events have no id; only replies are correlated.
                    if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                        if let Some(w) = pending.lock().await.remove(&id) {
                            let _ = w.send(v);
                        }
                    }
                }
                // Flag first, then wake the waiters. A call landing between
                // the two sees the flag; one landing after the clear would
                // otherwise sit in a map nobody will ever drain again.
                closed.store(true, Ordering::Relaxed);
                pending.lock().await.clear();
            });
        }

        let b = Self {
            child: Mutex::new(child),
            tx,
            pending,
            next_id: AtomicU64::new(1),
            closed,
        };
        b.call("Page.enable", json!({})).await?;
        b.call("Runtime.enable", json!({})).await?;
        b.present_as_a_normal_browser().await;
        Ok(b)
    }

    /// Whether the connection to the browser is still up.
    ///
    /// The owner uses this to decide whether to keep holding it: a dead
    /// browser has to be replaced, not retried.
    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(BrowserError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (w_tx, w_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, w_tx);
        // The reader can have finished flagging and draining between the check
        // above and this insert, which would leave this reply waiting for the
        // full timeout. Re-reading after the insert closes that window.
        if self.closed.load(Ordering::Relaxed) {
            self.pending.lock().await.remove(&id);
            return Err(BrowserError::Closed);
        }

        let frame = json!({ "id": id, "method": method, "params": params });
        self.tx
            .send(frame.to_string())
            .map_err(|_| BrowserError::Closed)?;

        let reply = tokio::time::timeout(CALL_TIMEOUT, w_rx)
            .await
            .map_err(|_| BrowserError::Protocol(format!("{method} timed out")))?
            .map_err(|_| BrowserError::Closed)?;

        if let Some(e) = reply.get("error") {
            return Err(BrowserError::Protocol(
                e.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
                    .to_owned(),
            ));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Evaluate an expression and return its value.
    async fn eval(&self, expr: &str) -> Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(d) = r.get("exceptionDetails") {
            let msg = d
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("script error");
            return Err(BrowserError::Protocol(msg.to_owned()));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    pub async fn navigate(&self, url: &str) -> Result<PageInfo> {
        self.call("Page.navigate", json!({ "url": url })).await?;
        self.await_ready().await?;
        self.info().await
    }

    /// Wait for the document to finish loading.
    ///
    /// Polls `readyState` rather than waiting on `Page.loadEventFired`: the
    /// event may already have fired before the listener is registered, and a
    /// page that never fires it would hang forever. Polling has a ceiling.
    async fn await_ready(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + NAV_TIMEOUT;
        loop {
            if let Ok(Value::String(s)) = self.eval("document.readyState").await {
                if s == "complete" || s == "interactive" {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                // Not an error: a page that is still loading is often still
                // usable, and failing here would be worse than proceeding.
                tracing::warn!("navigation did not settle within the timeout");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn info(&self) -> Result<PageInfo> {
        let v = self
            .eval("JSON.stringify({url: location.href, title: document.title})")
            .await?;
        let s = v.as_str().unwrap_or("{}");
        let parsed: Value = serde_json::from_str(s).unwrap_or(json!({}));
        Ok(PageInfo {
            url: parsed["url"].as_str().unwrap_or_default().to_owned(),
            title: parsed["title"].as_str().unwrap_or_default().to_owned(),
        })
    }

    /// Readable text of the page.
    pub async fn text(&self) -> Result<String> {
        let v = self
            .eval("document.body ? document.body.innerText : ''")
            .await?;
        Ok(v.as_str().unwrap_or_default().to_owned())
    }

    /// Every link on the page, as `text -> href`.
    pub async fn links(&self) -> Result<Value> {
        let v = self.eval(
            "JSON.stringify([...document.querySelectorAll('a[href]')].slice(0,200).map(a=>({text:a.innerText.trim().slice(0,120),href:a.href})))",
        ).await?;
        Ok(serde_json::from_str(v.as_str().unwrap_or("[]")).unwrap_or(json!([])))
    }

    pub async fn click(&self, selector: &str) -> Result<()> {
        let expr = format!(
            "(()=>{{const e=document.querySelector({});if(!e)return 'missing';e.click();return 'ok';}})()",
            js_string(selector)
        );
        match self.eval(&expr).await?.as_str() {
            Some("ok") => Ok(()),
            _ => Err(BrowserError::NoSuchElement(selector.to_owned())),
        }
    }

    pub async fn fill(&self, selector: &str, text: &str) -> Result<()> {
        // Setting `.value` alone leaves most frameworks unaware anything
        // changed, so dispatch the events a real keystroke would produce.
        let expr = format!(
            "(()=>{{const e=document.querySelector({});if(!e)return 'missing';\
             e.focus();e.value={};\
             e.dispatchEvent(new Event('input',{{bubbles:true}}));\
             e.dispatchEvent(new Event('change',{{bubbles:true}}));return 'ok';}})()",
            js_string(selector),
            js_string(text)
        );
        match self.eval(&expr).await?.as_str() {
            Some("ok") => Ok(()),
            _ => Err(BrowserError::NoSuchElement(selector.to_owned())),
        }
    }

    /// PNG bytes of the visible page.
    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        let r = self
            .call("Page.captureScreenshot", json!({ "format": "png" }))
            .await?;
        let b64 = r
            .get("data")
            .and_then(|d| d.as_str())
            .ok_or_else(|| BrowserError::Protocol("no screenshot data".into()))?;
        b64_decode(b64).ok_or_else(|| BrowserError::Protocol("undecodable screenshot".into()))
    }

    /// Drop the `Headless` token from the user agent this browser sends.
    ///
    /// Chrome announces `HeadlessChrome/<version>` when run headless, and many
    /// sites and WAFs refuse that string outright, while the same browser's
    /// `sec-ch-ua` client hints say `Google Chrome` and the version. The
    /// header contradicts itself, and the half that gets a page blocked is the
    /// half nothing else agrees with. An agent whose purpose is to work real
    /// web apps cannot start by being turned away.
    ///
    /// This is not cloaking. It is the same browser, the person's own session,
    /// on sites they signed into; headless is a rendering mode, not an
    /// identity. Automation stays detectable in many other ways and openbot
    /// makes no attempt to defeat any of them.
    ///
    /// The string is derived from the browser's own reported version rather
    /// than hardcoded: a pinned version goes stale, and a stale version is
    /// both a maintenance trap and a louder signal than the one it replaced.
    ///
    /// Best-effort: a browser that will not answer is no reason to refuse to
    /// start, since every tool still works with the default header.
    async fn present_as_a_normal_browser(&self) {
        let Ok(v) = self.call("Browser.getVersion", json!({})).await else {
            return;
        };
        let Some(ua) = v.get("userAgent").and_then(|u| u.as_str()) else {
            return;
        };
        if !ua.contains("HeadlessChrome") {
            return;
        }
        let honest = ua.replace("HeadlessChrome", "Chrome");
        let _ = self.call("Network.enable", json!({})).await;
        if let Err(e) = self
            .call(
                "Network.setUserAgentOverride",
                json!({ "userAgent": honest }),
            )
            .await
        {
            tracing::warn!(error = %e, "could not set the user agent; pages may be refused");
        }
    }

    /// Evaluate an expression and return its value as a string.
    ///
    /// Not exposed as a tool. This is for tests that need to observe what a
    /// page did, which is the only way to know a synthetic click landed:
    /// Chrome accepts mouse events without error even when nothing receives
    /// them.
    pub async fn text_of(&self, expr: &str) -> Result<String> {
        Ok(self
            .eval(expr)
            .await?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// A frame for the live viewer: JPEG bytes plus the CSS viewport it covers.
    ///
    /// JPEG rather than PNG because this goes over the wire many times a
    /// second; a full-page PNG is roughly an order of magnitude larger for a
    /// picture of a web page, and the difference determines the feel of the
    /// viewer.
    ///
    /// The viewport travels with the image so a click at (x, y) on screen maps
    /// back to the same point in the page. Reading it off the image alone is
    /// wrong the moment device pixel ratio is not 1.
    pub async fn frame(&self, quality: u8) -> Result<Frame> {
        let r = self
            .call(
                "Page.captureScreenshot",
                json!({ "format": "jpeg", "quality": quality.clamp(1, 100) }),
            )
            .await?;
        let b64 = r
            .get("data")
            .and_then(|d| d.as_str())
            .ok_or_else(|| BrowserError::Protocol("no frame data".into()))?;
        let jpeg =
            b64_decode(b64).ok_or_else(|| BrowserError::Protocol("undecodable frame".into()))?;

        // One eval for everything the viewer needs about this frame. A second
        // round trip per frame is a third of the budget at any usable rate.
        let meta = self
            .eval(
                "JSON.stringify({w:window.innerWidth,h:window.innerHeight,\
                 u:location.href,t:document.title})",
            )
            .await?;
        let m: serde_json::Value =
            serde_json::from_str(meta.as_str().unwrap_or("{}")).unwrap_or_default();

        Ok(Frame {
            jpeg,
            width: m.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0),
            height: m.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0),
            url: m
                .get("u")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            title: m
                .get("t")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// Click at a point in the page, the way a mouse does.
    ///
    /// Distinct from [`Self::click`], which needs a CSS selector: a person
    /// driving the viewer has a picture, not a DOM. It is also the only way to
    /// reach a canvas, a map, or anything else that draws its own controls.
    ///
    /// Press and release are separate CDP events, and `clickCount` must be
    /// set: without it Chrome delivers the events but no `click` ever fires,
    /// which looks exactly like a page that ignored the input.
    pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        // Move first: hover handlers and menus that open on mouseover are a
        // large fraction of what a person needs to click.
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0 }),
        )
        .await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": x, "y": y,
                    "button": "left", "buttons": 1, "clickCount": 1 }),
        )
        .await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseReleased", "x": x, "y": y,
                    "button": "left", "buttons": 0, "clickCount": 1 }),
        )
        .await?;
        Ok(())
    }

    /// Type text as keystrokes into whatever has focus.
    ///
    /// One key event pair per character rather than `Input.insertText`.
    /// `insertText` is faster and one call, but it bypasses `keydown`
    /// entirely, so a search box that filters as you type, or a field that
    /// blocks non-numeric keys, sees the text appear without ever seeing a
    /// key. Those are common enough that correctness wins over the round
    /// trips.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        for ch in text.chars() {
            let s = ch.to_string();
            self.call(
                "Input.dispatchKeyEvent",
                json!({ "type": "keyDown", "text": s, "unmodifiedText": s, "key": s }),
            )
            .await?;
            self.call(
                "Input.dispatchKeyEvent",
                json!({ "type": "keyUp", "key": s }),
            )
            .await?;
        }
        Ok(())
    }

    /// Press a named key: `Enter`, `Tab`, `Escape`, `Backspace`, an arrow.
    pub async fn key(&self, name: &str) -> Result<()> {
        let (code, text) = match name {
            "Enter" => (13, "\r"),
            "Tab" => (9, "\t"),
            "Escape" => (27, ""),
            "Backspace" => (8, ""),
            "Delete" => (46, ""),
            "ArrowUp" => (38, ""),
            "ArrowDown" => (40, ""),
            "ArrowLeft" => (37, ""),
            "ArrowRight" => (39, ""),
            "Home" => (36, ""),
            "End" => (35, ""),
            "PageUp" => (33, ""),
            "PageDown" => (34, ""),
            other => return Err(BrowserError::Protocol(format!("unknown key `{other}`"))),
        };
        // `rawKeyDown` rather than `keyDown` for keys with no text: Chrome
        // treats a keyDown carrying no text as a character event and swallows
        // the navigation ones.
        let kind = if text.is_empty() {
            "rawKeyDown"
        } else {
            "keyDown"
        };
        self.call(
            "Input.dispatchKeyEvent",
            json!({ "type": kind, "key": name, "code": name,
                    "windowsVirtualKeyCode": code, "nativeVirtualKeyCode": code,
                    "text": text, "unmodifiedText": text }),
        )
        .await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": name, "code": name,
                    "windowsVirtualKeyCode": code, "nativeVirtualKeyCode": code }),
        )
        .await?;
        Ok(())
    }

    /// Scroll the page by a wheel delta at a point.
    pub async fn scroll(&self, x: f64, y: f64, delta_y: f64) -> Result<()> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y,
                    "deltaX": 0, "deltaY": delta_y }),
        )
        .await?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        // `Browser.close` asks either kind to exit cleanly, and is the only
        // way to close an adopted one, since there is no handle to kill.
        let _ = self.call("Browser.close", json!({})).await;
        if let Some(c) = self.child.lock().await.as_mut() {
            let _ = c.kill().await;
        }
    }
}

/// One frame of the live viewer.
#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
    /// CSS pixels the image covers, for mapping a click back into the page.
    pub width: f64,
    pub height: f64,
    /// Where the page is now. A person watching needs to know what they are
    /// about to click on, and it costs nothing on top of the size query.
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
}

/// A live DevTools endpoint on the profile's recorded port, if there is one.
///
/// The port file lives inside the profile directory, so a browser answering
/// there is that profile's browser. The residual risk is a stale file whose
/// port has since been taken by some other debuggable browser; requiring the
/// reply to be Chrome's own `/json/version` narrows it, and because the file
/// is deleted whenever this returns `None`, a stale one never survives a
/// launch.
///
/// The timeout is short: this runs on the path to every browser tool call,
/// and a dead port must cost a moment, not the launch timeout.
async fn adoptable(port_file: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(port_file).ok()?;
    let port: u16 = raw.lines().next()?.trim().parse().ok()?;
    let url = format!("http://127.0.0.1:{port}/json/version");
    let resp = tokio::time::timeout(ADOPT_TIMEOUT, reqwest::get(&url))
        .await
        .ok()?
        .ok()?;
    let v = tokio::time::timeout(ADOPT_TIMEOUT, resp.json::<Value>())
        .await
        .ok()?
        .ok()?;
    // Chrome's `/json/version` names itself here. Anything else listening on
    // a recycled port is not this guest's to drive.
    v.get("Browser")?.as_str()?;
    Some(port)
}

/// Chrome writes its chosen port to `DevToolsActivePort` once it is listening.
async fn wait_for_port(port_file: &Path) -> Result<u16> {
    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(s) = std::fs::read_to_string(port_file) {
            if let Some(first) = s.lines().next() {
                if let Ok(p) = first.trim().parse::<u16>() {
                    return Ok(p);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::Launch(
                "the browser never published a debugging port".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Find a page target to attach to.
async fn page_target(port: u16) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/list");
    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(list) = resp.json::<Value>().await {
                if let Some(t) = list.as_array().and_then(|a| {
                    a.iter()
                        .find(|t| t.get("type").and_then(|t| t.as_str()) == Some("page"))
                }) {
                    if let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|u| u.as_str()) {
                        return Ok(ws.to_owned());
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::Launch("no page target appeared".into()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Quote a Rust string as a JavaScript string literal.
///
/// Everything here is model-supplied, and it is being spliced into source that
/// the browser will execute. `serde_json` produces a correctly escaped JSON
/// string, which is a valid JS string literal, so quotes and backslashes
/// cannot break out of it.
fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The `--user-data-dir` Chrome will accept.
///
/// Chrome will not take a relative profile path: it starts, exits at once,
/// and the only symptom is the debugging-port file never appearing, which
/// reads as "the browser is broken" rather than "that path was relative".
/// Tests pass absolute temporary paths, so this is only observable through a
/// caller that computes its profile from a relative `--workspace`.
///
/// `absolute` rather than `canonicalize`: it needs no existing path and does
/// not produce Windows' verbatim `\?\` prefix, which brings its own
/// compatibility problems.
fn profile_dir(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Minimal base64 encoder, for putting a viewer frame in a JSON result.
pub fn b64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Minimal base64 decoder; a screenshot is the only thing that needs one.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = T.iter().position(|&t| t == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_profile_becomes_absolute_before_chrome_sees_it() {
        // A relative profile such as `./.openbot-browser` makes Chrome exit at
        // once and every browser tool fail with "never published a debugging
        // port". Tested here rather than end to end because proving it
        // against a real Chrome needs a process-global `set_current_dir`,
        // which would interfere with whatever else the suite is running.
        let out = profile_dir(Path::new("./relative-profile"));
        assert!(out.is_absolute(), "{} is still relative", out.display());
        assert!(out.ends_with("relative-profile"));
        // Windows' verbatim prefix brings its own compatibility problems, so
        // `canonicalize` is intentionally not used.
        assert!(!out.to_string_lossy().starts_with(r"\?\"));

        // An already-absolute path is left alone.
        let abs = std::env::temp_dir().join("openbot-profile");
        assert_eq!(profile_dir(&abs), abs);
    }

    #[test]
    fn js_strings_cannot_break_out_of_their_quotes() {
        // A selector is model-supplied and spliced into executing source.
        assert_eq!(js_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(js_string(r"a\b"), r#""a\\b""#);
        assert_eq!(js_string("');alert(1);('"), r#""');alert(1);('""#);
        // Newlines would otherwise terminate the statement.
        assert_eq!(js_string("a\nb"), r#""a\nb""#);
    }

    #[test]
    fn base64_round_trips_known_vectors() {
        assert_eq!(b64_decode("").unwrap(), b"");
        assert_eq!(b64_decode("TWE=").unwrap(), b"Ma");
        assert_eq!(b64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(b64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        // PNG magic, which is what a screenshot starts with.
        assert_eq!(
            &b64_decode("iVBORw0KGgo=").unwrap()[..4],
            &[0x89, b'P', b'N', b'G']
        );
    }

    #[test]
    fn base64_rejects_junk_rather_than_returning_garbage() {
        assert!(b64_decode("not valid!").is_none());
    }

    #[test]
    fn the_browser_keeps_its_own_sandbox_unless_someone_asks_otherwise() {
        // The renderer parses pages chosen by a model that may have been
        // steered by an earlier page. Chrome's sandbox is what stops a parsing
        // bug there from becoming code execution as the user — which, because
        // `shell.exec` inherits this process's environment, reaches the model
        // credential. This assertion is on an *absence*, which is exactly why it
        // has to be made here: a live-browser test that only checks the browser
        // came up passes identically either way.
        let args = chrome_args(Path::new("/tmp/p"), false);
        assert!(
            !args.iter().any(|a| a == "--no-sandbox"),
            "the default command line must not disable Chrome's sandbox: {args:?}"
        );
    }

    #[test]
    fn the_sandbox_can_still_be_turned_off_for_a_container_that_needs_it() {
        // A container running as root cannot initialise the sandbox and Chrome
        // refuses to start. Removing the escape hatch would trade one broken
        // deployment for another, so it stays — opt-in, so the person who needs
        // it is the person who set it.
        let args = chrome_args(Path::new("/tmp/p"), true);
        assert!(
            args.iter().any(|a| a == "--no-sandbox"),
            "asking for it must still work: {args:?}"
        );
    }

    #[test]
    fn the_page_to_open_stays_last_on_the_command_line() {
        // Guards the mechanism the two tests above rely on. `about:blank` is a
        // positional argument, and Chrome reads the first non-flag as the URL:
        // pushing the conditional flag after it would make Chrome treat
        // "--no-sandbox" as a second URL and silently stop honouring it, so
        // both tests would keep passing while the behaviour was gone.
        for on in [false, true] {
            let args = chrome_args(Path::new("/tmp/p"), on);
            assert_eq!(
                args.last().map(String::as_str),
                Some("about:blank"),
                "the URL must stay last (no_sandbox={on}): {args:?}"
            );
        }
    }

    #[test]
    fn only_an_explicit_yes_turns_the_sandbox_off() {
        // A variable someone left set to "0" or "no" while debugging must not
        // quietly disable it on every later run.
        for (value, expected) in [
            (Some("1"), true),
            (Some("true"), true),
            (Some("TRUE"), true),
            (Some(" 1 "), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some(""), false),
            (Some("yes"), false),
            (None, false),
        ] {
            assert_eq!(
                parse_no_sandbox(value),
                expected,
                "OPENBOT_BROWSER_NO_SANDBOX={value:?}"
            );
        }
    }

    #[test]
    fn an_explicit_browser_path_is_honoured_when_it_exists() {
        // The name of this test was a claim nothing checked. The version it
        // replaces called the search twice, discarded both results, and set an
        // override to a path that does not exist — so the one case the name
        // promises, an override that *does* exist, was never exercised at all.
        let real = std::env::current_exe().expect("the test binary exists");
        let found = resolve_browser(Some(real.clone())).expect("an existing override is honoured");
        assert_eq!(found, real, "the override must be returned unchanged");
    }

    #[test]
    fn an_override_pointing_nowhere_says_so_instead_of_no_browser_found() {
        // The bug this pins: a missing override became None, the caller turned
        // None into NotFound, and NotFound tells you to "set OPENBOT_BROWSER to
        // its path". Someone who mistyped that variable was advised to do the
        // thing they had just done, with no hint that the variable was even
        // being read. CLAUDE.md points people here when their browser is
        // somewhere unusual, so this lands on exactly the users already having
        // the hardest time.
        let bad = PathBuf::from("/definitely/not/here/chrome");
        let err = resolve_browser(Some(bad.clone())).expect_err("a missing override is an error");
        assert!(
            matches!(err, BrowserError::OverrideMissing(ref p) if *p == bad),
            "expected OverrideMissing({bad:?}), got {err:?}"
        );
        let shown = err.to_string();
        assert!(
            shown.contains("OPENBOT_BROWSER") && shown.contains("not/here"),
            "the message must name the variable and quote the path back: {shown}"
        );
        assert!(
            !shown.contains("no Chromium-family browser found"),
            "this is the message that made the two cases indistinguishable: {shown}"
        );
    }

    #[test]
    fn an_absent_override_falls_through_to_the_usual_install_locations() {
        // Whether a browser is installed on the machine running this is not
        // knowable, so this asserts the property that holds either way: with no
        // override, the answer is never OverrideMissing. Without this, a
        // resolver that reported OverrideMissing unconditionally would pass the
        // two tests above.
        assert!(
            !matches!(resolve_browser(None), Err(BrowserError::OverrideMissing(_))),
            "with no override set, the override branch must not be reachable"
        );
    }
}
