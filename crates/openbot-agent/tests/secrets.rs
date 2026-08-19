//! A credential a Bot asked for reaches the store and nowhere else.
//!
//! The promise is that a supplied credential is masked, absent from the
//! transcript, and never shown to the model. Two other tests cover parts of
//! it: `openbotd`'s `a_supplied_credential_is_stored_and_never_returned` checks
//! the tool result, and `openbot-cli`'s checks that no unattended approval mode
//! supplies a value. Neither exercises the middle: `HubClient`'s
//! `Method::SecretRequest` arm, the code that routes the ask to a person and
//! puts the answer back on the wire. And "absent from the tool result" does
//! not imply "absent from everything a running turn writes down".
//!
//! This test drives the real path (real hub, real `HubClient`, real agent
//! loop) with a sentinel value, then searches for the sentinel in three
//! places: `outcome.transcript`, which is verbatim what `openbot-cli` appends to
//! `conversation.jsonl`; every `AgentEvent` a client renders; and every byte
//! the hub wrote under the home.
//!
//! The conversation log is written a crate later, by `openbot-cli`, so it does
//! not exist under this home and the file sweep does not cover it; the sweep
//! covers what the hub wrote. The transcript is therefore checked from the
//! value `openbot-cli` persists rather than from a file this test never causes
//! to exist.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use openbot_agent::providers::Scripted;
use openbot_agent::{Agent, AgentConfig, ApprovalHandler, HubClient};
use openbot_proto::approval::{ApprovalDecision, ApprovalRequestParams, SecretRequestParams};
use openbotd::policy::Policy;
use openbotd::secrets::SecretStore;
use serde_json::json;
use tokio::sync::mpsc;

/// Never in a log, never in an event, never in the model's context.
const VALUE: &str = "sk-live-SENTINEL-e7b1a4-NEVER-ANYWHERE-BUT-THE-STORE";

/// A person who types the credential when asked, and records being asked.
struct Person {
    asked: Mutex<Vec<SecretRequestParams>>,
}

#[async_trait::async_trait]
impl ApprovalHandler for Person {
    async fn decide(&self, _req: &ApprovalRequestParams) -> ApprovalDecision {
        ApprovalDecision::allow_once()
    }
    async fn supply(&self, req: &SecretRequestParams) -> Option<String> {
        self.asked.lock().unwrap().push(req.clone());
        Some(VALUE.to_owned())
    }
}

/// Every file under `dir` that contains `needle`, relative to `dir`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(bytes) = std::fs::read(&p) {
                // As bytes: a transcript writer that chose a different
                // encoding still leaks, and `read_to_string` would skip the
                // file rather than report it.
                if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    found.push(p.strip_prefix(dir).unwrap_or(&p).to_path_buf());
                }
            }
        }
    }
    found.sort();
    found
}

#[tokio::test]
async fn a_credential_reaches_the_store_and_no_other_file_or_event() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let secrets = Arc::new(SecretStore::open(home.path())?);
    let hub = Arc::new(
        openbotd::hub::Hub::with_policy(Policy::allow_all()).with_secrets(Arc::clone(&secrets)),
    );
    let (listener, addr) = openbotd::server::Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::clone(&Arc::new(openbotd::server::Server::new(hub))).serve(listener));

    let person = Arc::new(Person {
        asked: Mutex::new(Vec::new()),
    });
    let (client, progress) = HubClient::connect_with(
        &format!("ws://{addr}/v1/tools"),
        Arc::clone(&person) as Arc<dyn ApprovalHandler>,
    )
    .await?;
    client.open_session().await?;

    let model = Arc::new(
        Scripted::builder()
            .call(
                "secret.request",
                json!({ "name": "linear-token", "why": "to file the issue you asked for" }),
            )
            .say("stored it")
            .build(),
    );
    let agent = Agent::new(
        model,
        Arc::clone(&client),
        AgentConfig {
            system: "test".into(),
            max_steps: 8,
            ..Default::default()
        },
    );
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let outcome = agent
        .run("get the token", Vec::new(), progress, ev_tx)
        .await;

    // The person was asked, and told what for: a prompt that cannot say why
    // is a prompt nobody can answer responsibly.
    let asked = person.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "the person was asked {} times", asked.len());
    assert_eq!(asked[0].name, "linear-token");
    assert!(
        asked[0].why.contains("file the issue"),
        "the reason did not reach the person: {:?}",
        asked[0].why
    );

    // It was stored.
    assert_eq!(secrets.get("linear-token")?.expose(), VALUE);

    // The transcript. `openbot run` persists exactly this, with
    //
    //     let fresh = &outcome.transcript[started_from..];
    //     bots.append(&b.id, fresh)?;
    //
    // in `openbot-cli/src/main.rs`, and `BotStore::append` writes those messages
    // as JSON lines to `bots/<id>/conversation.jsonl`. Serialising them the
    // same way and searching inspects the file's contents, a crate earlier.
    let persisted = outcome
        .transcript
        .iter()
        .map(|m| serde_json::to_string(m).expect("a message serialises"))
        .collect::<Vec<_>>();
    // The tool call has to be in the record: a transcript that dropped the
    // exchange entirely would pass a search for the value while hiding that
    // the Bot ever asked.
    assert!(
        persisted.iter().any(|m| m.contains("secret.request")),
        "the request is not in the transcript at all, so its absence proves \
         nothing: {persisted:?}"
    );
    for m in &persisted {
        assert!(
            !m.contains(VALUE),
            "a credential reached the transcript: {m}"
        );
    }
    assert!(
        !format!("{outcome:?}").contains(VALUE),
        "a credential reached the turn's outcome: {outcome:?}"
    );

    // Nothing a client renders carries it either. Debug rather than a field
    // walk: whatever a future variant adds is included without this test being
    // updated to know about it.
    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(format!("{e:?}"));
    }
    assert!(
        !events.is_empty(),
        "no events at all; this test would pass vacuously"
    );
    for e in &events {
        assert!(
            !e.contains(VALUE),
            "a credential reached the event stream: {e}"
        );
    }

    // On disk: of everything the hub wrote under the home, only the store
    // holds it. The conversation is not written here (that happens a crate
    // later, in `openbot-cli`), which is why the transcript is checked above
    // from the value `openbot-cli` appends rather than by looking for a file
    // this test never causes to exist.
    let leaks = files_containing(home.path(), VALUE);
    assert_eq!(
        leaks,
        vec![PathBuf::from("secrets.json")],
        "the credential is on disk somewhere it was promised not to be"
    );
    Ok(())
}
