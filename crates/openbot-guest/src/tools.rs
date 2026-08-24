//! The guest's tool implementations, and the workspace confinement they run
//! under.
//!
//! Confinement is the security-critical part of this file. The guest backs a
//! remote workspace shared by every Bot on one account, so a path escape is a
//! tenant boundary violation. Resolution rejects `..` traversal, absolute
//! paths outside the root, and symlinks that point out, and it is enforced on
//! the canonicalised path, because a check on the lexical path is defeated by
//! any symlink.
//!
//! Dangling symlinks need particular care: `exists()` follows links, so a
//! broken link looks like a name that is not there, and writing through it
//! would create the target outside the workspace. The ancestor walk in
//! [`Workspace::resolve`] therefore stops on the directory entry
//! (`symlink_metadata`), so a broken link is found and refused.
//!
//! This matters beyond `fs.write`: `browser.screenshot` takes a
//! caller-supplied path and is `allow` in the shipped default policy, since a
//! screenshot is pixels of a page the agent already opened. Through a broken
//! link it would be an unapproved write to an arbitrary location. The
//! workspace is durable and shared between Bots, so a link only has to arrive
//! once.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use openbot_proto::frames::ToolDescription;
use serde_json::{json, Value};

use crate::browser::{self, Browser};

/// Cap on captured process output, matching the documented default. Beyond
/// this the output is truncated and the tool says so, rather than returning a
/// payload large enough to blow a context window.
const OUTPUT_BYTE_LIMIT: usize = 20_000;

/// Default wall-clock limit for a foreground command.
const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// What a tool call runs against: the workspace, and a browser started on
/// first use.
///
/// The browser is lazy because launching Chrome costs a second and most tasks
/// never touch it, but once launched it is kept, so a session's cookies and
/// scroll position persist across calls.
pub struct Context {
    pub ws: Workspace,
    profile: PathBuf,
    /// Intentionally replaceable rather than a `OnceCell`. If Chrome dies on
    /// its own (a crashed tab, an OOM kill, someone closing it), a `OnceCell`
    /// would hold a dead handle with no way to put a live one back, and every
    /// browser tool would fail for the rest of the guest's life.
    browser: tokio::sync::Mutex<Option<Arc<Browser>>>,
}

impl Context {
    pub fn new(ws: Workspace, profile: impl Into<PathBuf>) -> Self {
        Self {
            ws,
            profile: profile.into(),
            browser: tokio::sync::Mutex::new(None),
        }
    }

    /// The browser, launching or replacing one as needed.
    async fn browser(&self) -> Result<Arc<Browser>, ToolError> {
        let mut slot = self.browser.lock().await;
        if let Some(b) = slot.as_ref() {
            if b.is_alive() {
                return Ok(Arc::clone(b));
            }
            // Drop it before launching: on a profile whose browser merely lost
            // its connection, the replacement has to be able to adopt or take
            // the lock, and a live handle here helps with neither.
            slot.take();
        }
        let b = Arc::new(
            Browser::launch(&self.profile)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?,
        );
        *slot = Some(Arc::clone(&b));
        Ok(b)
    }

    /// The browser, only if one is already running.
    ///
    /// For the live viewer: watching a computer must not change it. Going
    /// through [`Self::browser`] would launch Chrome inside the guest the
    /// moment someone opened the viewer, a side effect an observer must not
    /// have.
    async fn browser_if_running(&self) -> Option<Arc<Browser>> {
        let slot = self.browser.lock().await;
        slot.as_ref().filter(|b| b.is_alive()).map(Arc::clone)
    }

    pub async fn shutdown(&self) {
        if let Some(b) = self.browser.lock().await.as_ref() {
            b.shutdown().await;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("invalid arguments: {0}")]
    BadArgs(String),
    #[error("path escapes the workspace: {0}")]
    Escape(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Failed(String),
}

pub struct Workspace {
    root: PathBuf,
    confine: bool,
}

/// Refuse to serve a workspace that is also the control plane's home.
///
/// `openbotd --home X` keeps `secrets.json` there. `openbot-guest --workspace X`
/// would then put every connector token inside the workspace, where `fs.read`
/// (allow-listed, no approval prompt, no trace) hands them to the model on
/// request. That defeats the point of the broker: credentials the guest can
/// use but never read.
///
/// Refusing to start is preferable to hiding those files. A denylist inside
/// the workspace leaks through `fs.list`, `shell.exec` and anything added
/// later, and there is no legitimate reason to do work inside the control
/// plane's home.
fn refuse_control_plane_home(root: &Path) -> std::io::Result<()> {
    // The root, and the one place the default `--home` puts a home inside it.
    //
    // A guest is told its workspace and nothing else, so it cannot know where
    // `--home` points; only `openbot up` knows both, and it refuses the overlap.
    // What the guest can check without walking the tree is a home one level
    // down: `--workspace .` next to a home in the same directory lands the
    // token store where a root-only check would miss it.
    //
    // This is intentionally not a scan. A model can write `notes/secrets.json`
    // into its own workspace, and a guest that refused to start over any file
    // the model named would be one the model can switch off. These are a
    // couple of extra paths, and the names are openbot's own defaults rather
    // than anything a person chose.
    let mut candidates = vec![root.to_path_buf()];
    candidates.extend(crate::DEFAULT_HOME_DIRS.iter().map(|d| root.join(d)));
    for dir in &candidates {
        for marker in ["secrets.json", "connectors.json"] {
            if dir.join(marker).exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{} is the control plane's home — it holds {marker}. Serving it as a \
                         workspace would let `fs.read` hand the model every stored credential \
                         with no approval prompt. Point --workspace somewhere else (the guest's \
                         files and openbotd's --home are different things).",
                        dir.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>, confine: bool) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        // Canonicalise once so every later comparison is against a real path.
        let root = root.canonicalize()?;
        refuse_control_plane_home(&root)?;
        Ok(Self { root, confine })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a caller-supplied path against the workspace root.
    ///
    /// Canonicalises the nearest existing ancestor and re-appends the missing
    /// tail, so a path that does not exist yet (a file about to be written) is
    /// still checked against the real location of its parent rather than a
    /// lexical guess.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let p = Path::new(raw);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };

        // Strip `.` and resolve `..` lexically first so a traversal is never
        // handed to the filesystem.
        let mut lexical = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !lexical.pop() {
                        return Err(ToolError::Escape(raw.to_owned()));
                    }
                }
                other => lexical.push(other.as_os_str()),
            }
        }

        if !self.confine {
            return Ok(lexical);
        }

        // Canonicalise the deepest existing ancestor, then re-append the tail.
        //
        // This must use `symlink_metadata`, not `exists`. `exists` follows the
        // link, so a dangling symlink (one whose target is not there yet)
        // would look like a name that does not exist, go into the
        // un-canonicalised tail, and pass the root check as `<root>/link`;
        // writing through it would create the target outside the root.
        // `symlink_metadata` asks about the directory entry instead: a broken
        // link has one, so the walk stops there, `canonicalize` fails on it,
        // and the path is refused.
        //
        // A file that genuinely does not exist has no entry either way, so a
        // path about to be written still resolves.
        let mut existing = lexical.clone();
        let mut tail = Vec::new();
        while std::fs::symlink_metadata(&existing).is_err() {
            let Some(name) = existing.file_name().map(|n| n.to_owned()) else {
                return Err(ToolError::Escape(raw.to_owned()));
            };
            tail.push(name);
            if !existing.pop() {
                return Err(ToolError::Escape(raw.to_owned()));
            }
        }
        let mut real = existing
            .canonicalize()
            .map_err(|_| ToolError::Escape(raw.to_owned()))?;
        for part in tail.into_iter().rev() {
            real.push(part);
        }

        if !real.starts_with(&self.root) {
            return Err(ToolError::Escape(raw.to_owned()));
        }
        Ok(real)
    }
}

/// The tool set this guest advertises in its `serve` snapshot.
///
/// Browser tools are only advertised when a browser is installed. Offering a
/// model a tool that cannot run wastes a turn and teaches it to distrust the
/// catalogue.
pub fn catalog() -> Vec<ToolDescription> {
    let mut tools = base_catalog();
    if browser::find_browser().is_some() {
        tools.extend(browser_catalog());
    }
    tools
}

fn browser_catalog() -> Vec<ToolDescription> {
    vec![
        ToolDescription::new(
            "browser.open",
            "Open a URL in the browser and return its title and address.",
            json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }),
        ),
        ToolDescription::new(
            "browser.read",
            "Return the readable text of the current page.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        ToolDescription::new(
            "browser.snapshot",
            "List what can be acted on, each with a ref like e1. Pass a ref to browser.click or browser.fill instead of guessing a CSS selector. Navigation invalidates refs; snapshot again after the page changes.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "How many elements at most (default 150)."
                    }
                },
                "required": []
            }),
        ),
        ToolDescription::new(
            "browser.links",
            "List the links on the current page as text and href.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        ToolDescription::new(
            "browser.click",
            "Click something. Give either a ref from browser.snapshot, or a CSS selector.",
            json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "A ref from browser.snapshot, like e3." },
                    "selector": { "type": "string", "description": "A CSS selector, if you have no ref." }
                },
                "required": []
            }),
        ),
        ToolDescription::new(
            "browser.fill",
            "Type text into a field. Give either a ref from browser.snapshot, or a CSS selector.",
            json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "A ref from browser.snapshot, like e2." },
                    "selector": { "type": "string", "description": "A CSS selector, if you have no ref." },
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        ),
        ToolDescription::new(
            "browser.screenshot",
            "Save a PNG of the current page into the workspace.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
        ToolDescription::new(
            "browser.click_at",
            "Click at a point in the page, in CSS pixels from the top-left of the \
             viewport. Use this for canvases, maps and anything that draws its own \
             controls; prefer browser.click when the thing has a selector.",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["x", "y"]
            }),
        ),
        ToolDescription::new(
            "browser.type",
            "Type text as keystrokes into whatever currently has focus. Click the \
             field first. Use browser.fill instead when you have a selector.",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        ),
        ToolDescription::new(
            "browser.key",
            "Press one named key: Enter, Tab, Escape, Backspace, Delete, Home, End, PageUp, PageDown, or an arrow.",
            json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
                "required": ["key"]
            }),
        ),
        ToolDescription::new(
            "browser.scroll",
            "Scroll the page. Positive dy scrolls down, as a wheel does.",
            json!({
                "type": "object",
                "properties": {
                    "dy": { "type": "number" },
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["dy"]
            }),
        ),
    ]
}

/// Tools that are served but intentionally not in the catalogue.
///
/// The catalogue is the model's menu, and a frame is a base64 JPEG: tens of
/// kilobytes of characters that no text model can use and that would crowd
/// out the actual task. The live viewer knows the name and calls it directly;
/// a model asking about the page wants `browser.read`.
///
/// Serving a tool that is not advertised is the same shape as a connector
/// owning its namespace while offline: the catalogue answers "what should you
/// use", not "what exists".
pub const VIEWER_ONLY_TOOLS: &[&str] = &["browser.frame"];

fn base_catalog() -> Vec<ToolDescription> {
    vec![
        ToolDescription::new(
            "fs.list",
            "List entries in a workspace directory.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Directory, relative to the workspace root." } },
                "required": []
            }),
        ),
        ToolDescription::new(
            "fs.read",
            "Read a UTF-8 text file from the workspace.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
        ToolDescription::new(
            "fs.write",
            "Write a UTF-8 text file into the workspace, creating parent directories.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "contents": { "type": "string" } },
                "required": ["path", "contents"]
            }),
        ),
        ToolDescription::new(
            "shell.exec",
            "Run a shell command in the workspace and capture its output.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600 }
                },
                "required": ["command"]
            }),
        ),
    ]
}

/// Which element an acting tool was aimed at.
///
/// `ref` wins when both are given, because a ref is the one the snapshot
/// actually handed out and a selector alongside it is a model hedging. Neither
/// is an error rather than a default: silently clicking the first thing on the
/// page would be a destructive guess, and these tools are gated by an approval
/// a person has already read and agreed to.
fn target_of(args: &Value) -> std::result::Result<browser::Target, ToolError> {
    if let Some(r) = args.get("ref").and_then(|v| v.as_str()) {
        let r = r.trim();
        let n = r
            .strip_prefix('e')
            .and_then(|d| d.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "`{r}` is not a ref. Refs come from browser.snapshot and look like e1, e2."
                ))
            })?;
        return Ok(browser::Target::Ref(n));
    }
    if let Some(sel) = args.get("selector").and_then(|v| v.as_str()) {
        if !sel.trim().is_empty() {
            return Ok(browser::Target::Selector(sel.to_owned()));
        }
    }
    Err(ToolError::Failed(
        "give either `ref` (from browser.snapshot, like e3) or `selector` (a CSS selector). browser.snapshot lists what is on the page and what each thing is called."
            .into(),
    ))
}

/// How to name that element back in the result.
fn target_label(t: &browser::Target) -> String {
    match t {
        browser::Target::Selector(s) => s.clone(),
        browser::Target::Ref(n) => format!("e{n}"),
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| ToolError::BadArgs(format!("`{key}` must be a string")))
}

/// A coordinate or delta. Accepts an integer or a float, since a model writes
/// `100` and a viewer sends `100.5` from a scaled image.
fn arg_f64(args: &Value, key: &str) -> Result<f64, ToolError> {
    args.get(key)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ToolError::BadArgs(format!("`{key}` must be a number")))
}

/// Truncate on a char boundary so the result is always valid UTF-8.
fn cap(mut s: String) -> (String, bool) {
    if s.len() <= OUTPUT_BYTE_LIMIT {
        return (s, false);
    }
    let mut end = OUTPUT_BYTE_LIMIT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

/// Execute one tool. `progress` is called for streamed updates before the
/// terminal value is returned.
pub async fn invoke(
    ctx: &Context,
    tool: &str,
    args: &Value,
    progress: &mut (dyn FnMut(Value) + Send),
) -> Result<Value, ToolError> {
    let ws = &ctx.ws;
    match tool {
        "browser.open" => {
            let url = arg_str(args, "url")?;
            // Only http(s). A `file://` would read straight through the
            // workspace confinement the filesystem tools enforce.
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ToolError::BadArgs(format!(
                    "only http and https URLs are allowed, got `{url}`"
                )));
            }
            progress(json!({ "stage": "navigating", "url": url }));
            let b = ctx.browser().await?;
            let info = b
                .navigate(&url)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            progress(json!({ "stage": "loaded" }));
            Ok(serde_json::to_value(info).unwrap_or(Value::Null))
        }

        "browser.read" => {
            let b = ctx.browser().await?;
            let text = b
                .text()
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let (text, truncated) = cap(text);
            Ok(json!({ "text": text, "truncated": truncated }))
        }

        "browser.links" => {
            let b = ctx.browser().await?;
            let links = b
                .links()
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "links": links }))
        }

        "browser.snapshot" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(150)
                .clamp(1, 1000) as usize;
            let b = ctx.browser().await?;
            b.snapshot(limit)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))
        }

        "browser.click" => {
            let target = target_of(args)?;
            let b = ctx.browser().await?;
            b.click_target(&target)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let info = b
                .info()
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "clicked": target_label(&target), "url": info.url, "title": info.title }))
        }

        "browser.fill" => {
            let target = target_of(args)?;
            let text = arg_str(args, "text")?;
            let b = ctx.browser().await?;
            b.fill_target(&target, &text)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "filled": target_label(&target), "chars": text.chars().count() }))
        }

        "browser.click_at" => {
            let x = arg_f64(args, "x")?;
            let y = arg_f64(args, "y")?;
            let b = ctx.browser().await?;
            b.click_at(x, y)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "clicked": [x, y] }))
        }

        "browser.type" => {
            let text = arg_str(args, "text")?;
            let b = ctx.browser().await?;
            b.type_text(&text)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "typed": text.chars().count() }))
        }

        "browser.key" => {
            let key = arg_str(args, "key")?;
            let b = ctx.browser().await?;
            b.key(&key)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "pressed": key }))
        }

        "browser.scroll" => {
            let dy = arg_f64(args, "dy")?;
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(10.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(10.0);
            let b = ctx.browser().await?;
            b.scroll(x, y, dy)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({ "scrolled": dy }))
        }

        // Served but never advertised; see `VIEWER_ONLY_TOOLS`.
        "browser.frame" => {
            let q = args.get("quality").and_then(|v| v.as_u64()).unwrap_or(60) as u8;
            let Some(b) = ctx.browser_if_running().await else {
                // Nothing to show, and not a reason to start a browser: an
                // observer that launches one is no longer an observer.
                return Ok(json!({ "idle": true }));
            };
            let f = b
                .frame(q)
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            Ok(json!({
                "jpeg_b64": crate::browser::b64_encode(&f.jpeg),
                "width": f.width,
                "height": f.height,
                "url": f.url,
                "title": f.title,
                "bytes": f.jpeg.len(),
            }))
        }

        "browser.screenshot" => {
            let raw = arg_str(args, "path")?;
            let path = ws.resolve(&raw)?;
            let b = ctx.browser().await?;
            let png = b
                .screenshot()
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &png)?;
            Ok(json!({ "path": raw, "bytes": png.len() }))
        }

        "fs.list" => {
            let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let dir = ws.resolve(raw)?;
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&dir)? {
                let e = e?;
                let meta = e.metadata()?;
                entries.push(json!({
                    "name": e.file_name().to_string_lossy(),
                    "kind": if meta.is_dir() { "dir" } else { "file" },
                    "bytes": if meta.is_file() { Some(meta.len()) } else { None },
                }));
            }
            entries.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_owned());
            Ok(json!({ "path": raw, "entries": entries }))
        }

        "fs.read" => {
            let raw = arg_str(args, "path")?;
            let path = ws.resolve(&raw)?;
            let bytes = std::fs::read(&path)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| ToolError::Failed(format!("{raw} is not valid UTF-8")))?;
            let (text, truncated) = cap(text);
            Ok(json!({ "path": raw, "contents": text, "truncated": truncated }))
        }

        "fs.write" => {
            let raw = arg_str(args, "path")?;
            let contents = arg_str(args, "contents")?;
            let path = ws.resolve(&raw)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, contents.as_bytes())?;
            Ok(json!({ "path": raw, "bytes_written": contents.len() }))
        }

        "shell.exec" => {
            let command = arg_str(args, "command")?;
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .map(std::time::Duration::from_secs)
                .unwrap_or(EXEC_TIMEOUT);

            progress(json!({ "stage": "starting", "command": command }));

            let mut cmd = if cfg!(windows) {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg(&command);
                c
            } else {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-lc").arg(&command);
                c
            };
            cmd.current_dir(ws.root());
            // Kill the child when the wait ends. Dropping the `output()`
            // future on timeout does not stop the process: tokio leaves a
            // child running unless told otherwise. Without this the model is
            // told "timed out" while the command is still writing files, and
            // an agent that retries stacks one orphan per attempt.
            //
            // This reaps the shell that was started. A grandchild the shell
            // spawned and detached can still outlive it: killing a whole
            // process tree needs a job object or a process group, and this
            // workspace forbids `unsafe`, which the `killpg` route requires.
            cmd.kill_on_drop(true);

            let out = tokio::time::timeout(timeout, cmd.output())
                .await
                .map_err(|_| {
                    ToolError::Failed(format!(
                        "command timed out after {timeout:?} and was killed"
                    ))
                })??;

            let (stdout, so_trunc) = cap(String::from_utf8_lossy(&out.stdout).into_owned());
            let (stderr, se_trunc) = cap(String::from_utf8_lossy(&out.stderr).into_owned());
            progress(json!({ "stage": "finished", "exit_code": out.status.code() }));

            Ok(json!({
                "exit_code": out.status.code(),
                "stdout": stdout,
                "stderr": stderr,
                "truncated": so_trunc || se_trunc,
            }))
        }

        other => Err(ToolError::Unknown(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path(), true).unwrap();
        (dir, ws)
    }

    fn temp_ctx() -> (tempfile::TempDir, Context) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path(), true).unwrap();
        let profile = dir.path().join(".browser");
        (dir, Context::new(ws, profile))
    }

    #[test]
    fn a_workspace_that_is_the_control_planes_home_is_refused() {
        // `fs.read` is allow-listed, so a control-plane home served as a
        // workspace hands over `secrets.json` with no approval prompt.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("secrets.json"),
            r#"{"linear-token":"sk-live-x"}"#,
        )
        .unwrap();

        let e = match Workspace::new(dir.path(), true) {
            Err(e) => e,
            Ok(_) => panic!("a control-plane home was served as a workspace"),
        };
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("--workspace"), "{e}");

        // The other marker file on its own is enough, since a home may have
        // connectors configured before any secret is stored.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("connectors.json"), "{}").unwrap();
        assert!(Workspace::new(dir.path(), true)
            .is_err_and(|e| e.to_string().contains("connectors.json")));
    }

    #[test]
    fn an_ordinary_workspace_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        // A file the model wrote with a confusing name must not brick the
        // guest: only the root itself is checked, not the tree.
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes/secrets.json"), "{}").unwrap();
        assert!(Workspace::new(dir.path(), true).map(|_| ()).is_ok());
    }

    #[test]
    fn resolves_relative_paths_under_the_root() {
        let (_d, ws) = temp_ws();
        let p = ws.resolve("notes/today.md").unwrap();
        assert!(p.starts_with(ws.root()));
        assert!(p.ends_with("today.md"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let (_d, ws) = temp_ws();
        for bad in ["../escape", "a/../../escape", "../../../../etc/passwd"] {
            assert!(
                matches!(ws.resolve(bad), Err(ToolError::Escape(_))),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_absolute_paths_outside_the_root() {
        let (_d, ws) = temp_ws();
        let outside = if cfg!(windows) {
            "C:\\Windows\\System32"
        } else {
            "/etc/passwd"
        };
        assert!(matches!(ws.resolve(outside), Err(ToolError::Escape(_))));
    }

    #[test]
    fn allows_absolute_paths_inside_the_root() {
        let (_d, ws) = temp_ws();
        let inside = ws.root().join("ok.txt");
        assert!(ws.resolve(inside.to_str().unwrap()).is_ok());
    }

    /// Make a symlink, or report that the platform would not allow it.
    ///
    /// Creating one needs a privilege on Windows that a plain user may not
    /// have. Returning `false` rather than failing keeps the suite runnable on
    /// such a machine; CI runs on Ubuntu and macOS, where it always works, so
    /// the property is never left unchecked everywhere at once.
    fn link(target: &Path, at: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, at).is_ok()
        }
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(target, at).is_ok()
            } else {
                std::os::windows::fs::symlink_file(target, at).is_ok()
            }
        }
    }

    /// A symlink pointing out of the workspace is not a way out.
    ///
    /// Of the three protections (`..` traversal, absolute paths, and symlinks
    /// that point out), the third is what canonicalisation exists for: a
    /// check on the lexical path is defeated by any symlink, because lexically
    /// `ws/link/x` is under `ws`.
    #[test]
    fn rejects_a_symlink_that_points_out_of_the_root() {
        let (d, ws) = temp_ws();
        // Outside the workspace, and beside it rather than far away: a root
        // check written with string prefixes would let `.../wsX` through.
        let outside = d.path().parent().expect("a parent").join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        let at = ws.root().join("link.txt");
        if !link(&outside, &at) {
            eprintln!("skipping: this platform would not create a symlink");
            return;
        }

        assert!(
            matches!(ws.resolve("link.txt"), Err(ToolError::Escape(_))),
            "a symlink out of the workspace resolved to {:?}",
            ws.resolve("link.txt")
        );
        std::fs::remove_file(&outside).ok();
    }

    /// The same through a directory, which is the shape that reaches a whole
    /// tree rather than one file.
    #[test]
    fn rejects_a_path_that_leaves_through_a_linked_directory() {
        let (d, ws) = temp_ws();
        let outside = d.path().parent().expect("a parent").join("outside-dir");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        let at = ws.root().join("out");
        if !link(&outside, &at) {
            eprintln!("skipping: this platform would not create a symlink");
            std::fs::remove_dir_all(&outside).ok();
            return;
        }

        for bad in ["out/secret.txt", "out"] {
            assert!(
                matches!(ws.resolve(bad), Err(ToolError::Escape(_))),
                "`{bad}` reached outside the workspace: {:?}",
                ws.resolve(bad)
            );
        }
        std::fs::remove_dir_all(&outside).ok();
    }

    /// A symlink that stays inside is ordinary and must keep working; a
    /// confinement that refuses everything is not confinement.
    #[test]
    fn allows_a_symlink_that_stays_inside_the_root() {
        let (_d, ws) = temp_ws();
        std::fs::write(ws.root().join("real.txt"), "fine").unwrap();
        let at = ws.root().join("alias.txt");
        if !link(&ws.root().join("real.txt"), &at) {
            eprintln!("skipping: this platform would not create a symlink");
            return;
        }
        let p = ws.resolve("alias.txt").expect("a link inside is allowed");
        assert!(p.starts_with(ws.root()), "{p:?}");
    }

    /// A symlink whose target does not exist yet.
    ///
    /// The tail of a path is re-appended without canonicalisation, because it
    /// is the part that does not exist (a file about to be written). A
    /// dangling link is where that reasoning meets a link: `exists()` follows
    /// the link, so a broken one looks like a name that is not there, and
    /// writing through it would create the target outside the root.
    #[test]
    fn a_dangling_symlink_out_of_the_root_is_not_a_way_out() {
        let (d, ws) = temp_ws();
        let outside = d.path().parent().expect("a parent").join("not-yet.txt");
        std::fs::remove_file(&outside).ok();

        let at = ws.root().join("dangling.txt");
        if !link(&outside, &at) {
            eprintln!("skipping: this platform would not create a symlink");
            return;
        }

        let got = ws.resolve("dangling.txt");
        assert!(
            matches!(got, Err(ToolError::Escape(_))),
            "a link to a file that does not exist yet resolved to {got:?}; \
             writing through it creates that file outside the workspace"
        );
        std::fs::remove_file(&outside).ok();
    }

    /// A broken link is refused even when it points inside the workspace.
    ///
    /// Stopping the walk at the directory entry refuses every broken link,
    /// including one whose target would be inside the workspace, where
    /// writing through it would be harmless. Allowing that would mean reading
    /// the link target and checking it separately: more code on the path that
    /// decides whether a write leaves the workspace, for a case nothing in this
    /// product produces, since the tools never create links.
    ///
    /// Fail closed, and pinned so the narrowing is a recorded decision.
    #[test]
    fn a_broken_link_is_refused_even_pointing_inside() {
        let (_d, ws) = temp_ws();
        let at = ws.root().join("alias.txt");
        if !link(&ws.root().join("not-written-yet.txt"), &at) {
            eprintln!("skipping: this platform would not create a symlink");
            return;
        }
        assert!(
            matches!(ws.resolve("alias.txt"), Err(ToolError::Escape(_))),
            "a broken link pointing inside is documented as refused; if this now succeeds the trade-off changed"
        );
    }

    /// A workspace that contains the control plane's home is refused.
    ///
    /// `openbotd --home` defaults to `./openbot-data`, so `openbot up --workspace .`
    /// (a plausible way to say "work in this directory") puts the token store
    /// one level down, where a root-only marker check would never look and
    /// `fs.read` is allow-listed with no approval prompt.
    #[test]
    fn a_workspace_holding_the_control_plane_home_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("openbot-data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("secrets.json"), r#"{"stripe":"sk-live"}"#).unwrap();

        let made = Workspace::new(dir.path(), true);
        assert!(
            made.is_err(),
            "a workspace containing the control plane's home was served"
        );
    }

    #[test]
    fn unconfined_mode_permits_escape_for_local_dev() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path(), false).unwrap();
        assert!(ws.resolve("../anywhere").is_ok());
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let (_d, ws) = temp_ctx();
        let mut noop = |_: Value| {};
        invoke(
            &ws,
            "fs.write",
            &json!({"path":"a/b.txt","contents":"hello"}),
            &mut noop,
        )
        .await
        .unwrap();
        let got = invoke(&ws, "fs.read", &json!({"path":"a/b.txt"}), &mut noop)
            .await
            .unwrap();
        assert_eq!(got["contents"], "hello");
        assert_eq!(got["truncated"], false);
    }

    #[tokio::test]
    async fn write_outside_the_workspace_is_refused() {
        let (_d, ws) = temp_ctx();
        let mut noop = |_: Value| {};
        let e = invoke(
            &ws,
            "fs.write",
            &json!({"path":"../pwned","contents":"x"}),
            &mut noop,
        )
        .await;
        assert!(matches!(e, Err(ToolError::Escape(_))));
    }

    #[tokio::test]
    async fn list_reports_files_and_dirs() {
        let (_d, ws) = temp_ctx();
        let mut noop = |_: Value| {};
        invoke(
            &ws,
            "fs.write",
            &json!({"path":"x.txt","contents":"hi"}),
            &mut noop,
        )
        .await
        .unwrap();
        let got = invoke(&ws, "fs.list", &json!({}), &mut noop).await.unwrap();
        let names: Vec<_> = got["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"x.txt"));
    }

    #[tokio::test]
    async fn exec_captures_output_and_emits_progress() {
        let (_d, ws) = temp_ctx();
        let mut seen = Vec::new();
        let mut p = |v: Value| seen.push(v);
        let got = invoke(
            &ws,
            "shell.exec",
            &json!({"command":"echo openbot"}),
            &mut p,
        )
        .await
        .unwrap();
        assert_eq!(got["exit_code"], 0);
        assert!(got["stdout"].as_str().unwrap().contains("openbot"));
        assert_eq!(
            seen.len(),
            2,
            "expected starting + finished progress frames"
        );
        assert_eq!(seen[0]["stage"], "starting");
    }

    #[tokio::test]
    async fn unknown_tool_is_reported_as_such() {
        let (_d, ws) = temp_ctx();
        let mut noop = |_: Value| {};
        assert!(matches!(
            invoke(&ws, "fs.delete_everything", &json!({}), &mut noop).await,
            Err(ToolError::Unknown(_))
        ));
    }

    #[test]
    fn output_cap_truncates_on_a_char_boundary() {
        let s = "é".repeat(OUTPUT_BYTE_LIMIT); // 2 bytes each
        let (out, truncated) = cap(s);
        assert!(truncated);
        assert!(out.len() <= OUTPUT_BYTE_LIMIT);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn catalog_ids_are_unique() {
        let c = catalog();
        let mut ids: Vec<_> = c.iter().map(|t| t.tool_id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate tool ids in the catalog");
    }
}

#[cfg(all(test, windows))]
mod windows_paths {
    use super::*;

    /// Canary: reserved device names behave as files inside the workspace.
    ///
    /// `CON`, `NUL`, `COM1`, `PRN` and `AUX` are devices to Win32 in every
    /// directory; classically, opening `<anywhere>\CON` gets the console
    /// rather than a file. A jail that checks the path and then hands it to
    /// `File::create` would let a Bot write to a device with every containment
    /// test still passing, because the path really is inside the root.
    ///
    /// On current Windows and std, writing through the resolved path leaves a
    /// real directory entry with the bytes in it, and this holds with or
    /// without the extended-length `\?\` path that `canonicalize` yields. So
    /// this asserts a property of the platform, not of `resolve`. It is kept
    /// because if a future Windows or std does route these names to devices,
    /// this fails and names the reason, and `resolve` would then need to
    /// refuse reserved names itself.
    ///
    /// The check reads the directory, not the file: reading back
    /// `root.join("NUL")` reports an empty string whether the write reached a
    /// file or the device, because a plain path opens the device even when a
    /// file of that name exists beside it. `read_dir` lists actual entries and
    /// cannot be answered by a device.
    #[test]
    fn a_reserved_device_name_is_a_file_not_a_device() {
        for name in ["CON", "NUL", "COM1", "PRN", "AUX"] {
            let dir = tempfile::tempdir().unwrap();
            let ws = Workspace::new(dir.path(), true).unwrap();

            let resolved = ws
                .resolve(name)
                .unwrap_or_else(|_| panic!("{name} is inside the workspace and was refused"));
            assert!(
                resolved.starts_with(ws.root()),
                "{name} resolved outside the workspace: {}",
                resolved.display()
            );

            let body = format!("body-{name}");
            std::fs::write(&resolved, &body)
                .unwrap_or_else(|e| panic!("writing {name} failed: {e}"));

            let entries: Vec<String> = std::fs::read_dir(dir.path())
                .expect("the workspace is readable")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                entries.iter().any(|e| e == name),
                "writing `{name}` left nothing in the workspace; it went to the device. \
                 Entries: {entries:?}"
            );
            assert_eq!(
                std::fs::read_to_string(&resolved).ok().as_deref(),
                Some(body.as_str()),
                "`{name}` exists but did not keep what was written to it"
            );
        }
    }
}
