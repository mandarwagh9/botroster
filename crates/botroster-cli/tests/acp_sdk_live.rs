//! The reference ACP SDK driving `botroster acp` as a real client would.
//!
//! `acp_live.rs` hand-rolls JSON-RPC bytes to test the agent, and the SDK's
//! own tests speak to fake lines. Neither proves that the actual client
//! library, the one the desktop client is built on, can negotiate with the
//! actual server. Every ACP feature the desktop client shows a person (a
//! session that lives, a turn that streams, a stop reason that means
//! something) crosses this join, so the join is tested here, on this binary,
//! through this SDK.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo};

use common::up::Up;

/// The one scripted reply `--demo` gives, ready for a person to read.
///
/// Everything in the demo turn flows through this: the streamed chunk asserts
/// the notification path, and the stop reason asserts the response path.
const DEMO_REPLY: &str = "Done.";

/// Does this update carry the words of the scripted reply?
fn carries(maybe: &SessionUpdate, text: &str) -> bool {
    match maybe {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => t.text.contains(text),
            _ => false,
        },
        _ => false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sdk_client_drives_the_shipped_binary_through_a_whole_turn() {
    // A real hub and guest, on an ephemeral port, exactly as `acp_live.rs` and
    // `cli_live.rs` use them. The demo turn calls no tools, but a turn cannot
    // even start without the stack: the agent binds a server id to the hub
    // before the first prompt.
    let up = Up::start().expect("botroster up");
    let home = tempfile::tempdir().expect("a home for the session's bots");

    let agent = AcpAgent::new(
        AcpAgentConfig::new(env!("CARGO_BIN_EXE_botroster"))
            .arg("acp")
            .arg("--demo")
            .arg("--home")
            .arg(home.path().to_str().expect("a utf-8 home path"))
            .env("BOTROSTER_HUB_URL", &up.hub)
            .env("NO_COLOR", "1"),
    );

    // What the client heard while the turn was running. Asserted on after
    // the turn returns: if the agent buffered instead of streaming, the
    // response would still arrive and only the inbox would be empty. A
    // desktop client that waits for the response before rendering anything
    // is not streaming.
    let inbox = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
    let seen = Arc::clone(&inbox);

    // The turn itself, as `connect_with` wants it: send the three requests,
    // hand back what the wire said. Everything asserted about the outcome
    // happens in this test body, so a failure is a failure and not a panic
    // swallowed by the SDK's internal tasks.
    let session = agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |note: SessionNotification, _cx| {
                seen.lock().expect("inbox").push(note.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            // The demo never asks, but a real client must be able to answer,
            // and the wiring is part of the join being proven.
            async move |req: RequestPermissionRequest, responder, _connection| {
                let outcome = match req.options.first() {
                    Some(option) => RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(option.option_id.clone()),
                    ),
                    None => RequestPermissionOutcome::Cancelled,
                };
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |conn: ConnectionTo<Agent>| async move {
            let init = conn
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let new = conn
                .send_request(NewSessionRequest::new("/tmp/payments-app"))
                .block_task()
                .await?;
            let prompt = conn
                .send_request(PromptRequest::new(
                    new.session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new("prove it"))],
                ))
                .block_task()
                .await?;
            Ok::<_, agent_client_protocol::Error>((
                init.protocol_version,
                init.agent_capabilities.load_session,
                new.session_id.to_string(),
                prompt.stop_reason,
            ))
        });

    // A turn that never answers is the deadlock this design avoids; make it a
    // failure instead of a hang. The agent gets 90 seconds at most.
    let (protocol, advertises_load, session_id, stop_reason) =
        tokio::time::timeout(Duration::from_secs(90), session)
            .await
            .expect("the turn did not finish in time")
            .expect("the SDK client could not complete the turn");

    assert_eq!(
        protocol,
        ProtocolVersion::V1,
        "botroster must answer in the version the client spoke"
    );
    assert!(
        advertises_load,
        "botroster stopped offering session loading, so a reconnecting client would show an empty transcript beside a Bot that remembers everything"
    );
    assert!(
        session_id.starts_with("botroster-"),
        "a session id should say whose it is, got {session_id}"
    );
    assert_eq!(
        stop_reason,
        StopReason::EndTurn,
        "the demo script completes, so anything else is a real difference"
    );

    let heard = inbox.lock().expect("inbox");
    assert!(
        heard.iter().any(|u| carries(u, DEMO_REPLY)),
        "the scripted reply never reached the client: {heard:?}"
    );
}
