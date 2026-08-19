//! Harness-side hub connection.
//!
//! One socket, many concurrent calls. A background reader demultiplexes: a
//! response is delivered to whichever call is waiting on its id, and a progress
//! notification goes out on a side channel so the caller can render it live
//! rather than discovering it after the fact.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use openbot_proto::approval::{
    ApprovalDecision, ApprovalRequestParams, SecretRequestParams, SecretRequestResult,
};
use openbot_proto::frames::*;
use openbot_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcError, RpcId, ServerId,
    SessionId, ToolCallId, ToolId,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("handshake: {0}")]
    Handshake(String),
    /// The hub understood the handshake and said no. Its own words, because
    /// they are more useful than anything this side can infer.
    #[error("the hub refused this connection: {0}")]
    Refused(String),
    #[error("the hub connection closed")]
    Closed,
    #[error("[{code}] {message}")]
    Rpc { code: i32, message: String },
    #[error("malformed reply: {0}")]
    Malformed(String),
    #[error("no session is open")]
    NoSession,
}

impl From<RpcError> for HubError {
    fn from(e: RpcError) -> Self {
        Self::Rpc {
            code: e.code,
            message: e.message,
        }
    }
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Outcome>>>>;

/// Answers the hub when it asks whether a call may proceed.
///
/// The harness enforces nothing; the hub does (SPEC 6.0). This trait is only
/// the path by which a person's answer gets back. A harness that never
/// implements it simply never gets gated results.
#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn decide(&self, req: &ApprovalRequestParams) -> ApprovalDecision;

    /// True when this handler refuses everything, whatever is asked.
    ///
    /// The distinction matters to the agent loop. A person answering "no" is a
    /// decision about one action, and trying something else is a reasonable
    /// response. Nobody being there to answer is an absence that cannot change
    /// mid-run, so retrying is guaranteed futile, and a nightly routine would
    /// otherwise spend its whole step budget rediscovering that.
    fn denies_everything(&self) -> bool {
        false
    }

    /// Supply a credential the account does not hold, or refuse.
    ///
    /// Refusing is the default, and it is correct for every handler that does
    /// not override it. `AllowAll` approving a shell command is an operator
    /// accepting a risk out of band; there is no equivalent here, because
    /// there is no value a handler could invent. A harness that auto-supplied
    /// a credential would be answering a question only a person can answer.
    ///
    /// The hub treats `None` and a timeout the same way, so a handler that
    /// cannot ask anybody should return it immediately rather than stall.
    async fn supply(&self, _req: &SecretRequestParams) -> Option<String> {
        None
    }
}

/// Refuses everything. The default for an unattended process: if nobody is
/// there to look at the card, the answer is no.
pub struct DenyAll;

#[async_trait::async_trait]
impl ApprovalHandler for DenyAll {
    async fn decide(&self, _req: &ApprovalRequestParams) -> ApprovalDecision {
        ApprovalDecision::deny().with_note("no approver is attached to this session")
    }

    fn denies_everything(&self) -> bool {
        true
    }
}

/// Approves everything. Only for tests and for runs where an operator has
/// accepted the risk out of band.
pub struct AllowAll;

#[async_trait::async_trait]
impl ApprovalHandler for AllowAll {
    async fn decide(&self, _req: &ApprovalRequestParams) -> ApprovalDecision {
        ApprovalDecision::allow_once()
    }
}

pub struct HubClient {
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicI64,
    session: Mutex<Option<SessionId>>,
    pub ack: HelloAck,
    /// Whether this connection's approver refuses everything, so the agent can
    /// stop instead of retrying something that can never be allowed.
    blanket_denial: bool,
}

impl HubClient {
    /// Whether this connection's approver refuses everything.
    ///
    /// Used by the agent loop to tell "the person said no to this" from "there
    /// is no person"; the second cannot change during a run.
    pub fn approvals_are_impossible(&self) -> bool {
        self.blanket_denial
    }

    /// Connect with no approver attached: anything the policy gates is denied.
    pub async fn connect(
        url: &str,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<ToolCallProgressFrame>), HubError> {
        Self::connect_with(url, Arc::new(DenyAll)).await
    }

    /// Connect and complete the handshake. The returned receiver yields tool
    /// progress as it arrives; `approver` answers the hub's approval requests.
    pub async fn connect_with(
        url: &str,
        approver: Arc<dyn ApprovalHandler>,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<ToolCallProgressFrame>), HubError> {
        let blanket_denial = approver.denies_everything();
        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| HubError::Connect(e.to_string()))?;
        let (mut sink, mut source) = stream.split();

        let hello = serde_json::to_string(&Hello::harness()).expect("hello serialises");
        sink.send(Message::Text(hello))
            .await
            .map_err(|e| HubError::Handshake(e.to_string()))?;

        let ack: HelloAck = match source.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<HelloAck>(&t) {
                Ok(a) => a,
                // A hub that refuses the handshake answers with an `RpcError`
                // saying exactly why. Parsing that as an ack fails on a missing
                // field, and reporting that error would bury the one useful
                // sentence (a version mismatch would surface as "missing field
                // `connection_id`", which names neither versions nor the fix).
                Err(e) => match serde_json::from_str::<RpcError>(&t) {
                    Ok(r) => return Err(HubError::Refused(r.message)),
                    Err(_) => return Err(HubError::Handshake(format!("{e} (payload: {t})"))),
                },
            },
            Some(Ok(other)) => {
                return Err(HubError::Handshake(format!("expected text, got {other:?}")))
            }
            Some(Err(e)) => return Err(HubError::Handshake(e.to_string())),
            None => return Err(HubError::Closed),
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(Message::Text(m)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let pending: Pending = Arc::default();
        let (prog_tx, prog_rx) = mpsc::unbounded_channel();

        {
            let pending = Arc::clone(&pending);
            let out = tx.clone();
            tokio::spawn(async move {
                while let Some(Ok(Message::Text(t))) = source.next().await {
                    match Frame::decode(&t) {
                        Ok(Frame::Response(r)) => {
                            let key = id_key(&r.id);
                            if let Some(w) = pending.lock().await.remove(&key) {
                                let _ = w.send(r.outcome);
                            }
                        }
                        Ok(Frame::Notification(n))
                            if n.parsed_method() == Some(Method::ToolCallProgress) =>
                        {
                            if let Some(p) = n.params {
                                if let Ok(f) = serde_json::from_value::<ToolCallProgressFrame>(p) {
                                    let _ = prog_tx.send(f);
                                }
                            }
                        }
                        Ok(Frame::Request(req))
                            if req.parsed_method() == Some(Method::ApprovalRequest) =>
                        {
                            // Answer on a task: a person may take a while, and
                            // the read loop must keep serving progress frames
                            // and other traffic while they think.
                            let approver = Arc::clone(&approver);
                            let out = out.clone();
                            tokio::spawn(async move {
                                let id = req.id.clone();
                                let decision = match req
                                    .params
                                    .clone()
                                    .map(serde_json::from_value::<ApprovalRequestParams>)
                                {
                                    Some(Ok(p)) => approver.decide(&p).await,
                                    // A request that cannot be read cannot be
                                    // shown to anyone, so it cannot be approved.
                                    _ => ApprovalDecision::deny()
                                        .with_note("malformed approval request"),
                                };
                                let _ =
                                    out.send(Frame::Response(Response::ok(id, decision)).encode());
                            });
                        }
                        Ok(Frame::Request(req))
                            if req.parsed_method() == Some(Method::SecretRequest) =>
                        {
                            // On a task for the same reason an approval is:
                            // somebody is being asked to go and find a token,
                            // and the read loop cannot stop while they do.
                            let approver = Arc::clone(&approver);
                            let out = out.clone();
                            tokio::spawn(async move {
                                let id = req.id.clone();
                                let value = match req
                                    .params
                                    .clone()
                                    .map(serde_json::from_value::<SecretRequestParams>)
                                {
                                    Some(Ok(p)) => approver.supply(&p).await,
                                    // Unreadable is unshowable, and an
                                    // unshowable request cannot be answered.
                                    _ => None,
                                };
                                let _ = out.send(
                                    Frame::Response(Response::ok(
                                        id,
                                        SecretRequestResult { value },
                                    ))
                                    .encode(),
                                );
                            });
                        }
                        _ => {}
                    }
                }
                // Socket died: fail every waiter rather than hanging them.
                pending.lock().await.clear();
            });
        }

        Ok((
            Arc::new(Self {
                tx,
                pending,
                next_id: AtomicI64::new(1),
                session: Mutex::new(None),
                ack,
                blanket_denial,
            }),
            prog_rx,
        ))
    }

    async fn call(
        &self,
        method: Method,
        params: Value,
        session: Option<SessionId>,
    ) -> Result<Value, HubError> {
        let id = RpcId::Num(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (w_tx, w_rx) = oneshot::channel();
        self.pending.lock().await.insert(id_key(&id), w_tx);

        let mut req = Request::new(id, method, Some(params));
        if let Some(s) = session {
            req = req.in_session(s);
        }
        self.tx
            .send(Frame::Request(req).encode())
            .map_err(|_| HubError::Closed)?;

        match w_rx.await {
            Ok(Outcome::Result(v)) => Ok(v),
            Ok(Outcome::Error(e)) => Err(e.into()),
            Err(_) => Err(HubError::Closed),
        }
    }

    /// Open a session and remember it for subsequent calls.
    pub async fn open_session(&self) -> Result<SessionId, HubError> {
        self.open_session_as(None).await
    }

    /// Open a session that acts as a named Bot.
    ///
    /// The hub uses this to attribute a handoff: it decides who `bot.send`
    /// is from. Attribution, not authorisation.
    pub async fn open_session_as(&self, bot: Option<&str>) -> Result<SessionId, HubError> {
        let params = match bot {
            Some(b) => serde_json::json!({ "bot": b }),
            None => serde_json::json!({}),
        };
        let v = self.call(Method::SessionOpen, params, None).await?;
        let r: SessionOpenResult =
            serde_json::from_value(v).map_err(|e| HubError::Malformed(e.to_string()))?;
        *self.session.lock().await = Some(r.session_id.clone());
        Ok(r.session_id)
    }

    async fn current_session(&self) -> Result<SessionId, HubError> {
        self.session.lock().await.clone().ok_or(HubError::NoSession)
    }

    /// Bind a tool server and return the tools it serves.
    pub async fn bind_server(&self, server: &str) -> Result<Vec<ToolDescription>, HubError> {
        let s = self.current_session().await?;
        let v = self
            .call(
                Method::SessionBindServer,
                serde_json::to_value(SessionBindServerParams {
                    server_id: ServerId::new(server),
                })
                .expect("params serialise"),
                Some(s),
            )
            .await?;
        let r: SessionBindServerResult =
            serde_json::from_value(v).map_err(|e| HubError::Malformed(e.to_string()))?;
        Ok(r.tools)
    }

    /// List the tools currently bound to the session.
    pub async fn list_tools(&self) -> Result<Vec<ToolDescription>, HubError> {
        let s = self.current_session().await?;
        let v = self
            .call(Method::ToolsList, serde_json::json!({}), Some(s))
            .await?;
        let r: ToolsListResult =
            serde_json::from_value(v).map_err(|e| HubError::Malformed(e.to_string()))?;
        Ok(r.tools)
    }

    /// Invoke a tool. Progress for this call arrives on the connect() receiver.
    pub async fn call_tool(
        &self,
        tool: &str,
        call_id: &ToolCallId,
        args: Value,
    ) -> Result<Value, HubError> {
        let s = self.current_session().await?;
        let v = self
            .call(
                Method::ToolCall,
                serde_json::to_value(ToolCallRequestParams {
                    tool_id: ToolId::new(tool),
                    call_id: call_id.clone(),
                    args,
                })
                .expect("params serialise"),
                Some(s),
            )
            .await?;
        let r: ToolCallResult =
            serde_json::from_value(v).map_err(|e| HubError::Malformed(e.to_string()))?;
        Ok(r.output)
    }

    /// Claim a computer for a person at the keyboard.
    ///
    /// While held, the hub refuses every tool call routed to that computer from
    /// any other session. Enforced there, not here, so a client cannot simply
    /// decline to check.
    pub async fn take_over(&self, server: &str, reason: &str) -> Result<bool, HubError> {
        let s = self.current_session().await?;
        let v = self
            .call(
                Method::ComputerTakeover,
                serde_json::json!({ "server_id": server, "reason": reason }),
                Some(s),
            )
            .await?;
        Ok(v.get("claimed").and_then(|c| c.as_bool()).unwrap_or(false))
    }

    /// Give the computer back. The hub also does this if this connection drops.
    pub async fn give_back(&self, server: &str) -> Result<bool, HubError> {
        let s = self.current_session().await?;
        let v = self
            .call(
                Method::ComputerRelease,
                serde_json::json!({ "server_id": server }),
                Some(s),
            )
            .await?;
        Ok(v.get("released").and_then(|c| c.as_bool()).unwrap_or(false))
    }

    /// Tool servers available to this principal.
    pub async fn list_servers(&self) -> Result<Vec<ServerInfo>, HubError> {
        let v = self
            .call(Method::ServersList, serde_json::json!({}), None)
            .await?;
        let r: ServersListResult =
            serde_json::from_value(v).map_err(|e| HubError::Malformed(e.to_string()))?;
        Ok(r.servers)
    }
}

fn id_key(id: &RpcId) -> String {
    match id {
        RpcId::Num(n) => n.to_string(),
        RpcId::Str(s) => s.clone(),
    }
}
