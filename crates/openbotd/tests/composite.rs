//! Catalogue wiring, asserted over the socket.
//!
//! A tool can be reachable by direct invocation while being invisible in the
//! catalogue the model sees. Unit tests on a provider cannot catch that: they
//! call `catalog()` on the provider, which is not where the union is built.
//!
//! This suite asserts on the `tools/list` payload the harness receives over
//! the socket, with a guest, the bot tools and a connector all bound at once.
//! If the union breaks at either chain site in the hub, this fails.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::*;
use openbot_proto::{
    Frame, Hello, Method, Outcome, Request, RpcId, ServerId, SessionId, ToolCallId, ToolCallParams,
    ToolId,
};
use openbotd::connector::{Connector, ConnectorTools};
use openbotd::hub::Hub;
use openbotd::internal::Composite;
use openbotd::policy::Policy;
use openbotd::secrets::{Secret, SecretStore};
use openbotd::server::Server;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TOKEN: &str = "sk-live-broker-must-not-leak-this-0123456789";

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Harness {
    sock: Sock,
    next_id: i64,
}

impl Harness {
    async fn connect(url: &str) -> anyhow::Result<Self> {
        let (mut sock, _) = connect_async(url).await?;
        sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
            .await?;
        let _ack = sock.next().await;
        Ok(Self { sock, next_id: 1 })
    }

    async fn call(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<Outcome> {
        let id = RpcId::Num(self.next_id);
        self.next_id += 1;
        let mut req = Request::new(id.clone(), method, Some(params));
        if let Some(s) = session {
            req = req.in_session(s.clone());
        }
        self.sock
            .send(Message::Text(Frame::Request(req).encode()))
            .await?;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), self.sock.next())
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

    async fn ok(
        &mut self,
        method: Method,
        params: serde_json::Value,
        session: Option<&SessionId>,
    ) -> anyhow::Result<serde_json::Value> {
        match self.call(method, params, session).await? {
            Outcome::Result(v) => Ok(v),
            Outcome::Error(e) => anyhow::bail!("{method} failed: [{}] {}", e.code, e.message),
        }
    }
}

/// A hub with all three tool sources bound: a real guest over a real socket,
/// the hub's own `bot.*`, and a connector to a mock MCP server.
async fn boot() -> anyhow::Result<(String, tempfile::TempDir, Arc<Mutex<Vec<String>>>)> {
    let dir = tempfile::tempdir()?;

    let secrets = Arc::new(SecretStore::open(dir.path())?);
    secrets.set("linear-token", Secret::new(TOKEN))?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = support::mock_mcp(Arc::clone(&seen)).await;

    let connectors = ConnectorTools::discover(
        vec![Connector {
            id: "linear".into(),
            url,
            authorization: "Bearer ${linear-token}".into(),
        }],
        secrets,
    )
    .await;

    let bots = Arc::new(openbot_bots::BotStore::open(dir.path())?);
    let internal = Composite::new(vec![
        Arc::new(openbotd::bot_tools::BotTools::new(bots)),
        Arc::new(connectors),
    ]);

    // Routing and discovery are what this suite is about; the approval gate has
    // its own suite.
    let hub =
        Arc::new(Hub::with_policy(Policy::allow_all()).with_internal_tools(Arc::new(internal)));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));

    let ws = format!("ws://{addr}/v1/tools");
    let guest_dir = dir.path().join("workspace");
    std::fs::create_dir_all(&guest_dir)?;
    let ctx = Arc::new(openbot_guest::Context::new(
        openbot_guest::Workspace::new(&guest_dir, true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = openbot_guest::GuestConfig {
        hub_url: ws.clone(),
        server_id: "openbot-workspace".into(),
        description: "test guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = openbot_guest::run(cfg, ctx).await;
    });

    let mut probe = Harness::connect(&ws).await?;
    for _ in 0..100 {
        let v = probe.ok(Method::ServersList, json!({}), None).await?;
        let list: ServersListResult = serde_json::from_value(v)?;
        if !list.servers.is_empty() {
            return Ok((ws, dir, seen));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("guest never registered")
}

async fn session(h: &mut Harness) -> anyhow::Result<SessionId> {
    let v = h.ok(Method::SessionOpen, json!({}), None).await?;
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    h.ok(
        Method::SessionBindServer,
        serde_json::to_value(SessionBindServerParams {
            server_id: ServerId::new("openbot-workspace"),
        })?,
        Some(&sid),
    )
    .await?;
    Ok(sid)
}

/// Every source has to appear in the one payload a model reads before it can
/// plan.
#[tokio::test]
async fn every_tool_source_appears_in_the_catalogue_the_model_sees() -> anyhow::Result<()> {
    let (ws, _dir, _seen) = boot().await?;
    let mut h = Harness::connect(&ws).await?;
    let sid = session(&mut h).await?;

    let v = h.ok(Method::ToolsList, json!({}), Some(&sid)).await?;
    let listed: ToolsListResult = serde_json::from_value(v)?;
    let ids: Vec<&str> = listed.tools.iter().map(|t| t.tool_id.as_str()).collect();

    for expected in [
        "fs.read",              // the guest, over a real socket
        "bot.send",             // the hub's own
        "linear__create_issue", // a connector, discovered from a remote
    ] {
        assert!(
            ids.contains(&expected),
            "`{expected}` is invisible to the model; catalogue is {ids:?}"
        );
    }
    Ok(())
}

/// The same union has to hold at the bind reply, which is where a harness
/// takes its first snapshot; it is a second, separately written chain in the
/// hub.
#[tokio::test]
async fn the_bind_reply_carries_the_same_union() -> anyhow::Result<()> {
    let (ws, _dir, _seen) = boot().await?;
    let mut h = Harness::connect(&ws).await?;

    let v = h.ok(Method::SessionOpen, json!({}), None).await?;
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    let v = h
        .ok(
            Method::SessionBindServer,
            serde_json::to_value(SessionBindServerParams {
                server_id: ServerId::new("openbot-workspace"),
            })?,
            Some(&sid),
        )
        .await?;
    let bound: SessionBindServerResult = serde_json::from_value(v)?;
    let ids: Vec<&str> = bound.tools.iter().map(|t| t.tool_id.as_str()).collect();

    for expected in ["fs.read", "bot.send", "linear__create_issue"] {
        assert!(
            ids.contains(&expected),
            "bind reply missing {expected}: {ids:?}"
        );
    }
    Ok(())
}

/// End to end through the socket: the guest asks the hub for a connector tool,
/// the credential is attached at the outbound edge, and only the result comes
/// back.
#[tokio::test]
async fn a_connector_call_crosses_the_hub_and_the_token_stays_behind() -> anyhow::Result<()> {
    let (ws, _dir, seen) = boot().await?;
    let mut h = Harness::connect(&ws).await?;
    let sid = session(&mut h).await?;

    let v = h
        .ok(
            Method::ToolCall,
            serde_json::to_value(ToolCallParams {
                call_id: ToolCallId::new("c1"),
                tool_id: ToolId::new("linear__create_issue"),
                args: json!({ "title": "it broke" }),
            })?,
            Some(&sid),
        )
        .await?;

    let text = v.to_string();
    assert!(
        text.contains("ROO-1"),
        "the result did not come back: {text}"
    );
    assert!(
        !text.contains(TOKEN) && !text.contains("sk-live"),
        "a credential crossed the hub boundary: {text}"
    );

    let headers = seen.lock().unwrap().clone();
    assert!(
        headers.iter().any(|h| h.contains(TOKEN)),
        "the remote never received the credential: {headers:?}"
    );
    Ok(())
}

/// A tool no source serves must fail the call, not the connection, and must
/// not be silently attributed to whichever source happens to be first.
#[tokio::test]
async fn an_unknown_tool_is_still_a_clean_error() -> anyhow::Result<()> {
    let (ws, _dir, _seen) = boot().await?;
    let mut h = Harness::connect(&ws).await?;
    let sid = session(&mut h).await?;

    let out = h
        .call(
            Method::ToolCall,
            serde_json::to_value(ToolCallParams {
                call_id: ToolCallId::new("c2"),
                tool_id: ToolId::new("nope.not.a.tool"),
                args: json!({}),
            })?,
            Some(&sid),
        )
        .await?;
    assert!(
        matches!(out, Outcome::Error(_)),
        "expected an error, got {out:?}"
    );
    Ok(())
}
