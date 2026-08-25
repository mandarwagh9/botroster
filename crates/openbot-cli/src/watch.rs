//! `openbot watch`: see the Bot's computer, and take it back when needed.
//!
//! A small local web server bridging a browser page to the hub:
//!
//! ```text
//! the browser ──HTTP──► openbot watch ──WebSocket──► hub ──► the guest
//!    frames ◄── JPEG        (this file)         browser.frame
//!    clicks ──► /input                          browser.click_at
//! ```
//!
//! # Why this port is dangerous, and what makes it safe
//!
//! This server can drive a computer that is signed into things. It listens on
//! loopback, but loopback is not a security boundary in a browser: any page
//! the user visits can `fetch("http://127.0.0.1:7777/input", …)`. The response
//! is blocked by CORS, but the click still happens. Three defences:
//!
//! 1. A capability in the URL. Every request must carry `?k=<token>`, freshly
//!    generated per run and printed once. A page that cannot guess it cannot
//!    use this server. (The same shape as Jupyter's token.)
//! 2. The `Host` header must be loopback. Otherwise `evil.com` resolving to
//!    127.0.0.1 (DNS rebinding) turns a same-origin page into a client, and
//!    the token becomes readable rather than merely unguessable.
//! 3. Bound to 127.0.0.1, never `0.0.0.0`, so nothing off this machine can
//!    reach it at all.
//!
//! # Approvals
//!
//! A person clicking in the viewer would otherwise be asked to approve every
//! click. This process answers for its own calls, but only for the four input
//! tools and only while it holds the takeover; everything else is denied. That
//! is not a privilege escalation: an approval is a client-side answer to a hub
//! question, and any harness can already answer `--approve auto`. What must
//! never happen is the hub trusting a client's claim, and it does not.

use std::sync::Arc;

use openbot_agent::{ApprovalHandler, HubClient};
use openbot_proto::approval::{ApprovalDecision, ApprovalRequestParams, SecretRequestParams};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// The page's input kinds, the tool each becomes, and the arguments it carries.
///
/// A single list: `/input` maps with it and `ViewerApprovals` answers from it,
/// so "the viewer sends this" and "the viewer answers for this" cannot drift
/// apart. Drift would be silent both ways, because the page posts with
/// `.catch(() => {})`: an unhandled kind (400) and a self-denied call (502)
/// both reach a person as a click that did nothing.
type Args = fn(&Value) -> Value;
const INPUT_KINDS: &[(&str, &str, Args)] = &[
    (
        "click",
        "browser.click_at",
        |b| json!({ "x": num(b, "x"), "y": num(b, "y") }),
    ),
    (
        "scroll",
        "browser.scroll",
        |b| json!({ "x": num(b, "x"), "y": num(b, "y"), "dy": num(b, "dy") }),
    ),
    (
        "type",
        "browser.type",
        |b| json!({ "text": text(b, "text") }),
    ),
    ("key", "browser.key", |b| json!({ "key": text(b, "key") })),
];

/// Frames are a read, arrive unprompted, and are how the viewer works at all.
const FRAME_TOOL: &str = "browser.frame";

/// The tool and arguments for one of the page's input posts.
fn input_tool(body: &Value) -> Option<(&'static str, Value)> {
    let kind = body.get("kind").and_then(Value::as_str)?;
    INPUT_KINDS
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, tool, args)| (*tool, args(body)))
}

/// Whether this is a tool the viewer answers for on the person's own behalf.
fn answers_for(tool: &str) -> bool {
    tool == FRAME_TOOL || INPUT_KINDS.iter().any(|(_, t, _)| *t == tool)
}

pub struct Watch {
    hub: Arc<HubClient>,
    server: String,
    token: String,
    /// Whether this viewer currently holds the computer. Shared with the
    /// approval handler, which must not answer for input while not driving.
    driving: Arc<std::sync::atomic::AtomicBool>,
    /// One tool call at a time. The guest's browser is a single page; two
    /// clicks racing produce an order nobody asked for.
    lock: Mutex<()>,
    calls: std::sync::atomic::AtomicU64,
}

/// Answers only for the person's own input, and only while they are driving.
///
/// Deciding on the tool's name alone is enough because a call cannot arrive
/// here unless this viewer made it. The hub addresses an approval to the
/// connection that asked (`Hub::ask_owner`, held by
/// `an_approval_is_never_shown_to_a_bystander` in openbotd), so a Bot typing
/// into the same page at the same moment is never put to this handler.
/// `browser.type` reaching here is always a person's own keystroke, which is
/// what makes allowing it without a prompt consent rather than a hole. If that
/// ever stops being true, this handler needs to know which call it is
/// answering for, and no amount of tightening `INPUT_KINDS` would substitute.
struct ViewerApprovals {
    driving: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl ApprovalHandler for ViewerApprovals {
    async fn decide(&self, req: &ApprovalRequestParams) -> ApprovalDecision {
        let tool = req.tool_id.as_str();
        let mine = answers_for(tool);
        let driving = self.driving.load(std::sync::atomic::Ordering::SeqCst);
        // `browser.frame` is a read and is allow-listed anyway; the rest are
        // the person's own keystrokes, which they consent to by typing them.
        // Anything else arriving on this connection is not something a viewer
        // does, so it is refused rather than waved through.
        if mine && (driving || tool == FRAME_TOOL) {
            ApprovalDecision::allow_once()
        } else {
            ApprovalDecision::deny()
                .with_note("the viewer only answers for input while you are driving")
        }
    }

    /// The viewer is not a place to hand over a credential. Implemented
    /// explicitly rather than inherited.
    ///
    /// The default `supply` already refuses, but for a different reason: there
    /// is no value an unattended handler could invent. The viewer has a person
    /// in front of it, so that argument does not apply here.
    ///
    /// This connection answers for the person's own keystrokes while they are
    /// driving and for nothing else, the same rule `decide` is built on. A
    /// credential request is not a keystroke the person is performing; it is
    /// a question from a turn that some other connection is running, and that
    /// connection's own approver is the one holding it. Answering here would
    /// put a credential on the viewer's loopback HTTP channel to reach a
    /// prompt that is already open somewhere else.
    async fn supply(&self, _req: &SecretRequestParams) -> Option<String> {
        None
    }
}

impl Watch {
    pub async fn connect(hub_url: &str, server: &str) -> anyhow::Result<Arc<Self>> {
        let driving = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (hub, _progress) = HubClient::connect_with(
            hub_url,
            Arc::new(ViewerApprovals {
                driving: Arc::clone(&driving),
            }),
        )
        .await
        .map_err(|e| crate::up::unreachable(hub_url, &e))?;
        hub.open_session().await?;
        // A viewer with no computer bound has nothing to show. Reported here
        // rather than as an empty screen the person has to interpret.
        hub.bind_server(server).await.map_err(|e| {
            anyhow::anyhow!("cannot watch `{server}`: {e}\n  is openbot-guest running?")
        })?;

        Ok(Arc::new(Self {
            hub,
            server: server.to_owned(),
            // uuid v4 is OS randomness; a predictable token is the same as no
            // token, since guessing it is the whole attack.
            token: uuid::Uuid::new_v4().to_string(),
            driving,
            lock: Mutex::new(()),
            calls: std::sync::atomic::AtomicU64::new(0),
        }))
    }

    pub fn url(&self, addr: std::net::SocketAddr) -> String {
        format!("http://{addr}/?k={}", self.token)
    }

    async fn call(&self, tool: &str, args: Value) -> anyhow::Result<Value> {
        let _held = self.lock.lock().await;
        let n = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = openbot_proto::ToolCallId::new(format!("watch-{n}"));
        Ok(self.hub.call_tool(tool, &id, args).await?)
    }

    /// Give the computer back before exiting.
    ///
    /// Politeness only: the hub releases when this connection drops, which
    /// covers the cases where nothing gets to run at all.
    pub async fn stop(&self) -> anyhow::Result<()> {
        if self.driving.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = self.hub.give_back(&self.server).await;
        }
        Ok(())
    }

    /// Bind the viewer's port, on loopback and nowhere else.
    ///
    /// The third of the three defences above, owned by the file that claims
    /// it. The address is intentionally not a parameter: a caller that could
    /// pass one would be a feature request away from `0.0.0.0` ("let me watch
    /// from my phone"), and then the token is the only thing between a
    /// signed-in computer and the network.
    ///
    /// # Errors
    /// If the port is taken.
    pub async fn listen(port: u16) -> std::io::Result<tokio::net::TcpListener> {
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
    }

    /// Serve until the process ends.
    pub async fn serve(self: Arc<Self>, listener: tokio::net::TcpListener) {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = me.one(sock).await {
                    tracing::debug!(error = %e, "viewer connection ended");
                }
            });
        }
    }

    async fn one(&self, mut sock: tokio::net::TcpStream) -> anyhow::Result<()> {
        let Some(req) = read_request(&mut sock).await? else {
            return Ok(());
        };

        // Rebinding defence: `evil.com` pointed at 127.0.0.1 arrives with its
        // own Host, and would otherwise be same-origin with this server.
        if !host_is_loopback(&req.host) {
            return respond(&mut sock, 403, "text/plain", b"bad host").await;
        }
        // Every route, including the page itself. Serving the HTML without the
        // key would hand an attacker the shape of the API and a same-origin
        // place to call it from.
        if !token_matches(&self.token, req.query_token.as_deref()) {
            return respond(&mut sock, 403, "text/plain", b"bad or missing ?k= token").await;
        }

        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => {
                respond(
                    &mut sock,
                    200,
                    "text/html; charset=utf-8",
                    include_str!("watch.html").as_bytes(),
                )
                .await
            }

            ("GET", "/frame") => {
                let out = match self.call("browser.frame", json!({ "quality": 62 })).await {
                    Ok(v) => v,
                    Err(e) if e.to_string().contains("-32006") => json!({ "locked": true }),
                    Err(e) => {
                        return respond(&mut sock, 502, "text/plain", e.to_string().as_bytes())
                            .await
                    }
                };
                json_out(&mut sock, &out).await
            }

            ("POST", "/takeover") => {
                let why = req
                    .json()
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("a person is at the keyboard")
                    .to_owned();
                match self.hub.take_over(&self.server, &why).await {
                    Ok(v) => {
                        self.driving
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        tracing::info!(reason = %why, "you have the computer");
                        json_out(&mut sock, &json!({ "claimed": v })).await
                    }
                    Err(e) => respond(&mut sock, 409, "text/plain", e.to_string().as_bytes()).await,
                }
            }

            ("POST", "/release") => {
                // Cleared first: if the release call fails, this handler must
                // not keep answering approvals as though a person were here.
                self.driving
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                match self.hub.give_back(&self.server).await {
                    Ok(v) => json_out(&mut sock, &json!({ "released": v })).await,
                    Err(e) => respond(&mut sock, 502, "text/plain", e.to_string().as_bytes()).await,
                }
            }

            ("POST", "/input") => {
                if !self.driving.load(std::sync::atomic::Ordering::SeqCst) {
                    // The page hides this, but the page is not the boundary.
                    return respond(&mut sock, 409, "text/plain", b"take control first").await;
                }
                let b = req.json();
                let Some((tool, args)) = input_tool(&b) else {
                    // Named, because the page's `.catch(() => {})` means this
                    // is the only place the kind is ever said out loud.
                    let kind = b.get("kind").and_then(Value::as_str).unwrap_or("(none)");
                    let msg = format!("unknown input kind `{kind}`");
                    return respond(&mut sock, 400, "text/plain", msg.as_bytes()).await;
                };
                match self.call(tool, args).await {
                    Ok(v) => json_out(&mut sock, &v).await,
                    Err(e) => respond(&mut sock, 502, "text/plain", e.to_string().as_bytes()).await,
                }
            }

            _ => respond(&mut sock, 404, "text/plain", b"no").await,
        }
    }
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

struct HttpRequest {
    method: String,
    path: String,
    query_token: Option<String>,
    host: String,
    body: Vec<u8>,
}

impl HttpRequest {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Compare the token without leaking where it stopped matching.
///
/// `==` on strings short-circuits at the first differing byte, which is a
/// timing oracle: a local page could in principle learn the token one
/// character at a time rather than guessing 122 bits at once. Over loopback
/// HTTP the difference is buried in syscall noise; this is the cost of one
/// `fold` against a class of attack, on the door to a computer that is signed
/// into things.
fn token_matches(expected: &str, got: Option<&str>) -> bool {
    let Some(got) = got else { return false };
    // Length is not a secret (it is a constant of the format), so comparing
    // it first is free.
    if got.len() != expected.len() {
        return false;
    }
    expected
        .bytes()
        .zip(got.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Loopback by literal address only. A name that resolves to 127.0.0.1 is
/// exactly the rebinding case this exists to refuse, so `localhost` is accepted
/// but nothing else is.
fn host_is_loopback(host: &str) -> bool {
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let name = name.trim_matches(|c| c == '[' || c == ']');
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> anyhow::Result<Option<HttpRequest>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    // Read until the end of the headers, with a cap: an unbounded read on a
    // socket anyone can open is a way to spend all this process's memory.
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 32 * 1024 {
            anyhow::bail!("request head too large");
        }
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let start = lines.next().unwrap_or_default();
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), q.to_owned()),
        None => (target, String::new()),
    };
    let query_token = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("k=").map(percent_decode));

    let mut host = String::new();
    let mut len = 0usize;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "host" => host = v.trim().to_owned(),
            "content-length" => len = v.trim().parse().unwrap_or(0),
            _ => {}
        }
    }

    let mut body = buf[head_end + 4..].to_vec();
    if len > 1024 * 1024 {
        anyhow::bail!("body too large");
    }
    while body.len() < len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }

    Ok(Some(HttpRequest {
        method,
        path,
        query_token,
        host,
        body,
    }))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

async fn json_out(sock: &mut tokio::net::TcpStream, v: &Value) -> anyhow::Result<()> {
    respond(sock, 200, "application/json", v.to_string().as_bytes()).await
}

async fn respond(
    sock: &mut tokio::net::TcpStream,
    status: u16,
    kind: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.flush().await?;
    Ok(())
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        502 => "Bad Gateway",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openbot_proto::approval::Decision;

    #[test]
    fn only_literal_loopback_hosts_are_accepted() {
        for good in [
            "127.0.0.1:7777",
            "localhost:7777",
            "127.0.0.1",
            "[::1]:7777",
        ] {
            assert!(host_is_loopback(good), "{good} was rejected");
        }
        // The rebinding case: a name that resolves to 127.0.0.1 arrives here
        // as itself, and must not be treated as local.
        for bad in [
            "evil.com",
            "localtest.me:7777",
            "127.0.0.1.nip.io:7777",
            "attacker.localhost.evil.com",
            "",
        ] {
            assert!(!host_is_loopback(bad), "{bad} was accepted");
        }
    }

    /// The viewer answers for keystrokes, never for a credential.
    ///
    /// A secret request is a different method and cannot reach this handler
    /// at all, so this guards the list rather than the wire. `INPUT_KINDS` is
    /// where "the viewer should handle secrets too" would naturally be added,
    /// and a viewer that auto-answered a credential request would be
    /// supplying one on a person's behalf without asking them, the opposite
    /// of what the broker is for.
    #[test]
    fn the_viewer_never_answers_for_anything_but_input() {
        for tool in answered_for() {
            assert!(
                tool.starts_with("browser."),
                "`{tool}` is not a browser input; the viewer answers for it without asking the person"
            );
        }
        assert!(
            !answers_for("secret.request"),
            "the viewer would supply a credential on somebody's behalf"
        );
    }

    /// Every input the shipped page sends is one this server handles.
    ///
    /// The page is `include_str!`d, so this reads the exact bytes that ship
    /// rather than a copy of them.
    ///
    /// This has to be a build-time check because there is no runtime signal:
    /// every post in the page ends `.catch(() => {})`, so a kind the server
    /// does not know (400) reaches a person as a click that did nothing.
    /// Swallowing is right for a scroll that lost a race (a toast per dropped
    /// wheel event would be worse than silence), but it means a mismatch here
    /// is invisible until somebody wonders why typing stopped working.
    #[test]
    fn every_input_the_page_sends_is_one_this_server_handles() {
        const PAGE: &str = include_str!("watch.html");
        let sites = PAGE.matches(r#"api("/input""#).count();
        let sent: Vec<&str> = PAGE
            .match_indices(r#"api("/input", {kind: ""#)
            .map(|(at, m)| {
                let rest = &PAGE[at + m.len()..];
                &rest[..rest.find('"').expect("the kind's string is closed")]
            })
            .collect();

        // Not "did the scan find anything" but "did it find one per call
        // site". A page reformatted to `{ kind:` would still parse some kinds
        // and quietly stop reading others, and the missing ones would surface
        // below as unhandled inputs: a defect that is not there, reported
        // against code that is fine.
        assert_eq!(
            sent.len(),
            sites,
            concat!(
                "the page posts to /input {} times but this scan read {} kinds ({:?}); ",
                "the page changed shape and this test no longer checks what it says it checks"
            ),
            sites,
            sent.len(),
            sent
        );

        for kind in &sent {
            assert!(
                input_tool(&json!({ "kind": kind })).is_some(),
                "the page sends `{kind}`, which /input answers 400 to and the page then swallows"
            );
        }
        for (kind, tool, _) in INPUT_KINDS {
            assert!(
                sent.contains(kind),
                "`{kind}` maps to `{tool}` but nothing in the page sends it"
            );
        }
    }

    /// Every tool the viewer answers for, read from the one list that defines
    /// them, so a tool added to the table is covered here without being named
    /// here.
    fn answered_for() -> Vec<&'static str> {
        INPUT_KINDS
            .iter()
            .map(|(_, tool, _)| *tool)
            .chain(std::iter::once(FRAME_TOOL))
            .collect()
    }

    /// The viewer's port must not be reachable from the network.
    ///
    /// Third of the three defences in this file's header. A listener on
    /// `0.0.0.0` would leave the token as the only thing between a signed-in
    /// computer and anyone who can route to this machine.
    #[test]
    fn the_viewer_binds_loopback_and_nothing_else() {
        let rt = tokio::runtime::Runtime::new().expect("a runtime");
        let listener = rt.block_on(Watch::listen(0)).expect("an ephemeral port");
        let addr = listener.local_addr().expect("an address");
        assert!(
            addr.ip().is_loopback(),
            "the viewer bound {addr}, which is reachable from off this machine"
        );
    }

    /// This checks the answers, not the timing.
    ///
    /// Replacing `==` with a fold buys the constant-time property, and no test
    /// here can see it: `expected == got` would pass every assertion below,
    /// because only the clock changes. This test is not a guard on the timing
    /// property.
    ///
    /// What it does guard is what hand-rolling a comparison risks: getting the
    /// answers wrong. Dropping the length check would make a prefix compare
    /// equal, and that is caught here.
    #[test]
    fn a_hand_rolled_token_comparison_still_answers_correctly() {
        let real = "6f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8";
        assert!(token_matches(real, Some(real)));
        assert!(!token_matches(real, None), "a missing token is not a match");
        assert!(!token_matches(real, Some("")), "nor is an empty one");
        // Right length, wrong at the very end and at the very start: both are
        // simply "no", and the function reads every byte either way.
        let mut nearly = real.to_owned();
        nearly.pop();
        nearly.push('9');
        assert!(!token_matches(real, Some(&nearly)));
        assert!(!token_matches(real, Some(&format!("X{}", &real[1..]))));
        assert!(
            !token_matches(real, Some(&format!("{real}extra"))),
            "a longer string that starts with the token is not the token"
        );
    }

    #[test]
    fn tokens_are_decoded_from_the_query() {
        assert_eq!(percent_decode("a%2Db"), "a-b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[tokio::test]
    async fn the_viewer_answers_only_for_input_and_only_while_driving() {
        use openbot_proto::ToolId;

        let driving = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = ViewerApprovals {
            driving: Arc::clone(&driving),
        };
        let ask = |tool: &str| ApprovalRequestParams {
            approval_id: openbot_proto::approval::ApprovalId::new("a1"),
            tool_id: ToolId::new(tool),
            call_id: openbot_proto::ToolCallId::new("c1"),
            args: json!({}),
            reason: "test".into(),
            timeout_secs: 120,
        };

        // Not driving: a click is nobody's intent, so it is refused.
        assert_eq!(
            h.decide(&ask("browser.click_at")).await.decision,
            Decision::Deny
        );
        // Frames are a read, and are how the viewer works at all.
        assert_ne!(
            h.decide(&ask("browser.frame")).await.decision,
            Decision::Deny
        );

        driving.store(true, std::sync::atomic::Ordering::SeqCst);
        for t in answered_for() {
            assert_ne!(h.decide(&ask(t)).await.decision, Decision::Deny, "{t}");
        }
        // Still refused while driving: a viewer does not run shell commands,
        // and this connection must not become a way to skip the prompt.
        for t in ["shell.exec", "fs.write", "bot.send", "linear__create_issue"] {
            assert_eq!(h.decide(&ask(t)).await.decision, Decision::Deny, "{t}");
        }
    }
}
