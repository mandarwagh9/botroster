//! The guest's hub client: connect, handshake, then serve tools.
//!
//! The guest is a tool server. It never initiates work: it answers
//! `session.bind` with a tool snapshot and executes `tool_call_request`
//! frames the hub routes to it.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::frames::*;
use openbot_proto::{
    codes, Frame, Hello, HelloAck, Method, Notification, Request, Response, RpcError, RpcId,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::tools::{self, Context};

/// Version reported to the hub in `SessionBindResult`, so a hub can tell which
/// guest build answered without a separate probe.
const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct GuestConfig {
    pub hub_url: String,
    pub server_id: String,
    pub description: String,
    /// The hub's per-home token, when whoever started this guest knows it.
    ///
    /// Passed rather than looked up because a guest knows a URL and has never
    /// known a home: the hub that spawned it does know, and `up` starts one
    /// in-process against a home that may not be the default. Falling back to
    /// the environment covers a guest started on its own, which is the
    /// split-deployment case.
    ///
    /// See `openbot_proto::Hello::token` for what this defends and what it
    /// does not.
    pub token: Option<String>,
}

/// Longest wait between reconnection attempts.
const RECONNECT_MAX: std::time::Duration = std::time::Duration::from_secs(30);
/// First wait, doubled on each failure up to [`RECONNECT_MAX`].
const RECONNECT_MIN: std::time::Duration = std::time::Duration::from_millis(500);

/// The hub understood the handshake and refused it.
///
/// Kept apart from every other connection failure because it is the one that
/// waiting cannot fix. An outage ends; a guest whose protocol the hub does not
/// speak stays wrong until a different binary is installed, and retrying it
/// forever only hides that behind a log line.
#[derive(Debug, thiserror::Error)]
#[error("the hub refused this guest: {message}")]
pub struct Refused {
    pub code: i32,
    pub message: String,
}

impl From<RpcError> for Refused {
    fn from(e: RpcError) -> Self {
        Self {
            code: e.code,
            message: e.message,
        }
    }
}

impl Refused {
    /// Whether trying again could ever produce a different answer.
    ///
    /// `INVALID_REQUEST` means this guest asked for something the hub will not
    /// grant: a protocol it does not speak, a hello missing its server id.
    /// Anything else may be a hub that is starting up or temporarily
    /// misconfigured, and those are retried.
    fn is_permanent(&self) -> bool {
        self.code == codes::INVALID_REQUEST
    }
}

/// Serve the hub, reconnecting for as long as the process lives.
///
/// [`run`] holds one connection and returns when it drops. Without a
/// supervisor, restarting the control plane would detach every computer
/// attached to it: the hub comes back with no tool servers and the next task
/// fails with "no such server". A hub restart is a routine upgrade, not an
/// outage to repair by hand.
///
/// The three failure modes are treated differently:
///
/// * Never connected is a configuration error (a wrong URL, a hub that was
///   never started) and returns, so a bad launch is visible immediately rather
///   than retrying an address that will never answer.
/// * Stopped working is an outage, and is retried forever with backoff.
/// * Refused is neither: the hub answered, understood, and said no. See
///   [`Refused`]; retrying it turns a version mismatch into a guest that looks
///   like it is running while doing nothing.
///
/// Reconnecting re-pushes the tool catalogue, so the hub knows what this
/// computer serves without operator action. Sessions bound to the old
/// connection are gone (the hub cleared them when the socket dropped), so a
/// harness has to bind again, as it would after any restart.
pub async fn run_supervised(cfg: GuestConfig, ws: Arc<Context>) -> anyhow::Result<()> {
    // The first attempt's failure is returned to the caller.
    run(cfg.clone(), Arc::clone(&ws)).await?;

    let mut wait = RECONNECT_MIN;
    loop {
        tracing::warn!(?wait, hub = %cfg.hub_url, "lost the hub; reconnecting");
        tokio::time::sleep(wait).await;
        match run(cfg.clone(), Arc::clone(&ws)).await {
            Ok(()) => {
                // Connected and served for a while, then the socket closed.
                // Start the backoff over: this was an ordinary disconnection,
                // not a hub that keeps refusing.
                wait = RECONNECT_MIN;
            }
            Err(e) => {
                // A refusal the hub will repeat is not an outage to wait out.
                // Exiting with the reason is what makes it visible, instead of
                // a warning every thirty seconds while `openbot status` reports
                // no computer attached.
                if e.downcast_ref::<Refused>()
                    .is_some_and(Refused::is_permanent)
                {
                    tracing::error!(error = %e, "the hub will not accept this guest; giving up");
                    return Err(e);
                }
                tracing::warn!(error = %e, "reconnect failed");
                wait = (wait * 2).min(RECONNECT_MAX);
            }
        }
    }
}

pub async fn run(cfg: GuestConfig, ws: Arc<Context>) -> anyhow::Result<()> {
    let (stream, _) = tokio_tungstenite::connect_async(&cfg.hub_url).await?;
    let (mut sink, mut source) = stream.split();

    // Handshake: a bare `hello` frame, answered by a bare `hello_ack`.
    // Report the workspace root so a client can place a file where it can be
    // read rather than guessing at the layout. See `openbot_proto::META_WORKSPACE`.
    let hello = Hello::tool_server(cfg.server_id.as_str())
        .with_description(&cfg.description)
        .with_metadata(serde_json::json!({
            openbot_proto::META_WORKSPACE: ws.ws.root(),
        }))
        .with_token(cfg.token.clone());
    sink.send(Message::Text(serde_json::to_string(&hello)?))
        .await?;

    let ack: HelloAck = match source.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<HelloAck>(&t) {
            Ok(a) => a,
            // A hub that refuses the handshake answers with an `RpcError`
            // naming the reason. Parsing that as an ack fails on a missing
            // field, and reporting only the parse failure would throw the
            // reason away.
            Err(e) => match serde_json::from_str::<RpcError>(&t) {
                Ok(r) => return Err(Refused::from(r).into()),
                Err(_) => {
                    anyhow::bail!("hub sent a malformed hello_ack: {e} (payload: {t})")
                }
            },
        },
        Some(Ok(other)) => anyhow::bail!("expected a text hello_ack, got {other:?}"),
        Some(Err(e)) => anyhow::bail!("socket error during handshake: {e}"),
        None => anyhow::bail!("hub closed the socket before the handshake completed"),
    };
    tracing::info!(
        connection_id = %ack.connection_id,
        user = %ack.user_id,
        hub = %ack.hub_version,
        capabilities = ?ack.capabilities,
        "connected to hub"
    );

    // One writer task owns the sink so tool tasks can emit progress
    // concurrently without contending for it.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = source.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {
                continue
            }
            Err(e) => {
                tracing::warn!(error = %e, "socket error");
                break;
            }
        };

        let frame = match Frame::decode(&text) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, payload = %text, "undecodable frame");
                continue;
            }
        };

        match frame {
            Frame::Request(req) => handle_request(req, &ws, &tx),
            Frame::Notification(n) => handle_notification(n),
            // The guest issues no requests of its own yet, so any response is
            // unsolicited.
            Frame::Response(_) => {}
        }
    }

    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn handle_notification(n: Notification) {
    match n.parsed_method() {
        Some(Method::SessionUnbind) => {
            tracing::info!(session = ?n.session_id, "session unbound");
        }
        _ => tracing::debug!(method = %n.method, "ignoring notification"),
    }
}

fn handle_request(req: Request, ws: &Arc<Context>, tx: &mpsc::UnboundedSender<String>) {
    let id = req.id.clone();

    match req.parsed_method() {
        Some(Method::SessionBind) => {
            // Answer with the full snapshot. `serve` is idempotent, so a
            // rebind after reconnect simply replaces the set.
            let result = SessionBindResult {
                tools: tools::catalog(),
                binary_version: Some(BINARY_VERSION.to_owned()),
            };
            send(tx, Frame::Response(Response::ok(id, result)));
        }

        Some(Method::Ping) => {
            send(tx, Frame::Response(Response::ok(id, PongFrame::default())));
        }

        Some(Method::ToolCallRequest) => {
            let params: ToolCallRequestParams = match req
                .params
                .clone()
                .ok_or_else(|| "missing params".to_owned())
                .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            {
                Ok(p) => p,
                Err(e) => {
                    send(
                        tx,
                        Frame::Response(Response::err(id, codes::INVALID_PARAMS, e)),
                    );
                    return;
                }
            };

            // Each call runs on its own task: a slow tool must not block the
            // read loop, or the guest would stop answering pings.
            let ws = Arc::clone(ws);
            let tx = tx.clone();
            let session = req.session_id.clone();
            tokio::spawn(async move {
                run_tool(params, ws, tx, id, session).await;
            });
        }

        Some(other) => {
            send(
                tx,
                Frame::Response(Response::err(
                    id,
                    codes::METHOD_NOT_FOUND,
                    format!("a tool server does not serve `{other}`"),
                )),
            );
        }
        None => {
            send(
                tx,
                Frame::Response(Response::err(
                    id,
                    codes::METHOD_NOT_FOUND,
                    format!("unknown method `{}`", req.method),
                )),
            );
        }
    }
}

async fn run_tool(
    params: ToolCallRequestParams,
    ws: Arc<Context>,
    tx: mpsc::UnboundedSender<String>,
    id: RpcId,
    session: Option<openbot_proto::SessionId>,
) {
    let call_id = params.call_id.clone();

    // Progress is emitted the moment the tool produces it, not batched at the
    // end. Batching would still satisfy the `Progress* Terminal` ordering rule
    // while giving a caller nothing for the length of a slow command, which is
    // the case progress exists for. The terminal frame is only sent after
    // `invoke` returns, so ordering holds either way.
    let outcome = {
        let tx = tx.clone();
        let call_id = call_id.clone();
        let session = session.clone();
        let mut sink = move |payload: serde_json::Value| {
            let mut n = Notification::new(
                Method::ToolCallProgress,
                ToolCallProgressFrame {
                    call_id: call_id.clone(),
                    payload,
                },
            );
            n.session_id = session.clone();
            send(&tx, Frame::Notification(n));
        };
        tools::invoke(&ws, params.tool_id.as_str(), &params.args, &mut sink).await
    };

    let frame = match outcome {
        Ok(output) => Frame::Response(Response::ok(id, ToolCallResult { call_id, output })),
        Err(e) => Frame::Response(Response {
            jsonrpc: "2.0".into(),
            id,
            outcome: openbot_proto::Outcome::Error(RpcError {
                code: codes::TOOL_FAILED,
                message: e.to_string(),
                data: Some(serde_json::json!({ "call_id": call_id, "tool": params.tool_id })),
            }),
        }),
    };
    send(&tx, frame);
}

fn send(tx: &mpsc::UnboundedSender<String>, frame: Frame) {
    // A closed channel means the writer task is gone, i.e. the socket died.
    let _ = tx.send(frame.encode());
}
