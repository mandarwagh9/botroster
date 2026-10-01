//! The frames an upstream tool server and harness expect, against a real hub.
//!
//! Slice B2. Slice B1 got a peer through the handshake; `harness --through list`
//! then failed with `tool server did not answer session.bind in time`. That is
//! this file's first test, and the cause was not guessable from either side's
//! type definitions:
//!
//! the upstream SDK reads the session for a `session.bind` from
//! **`/params/session_id`** and, when it is absent, `continue`s without replying
//! (`xai-computer-hub-sdk/src/server.rs:1605-1610`). This hub sent an empty body
//! with the session only on the request envelope, so the peer dropped the bind
//! in silence and the hub waited out its 30-second timeout.
//!
//! Worth setting beside the `session_open` case from B1, because the two point
//! in opposite directions. Upstream's `session_open` params have **no** session
//! field and ride it on the envelope; upstream's `session.bind` reads it from
//! **params**. Two session-carrying frames, two conventions, and a hub that
//! followed only one of them looked broken in a way that read like a timeout
//! rather than like a disagreement.
//!
//! The tool server below is hand-written to upstream's rule rather than being
//! the SDK itself, because under D1 = A no upstream code enters this repository
//! and because the rule is two lines: read `/params/session_id`, reply if it is
//! there, drop it silently if it is not. Each test cites the file the shape came
//! from.

use std::time::Duration;

use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, SessionId,
};
use botrosterd::hub::Admission;
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const TOKEN: &str = "the-hub-token";

/// A tool server that behaves the way the published SDK does.
///
/// `session.bind` is answered **only** when the session is in `params`, which is
/// what upstream does and the entire point of this file. A test that used a
/// lenient server would pass against a hub that is still wrong.
struct UpstreamToolServer {
    server_id: String,
    tool_name: String,
    /// Every `session.bind` this server was asked to serve, for the tests that
    /// assert on what actually arrived.
    seen_binds: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl UpstreamToolServer {
    async fn start(url: &str, server_id: &str, tool_name: &str) -> Self {
        let seen_binds = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let url = url.to_owned();
        let server_id = server_id.to_owned();
        let tool_name = tool_name.to_owned();
        let keep_tool = tool_name.clone();
        let seen = std::sync::Arc::clone(&seen_binds);

        let hello = Hello::tool_server(server_id.as_str()).with_description("upstream-shaped peer");
        tokio::spawn(async move {
            let Ok((mut req, _)) = connect_async(&url).await else {
                return;
            };
            let mut hello = hello;
            if let Some(t) = hello.token.as_mut() {
                *t = TOKEN.to_owned();
            }
            if req
                .send(Message::Text(serde_json::to_string(&hello).unwrap()))
                .await
                .is_err()
            {
                return;
            }
            // The ack. Read once and discarded: this server has nothing to say
            // about it.
            if !matches!(req.next().await, Some(Ok(Message::Text(_)))) {
                return;
            }

            while let Some(Ok(Message::Text(t))) = req.next().await {
                let Ok(frame) = Frame::decode(&t) else {
                    continue;
                };
                let Frame::Request(r) = frame else { continue };
                if r.parsed_method() != Some(Method::SessionBind) {
                    continue;
                }
                let params = r.params.clone().unwrap_or(serde_json::Value::Null);
                if let Ok(mut v) = seen.lock() {
                    v.push(serde_json::json!({
                        "params": params.clone(),
                        "envelope_session_id": r.session_id.clone(),
                    }));
                }
                // Upstream's frame router, verbatim (`demux.rs:379-395`): a frame
                // carrying an envelope `session_id` goes to a per-session inbox,
                // and only a frame without one reaches the notification channel
                // that answers binds. This is the rule the first version of this
                // fixture did not model, and modelling it is what caught the hub
                // sending the session in both places — a frame with both is
                // routed away from the code that would have answered it.
                if r.session_id.is_some() {
                    continue;
                }
                // And the session comes from the PARAMS (`server.rs:1605-1610`).
                // Absent means the bind is dropped silently, with no error.
                let Some(sid) = params.get("session_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let _ = sid;
                let reply = SessionBindResult {
                    tools: vec![ToolDescription::new(
                        tool_name.as_str(),
                        "an upstream-shaped tool",
                        json!({ "type": "object", "properties": {} }),
                    )],
                    binary_version: Some("peer-0.1.0".to_owned()),
                };
                let out = Response::ok(r.id.clone(), reply);
                if req
                    .send(Message::Text(Frame::Response(out).encode()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });

        Self {
            server_id: server_id.to_owned(),
            tool_name: keep_tool,
            seen_binds,
        }
    }

    fn tool_name(&self) -> &str {
        &self.tool_name
    }
}

async fn hub() -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let hub = Hub::with_policy(Policy::default())
        .admitting(Admission::Token(TOKEN.to_owned()))
        // A short bind timeout: the failure this file exists for is a 30-second
        // hang, and a test should not spend 30 seconds proving it.
        .binding_within(std::time::Duration::from_secs(5));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    let handle =
        tokio::spawn(std::sync::Arc::new(Server::new(std::sync::Arc::new(hub))).serve(listener));
    Ok((format!("ws://{addr}/v1/tools"), handle))
}

async fn harness(url: &str) -> anyhow::Result<(Sock, SessionId)> {
    let mut sock = connect_async(url).await?.0;
    let mut hello = Hello::harness();
    hello.token = Some(TOKEN.to_owned());
    sock.send(Message::Text(serde_json::to_string(&hello)?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => {
            let _: HelloAck = serde_json::from_str(&t)?;
        }
        other => anyhow::bail!("bad handshake reply: {other:?}"),
    }
    let Outcome::Result(v) = request(&mut sock, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    Ok((sock, sid))
}

/// The bind a harness asks for, and the tool snapshot that came back.
async fn bind(
    sock: &mut Sock,
    sid: &SessionId,
    server_id: &str,
) -> anyhow::Result<Result<Vec<ToolDescription>, String>> {
    let params = json!({ "server_id": server_id });
    let outcome = request(sock, 2, Method::SessionBindServer, params, Some(sid)).await?;
    Ok(match outcome {
        Outcome::Result(v) => Ok(serde_json::from_value::<SessionBindServerResult>(v)?.tools),
        Outcome::Error(e) => Err(e.message),
    })
}

/// A tool server that reads the session from `params` must be able to serve.
///
/// This is the whole of the timeout, in one assertion. Before the fix the hub
/// sent `{}` and waited; the peer dropped it and the hub reported a server that
/// "did not answer", which is a diagnosis pointing at the peer rather than at
/// the disagreement that caused it.
#[tokio::test]
async fn a_tool_server_that_reads_the_session_from_params_is_served() -> anyhow::Result<()> {
    let (url, _hub) = hub().await.expect("hub");
    let peer = UpstreamToolServer::start(&url, "peer-1", "peer:echo").await;
    let (mut sock, sid) = harness(&url).await.expect("harness");

    // Wait for the server to register rather than sleeping a guess.
    wait_for_a_server(&mut sock).await?;

    let tools = bind(&mut sock, &sid, &peer.server_id)
        .await
        .expect("the bind completed");
    let tools = tools.unwrap_or_else(|e| panic!("the hub refused the bind: {e}"));

    assert!(
        tools.iter().any(|t| t.tool_id.as_str() == peer.tool_name()),
        "the tool the peer published did not come back through the hub: {tools:?}"
    );
    // The hub cached the snapshot: `tools.list` answers from the session, not
    // from a second round trip, and this is where a snapshot that never landed
    // shows up as an empty catalogue rather than an error.
    let Outcome::Result(v) =
        request(&mut sock, 4, Method::ToolsList, json!({}), Some(&sid)).await?
    else {
        anyhow::bail!("tools.list did not answer");
    };
    let listed: ToolsListResult = serde_json::from_value(v)?;
    assert!(
        listed
            .tools
            .iter()
            .any(|t| t.tool_id.as_str() == peer.tool_name()),
        "the tool is missing from tools.list after a successful bind: {:?}",
        listed.tools
    );
    Ok(())
}

/// The session must **not** be on the envelope, because upstream's router sends
/// any frame that carries one to a per-session inbox instead of to the
/// notification channel that answers binds.
///
/// This is the half of the shape that is easy to get wrong in the helpful
/// direction: putting the session in both places looks maximally compatible and
/// is exactly what made the first fix insufficient. The first version of this
/// file's fixture read only `params`, so it passed against a hub that still sent
/// the envelope session, and the real peer kept timing out. The fixture now
/// models the router as well as the reader.
#[tokio::test]
async fn the_session_must_not_also_ride_the_envelope() -> anyhow::Result<()> {
    let (url, _hub) = hub().await.expect("hub");
    let peer = UpstreamToolServer::start(&url, "peer-1", "peer:echo").await;
    let (mut sock, sid) = harness(&url).await.expect("harness");
    wait_for_a_server(&mut sock).await?;
    let tools = bind(&mut sock, &sid, &peer.server_id)
        .await
        .expect("the bind completed")
        .unwrap_or_else(|e| panic!("the hub refused the bind: {e}"));
    assert!(!tools.is_empty(), "the bind returned nothing to check");

    // And the shape itself, asserted directly rather than only through the
    // bind succeeding: a hub that stopped sending the session in params and
    // started relying on something else would pass the bind test on this
    // fixture's leniency and fail against a real server.
    let seen = peer.seen_binds.lock().expect("seen binds").clone();
    assert!(
        seen.iter().any(|b| b["params"]["session_id"].is_string()),
        "no `session.bind` carried the session in params: {seen:?}"
    );
    assert!(
        seen.iter().all(|b| b["envelope_session_id"].is_null()),
        "a `session.bind` carried the session on the envelope, which routes it away \
         from the code that answers binds: {seen:?}"
    );
    Ok(())
}

/// Block until the hub reports at least one registered tool server.
///
/// A tool server connects on its own schedule, so a test that binds the instant
/// the hub is up races it and fails with "no tool server registered as ..." —
/// which reads like a hub fault rather than like a test that did not wait. The
/// loop gives up rather than hanging, so a server that never arrives is still a
/// failure.
async fn wait_for_a_server(sock: &mut Sock) -> anyhow::Result<()> {
    for _ in 0..100 {
        if let Outcome::Result(v) = request(sock, 9, Method::ServersList, json!({}), None).await? {
            if !serde_json::from_value::<ServersListResult>(v)?
                .servers
                .is_empty()
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("no tool server registered with the hub after 5 seconds")
}

async fn request(
    sock: &mut Sock,
    id: i64,
    method: Method,
    params: serde_json::Value,
    session: Option<&SessionId>,
) -> anyhow::Result<Outcome> {
    let id = RpcId::Num(id);
    let mut req = Request::new(id.clone(), method, Some(params));
    if let Some(s) = session {
        req = req.in_session(s.clone());
    }
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out awaiting {method}"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed awaiting {method}");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == id {
                return Ok(r.outcome);
            }
        }
    }
}
