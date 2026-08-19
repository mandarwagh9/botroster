//! The credential broker's central property: a token reaches the remote
//! server and nothing else.
//!
//! A mock MCP server records the Authorization header it was sent; the test
//! then asserts the same token appears nowhere in the tool catalogue, the
//! result, or an error.

mod support;

use std::sync::{Arc, Mutex};

use openbotd::connector::{Connector, ConnectorTools};
use openbotd::hub::InternalTools;
use openbotd::secrets::{Secret, SecretStore};
use serde_json::json;
use support::mock_mcp;

const TOKEN: &str = "sk-live-broker-must-not-leak-this-0123456789";

async fn broker() -> (tempfile::TempDir, ConnectorTools, Arc<Mutex<Vec<String>>>) {
    let d = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretStore::open(d.path()).unwrap());
    secrets.set("linear-token", Secret::new(TOKEN)).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = mock_mcp(Arc::clone(&seen)).await;

    let tools = ConnectorTools::discover(
        vec![Connector {
            id: "linear".into(),
            url,
            authorization: "Bearer ${linear-token}".into(),
        }],
        secrets,
    )
    .await;
    (d, tools, seen)
}

#[tokio::test]
async fn the_token_reaches_the_remote_and_nothing_else() {
    let (_d, tools, seen) = broker().await;

    let catalog = tools.catalog();
    assert_eq!(catalog.len(), 1, "discovery failed");
    assert_eq!(catalog[0].tool_id.as_str(), "linear__create_issue");

    let out = tools
        .invoke(
            Some("piper"),
            "linear__create_issue",
            &json!({ "title": "it broke" }),
        )
        .await
        .expect("call");

    // The remote did receive the credential: the broker is not merely
    // withholding it from everyone.
    let headers = seen.lock().unwrap().clone();
    assert!(
        headers.iter().any(|h| h.contains(TOKEN)),
        "the remote never got the token: {headers:?}"
    );

    // And it appears in nothing the guest or the model can see.
    let catalog_text = format!("{catalog:?}");
    let result_text = format!("{out:?}") + &out.to_string();
    assert!(
        !catalog_text.contains(TOKEN),
        "token leaked into the tool catalogue"
    );
    assert!(
        !result_text.contains(TOKEN),
        "token leaked into a tool result"
    );
    assert!(
        result_text.contains("ROO-1"),
        "the result did not come back: {result_text}"
    );
}

#[tokio::test]
async fn an_error_from_a_connector_carries_no_credential() {
    let d = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretStore::open(d.path()).unwrap());
    secrets.set("linear-token", Secret::new(TOKEN)).unwrap();

    // A host that will not resolve: the failure path is where a naive
    // implementation prints the whole request, credential included.
    let tools = ConnectorTools::discover(
        vec![Connector {
            id: "linear".into(),
            url: "http://127.0.0.1:1/mcp".into(),
            authorization: "Bearer ${linear-token}".into(),
        }],
        Arc::clone(&secrets),
    )
    .await;

    // Discovery failed, so it offers nothing rather than advertising tools it
    // cannot call.
    assert!(tools.catalog().is_empty());

    let e = tools
        .invoke(None, "linear__create_issue", &json!({}))
        .await
        .unwrap_err();
    assert!(
        !e.contains(TOKEN),
        "a token leaked into an error message: {e}"
    );
    assert!(!e.contains("sk-live"), "{e}");
}

/// Discovery runs before the hub can accept anyone, so a connector that
/// accepts the connection and then says nothing must not hold the door shut.
/// Three of them, concurrently, still inside one deadline.
#[tokio::test]
async fn a_hung_connector_cannot_stall_startup() {
    let d = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretStore::open(d.path()).unwrap());
    secrets.set("t", Secret::new(TOKEN)).unwrap();

    let hung = support::black_hole().await;
    let connectors = (0..3)
        .map(|i| Connector {
            id: format!("dead{i}"),
            url: hung.clone(),
            authorization: "Bearer ${t}".into(),
        })
        .collect();

    let started = std::time::Instant::now();
    let tools = ConnectorTools::discover(connectors, secrets).await;
    let waited = started.elapsed();

    assert!(tools.catalog().is_empty());
    // The deadline is 5s and the probes are concurrent; serial probing would
    // take 15s and a missing deadline would take forever.
    assert!(
        waited < std::time::Duration::from_secs(9),
        "startup waited {waited:?} on unresponsive connectors"
    );
}

#[tokio::test]
async fn a_tool_from_an_unknown_connector_is_refused() {
    let (_d, tools, _seen) = broker().await;
    let e = tools
        .invoke(None, "notaconnector__do_thing", &json!({}))
        .await
        .unwrap_err();
    assert!(e.contains("no connector"), "{e}");
}

#[tokio::test]
async fn a_missing_secret_fails_the_call_rather_than_sending_an_empty_header() {
    let d = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretStore::open(d.path()).unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = mock_mcp(Arc::clone(&seen)).await;

    // The secret is never set. Substituting nothing would send `Bearer ` and
    // produce a confusing 401 instead of a clear configuration error.
    let tools = ConnectorTools::discover(
        vec![Connector {
            id: "linear".into(),
            url,
            authorization: "Bearer ${never-configured}".into(),
        }],
        secrets,
    )
    .await;

    assert!(
        tools.catalog().is_empty(),
        "a connector with no credential was offered"
    );
    let e = tools
        .invoke(None, "linear__create_issue", &json!({}))
        .await
        .unwrap_err();
    assert!(e.contains("never-configured"), "unhelpful error: {e}");
}
