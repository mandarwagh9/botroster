//! What the hub writes down about a session, against a real hub and a real store.
//!
//! The record exists so that a Bot's history can be replayed and diffed, which
//! makes fidelity the whole of its value: a step that is missing reads as work
//! the Bot never did, and a step that is wrong is worse than no record at all.
//! So these drive the hub over a socket and read what actually landed on disk,
//! rather than asserting on the struct that was going to be written.
//!
//! The credential test is the one to keep. Everything else here can be wrong in
//! a way that costs somebody an afternoon; that one can be wrong in a way that
//! writes an API key into a file and leaves it there.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use botroster_bots::{BotId, BotStore};
use botroster_proto::approval::SecretRequestResult;
use botroster_proto::frames::*;
use botroster_proto::{
    Frame, Hello, HelloAck, Method, Outcome, Request, Response, RpcId, SessionId,
};
use botrosterd::hub::Hub;
use botrosterd::policy::{Action, Policy, Rule};
use botrosterd::record::ToBotStore;
use botrosterd::record::{Decided, Ended, Step};
use botrosterd::secrets::SecretStore;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A credential shaped like a real one, so a substring search for it is a
/// meaningful search.
const VALUE: &str = "sk-live-NEVER-IN-A-RECORD-4b71e";

/// A hub recording into a store, and a socket already through the handshake.
struct Fixture {
    sock: Sock,
    store: Arc<BotStore>,
    _dir: tempfile::TempDir,
    secrets: Arc<SecretStore>,
    next_id: i64,
    url: String,
}

impl Fixture {
    async fn start(policy: Policy) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Arc::new(BotStore::open(dir.path())?);
        let secrets = Arc::new(SecretStore::open(dir.path())?);
        let hub = Arc::new(
            Hub::with_policy(policy)
                .recording_to(Arc::new(ToBotStore::spawn(Arc::clone(&store))))
                .with_internal_tools(Arc::new(botrosterd::bot_tools::BotTools::new(Arc::clone(
                    &store,
                ))))
                .with_secrets(Arc::clone(&secrets))
                .with_approval_timeout(Duration::from_secs(3)),
        );
        let (listener, addr) = Server::bind("127.0.0.1:0").await?;
        tokio::spawn(Arc::new(Server::new(hub)).serve(listener));

        let url = format!("ws://{addr}/v1/tools");
        let (mut sock, _) = connect_async(&url).await?;
        sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
            .await?;
        match sock.next().await {
            Some(Ok(Message::Text(t))) => {
                let _: HelloAck = serde_json::from_str(&t)?;
            }
            other => anyhow::bail!("bad handshake: {other:?}"),
        }
        Ok(Self {
            sock,
            store,
            _dir: dir,
            secrets,
            next_id: 1,
            url,
        })
    }

    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// Open a session, optionally acting as a Bot.
    async fn open(&mut self, bot: Option<&str>) -> anyhow::Result<SessionId> {
        self.open_with(bot, None).await
    }

    /// Open a session that answers from a past session's record.
    async fn replay(&mut self, bot: &str, of: &SessionId) -> anyhow::Result<SessionId> {
        self.open_with(Some(bot), Some((bot.to_owned(), of.clone())))
            .await
    }

    async fn open_with(
        &mut self,
        bot: Option<&str>,
        replay: Option<(String, SessionId)>,
    ) -> anyhow::Result<SessionId> {
        let id = self.id();
        let mut params = match bot {
            Some(b) => json!({ "bot": b }),
            None => json!({}),
        };
        if let Some((rb, rs)) = replay {
            params["replay"] = json!({ "bot": rb, "session": rs.as_str() });
        }
        self.sock
            .send(Message::Text(
                Frame::Request(Request::new(
                    RpcId::Num(id),
                    Method::SessionOpen,
                    Some(params),
                ))
                .encode(),
            ))
            .await?;
        loop {
            let Some(Ok(Message::Text(t))) = self.sock.next().await else {
                anyhow::bail!("socket closed opening a session");
            };
            if let Frame::Response(r) = Frame::decode(&t)? {
                if r.id == RpcId::Num(id) {
                    let v = match r.outcome {
                        Outcome::Result(v) => v,
                        Outcome::Error(e) => anyhow::bail!("{}", e.message),
                    };
                    return Ok(serde_json::from_value::<SessionOpenResult>(v)?.session_id);
                }
            }
        }
    }

    /// Bind a tool server to a session, so its calls are forwarded to it.
    async fn bind(&mut self, sid: &SessionId, server: &str) -> anyhow::Result<()> {
        let id = self.id();
        self.sock
            .send(Message::Text(
                Frame::Request(
                    Request::new(
                        RpcId::Num(id),
                        Method::SessionBindServer,
                        Some(json!({ "server_id": server })),
                    )
                    .in_session(sid.clone()),
                )
                .encode(),
            ))
            .await?;
        loop {
            let Some(Ok(Message::Text(t))) = self.sock.next().await else {
                anyhow::bail!("socket closed binding a server");
            };
            if let Frame::Response(r) = Frame::decode(&t)? {
                if r.id == RpcId::Num(id) {
                    return match r.outcome {
                        Outcome::Result(_) => Ok(()),
                        Outcome::Error(e) => anyhow::bail!("{}", e.message),
                    };
                }
            }
        }
    }

    /// Make one tool call and return how it ended.
    ///
    /// `on_secret` answers a `secret.request` the hub sends back, which is the
    /// only mid-call question these tests provoke.
    async fn call(
        &mut self,
        sid: &SessionId,
        tool: &str,
        args: serde_json::Value,
        on_secret: Option<&str>,
    ) -> anyhow::Result<Outcome> {
        let id = self.id();
        let call = json!({
            "call_id": format!("call-{id}"),
            "tool_id": tool,
            "args": args,
        });
        self.sock
            .send(Message::Text(
                Frame::Request(
                    Request::new(RpcId::Num(id), Method::ToolCall, Some(call))
                        .in_session(sid.clone()),
                )
                .encode(),
            ))
            .await?;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(20), self.sock.next())
                .await
                .map_err(|_| anyhow::anyhow!("the hub never answered `{tool}`"))?;
            let Some(Ok(Message::Text(t))) = msg else {
                anyhow::bail!("socket closed waiting for `{tool}`");
            };
            match Frame::decode(&t)? {
                Frame::Request(r) if r.parsed_method() == Some(Method::SecretRequest) => {
                    let reply = Response::ok(
                        r.id.clone(),
                        SecretRequestResult {
                            value: on_secret.map(str::to_owned),
                        },
                    );
                    self.sock
                        .send(Message::Text(Frame::Response(reply).encode()))
                        .await?;
                }
                Frame::Response(r) if r.id == RpcId::Num(id) => return Ok(r.outcome),
                _ => {}
            }
        }
    }

    /// Start a real guest on `work` and wait for it to register.
    ///
    /// Extracted because two replay tests need one: the property being checked
    /// is that a replayed call never reaches the guest, and that is only
    /// meaningful if there is a guest it could have reached.
    async fn attach_guest(&self, work: &std::path::Path) -> anyhow::Result<()> {
        let ctx = Arc::new(botroster_guest::Context::new(
            botroster_guest::Workspace::new(work, true)?,
            work.join(".browser-profile"),
        ));
        let cfg = botroster_guest::GuestConfig {
            hub_url: self.url.clone(),
            server_id: "botroster-workspace".into(),
            description: "the guest for the record tests".into(),
            token: None,
        };
        tokio::spawn(async move {
            let _ = botroster_guest::run(cfg, ctx).await;
        });
        Ok(())
    }

    /// Bind, polling until the guest has registered.
    ///
    /// A fixed sleep is either flaky on a loaded machine or slow on an idle
    /// one.
    async fn bind_with_patience(&mut self, sid: &SessionId, server: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.bind(sid, server).await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("the guest never registered with the hub");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The steps on disk, once there are `want` of them.
    ///
    /// Polled rather than read once: the writer is a task behind a channel, on
    /// purpose — the hub must not block on a disk to answer a tool call — so
    /// the record lands just after the response does. Waiting for a count
    /// rather than sleeping keeps the test honest on a slow machine and quick
    /// on a fast one.
    async fn steps(&self, bot: &str, sid: &SessionId, want: usize) -> Vec<Step> {
        let id = BotId(bot.to_owned());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lines = self
                .store
                .session_record(&id, sid.as_str())
                .expect("the record is readable");
            if lines.len() >= want || Instant::now() >= deadline {
                return lines
                    .iter()
                    .map(|l| {
                        serde_json::from_str(l).unwrap_or_else(|e| {
                            panic!("a recorded line does not parse as a step: {e}\n{l}")
                        })
                    })
                    .collect();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Every byte the record holds for this session.
    fn raw(&self, bot: &str, sid: &SessionId) -> String {
        self.store
            .session_record(&BotId(bot.to_owned()), sid.as_str())
            .expect("the record is readable")
            .join("\n")
    }
}

/// A call the policy allowed is recorded, with what it returned.
#[tokio::test]
async fn a_permitted_call_is_recorded_with_its_result() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let sid = f.open(Some("scout")).await?;

    let outcome = f.call(&sid, "bot.list", json!({}), None).await?;
    assert!(
        matches!(outcome, Outcome::Result(_)),
        "the call itself failed: {outcome:?}"
    );

    let steps = f.steps("scout", &sid, 1).await;
    assert_eq!(steps.len(), 1, "expected one step, got {steps:?}");
    let step = &steps[0];
    assert_eq!(step.seq, 1);
    assert_eq!(step.tool, "bot.list");
    assert_eq!(step.decided, Decided::Policy);
    assert!(
        matches!(step.ended, Ended::Ok(_)),
        "a call that succeeded was not recorded as such: {:?}",
        step.ended
    );
    assert!(
        step.args.is_complete(),
        "arguments this small should be kept whole"
    );
    Ok(())
}

/// A call the policy refused is recorded as refused, and says who refused it.
///
/// The half that a log of successes would miss, and the half somebody debugging
/// a Bot that "did nothing" actually needs. A record that simply omits refused
/// calls reads as a Bot that never tried.
#[tokio::test]
async fn a_refusal_is_recorded_and_names_who_refused() -> anyhow::Result<()> {
    let policy = Policy {
        rules: vec![Rule::deny("bot.send", "not from a test")],
        fallback: Action::Allow,
        grants: std::collections::BTreeSet::new(),
        allow_private_browser_destinations: false,
    };
    let mut f = Fixture::start(policy).await?;
    let sid = f.open(Some("scout")).await?;

    let outcome = f
        .call(
            &sid,
            "bot.send",
            json!({"to": "nobody", "text": "hi"}),
            None,
        )
        .await?;
    assert!(
        matches!(outcome, Outcome::Error(_)),
        "a denied call succeeded: {outcome:?}"
    );

    let steps = f.steps("scout", &sid, 1).await;
    assert_eq!(steps.len(), 1, "the refusal was not recorded at all");
    let step = &steps[0];
    assert_eq!(step.tool, "bot.send");
    assert_eq!(step.ended, Ended::Refused);
    let Decided::RefusedByPolicy(why) = &step.decided else {
        panic!("recorded as {:?}, not as a policy refusal", step.decided);
    };
    assert!(
        why.contains("not from a test"),
        "the record kept the refusal but lost the reason: {why}"
    );
    assert!(!step.decided.permitted());
    Ok(())
}

/// **A credential's value reaches no byte of the record.**
///
/// Not "the result type has no field for it" — that is a fact about a struct,
/// and the record is a file. `secret.request` is the one tool whose whole
/// purpose is to move a secret through the hub, so it is the one place a
/// recorder could write an API key to disk and nobody would look. The
/// assertion is on the bytes.
#[tokio::test]
async fn a_credential_never_reaches_the_record() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let sid = f.open(Some("scout")).await?;

    let outcome = f
        .call(
            &sid,
            "secret.request",
            json!({ "name": "linear-token", "why": "to file the issue you asked for" }),
            Some(VALUE),
        )
        .await?;
    assert!(
        matches!(outcome, Outcome::Result(_)),
        "the credential was not stored: {outcome:?}"
    );

    // It really did go through — otherwise this test proves only that a call
    // which never happened wrote nothing.
    assert_eq!(
        f.secrets.get("linear-token").expect("stored").expose(),
        VALUE,
        "the credential never reached the store, so this test is vacuous"
    );

    let steps = f.steps("scout", &sid, 1).await;
    assert_eq!(steps.len(), 1, "the credential request was not recorded");

    let raw = f.raw("scout", &sid);
    assert!(
        !raw.contains(VALUE),
        "the credential is in the record on disk:\n{raw}"
    );
    // A partial leak is a leak: a recorder that kept the first 4kB of a long
    // value would pass a whole-string search and still have written the key.
    assert!(
        !raw.contains("NEVER-IN-A-RECORD"),
        "part of the credential is in the record on disk:\n{raw}"
    );
    // And the step is genuinely there and useful, rather than empty.
    assert!(
        raw.contains("linear-token"),
        "the record does not say which credential was asked for:\n{raw}"
    );
    Ok(())
}

/// The numbers in the record match the order of the lines.
///
/// `seq` is assigned by the writer rather than the caller for exactly this: two
/// calls finishing in either order still produce a file whose sequence and
/// whose lines agree. A record whose order cannot be trusted is worse than one
/// that lags, because a diff over it reports divergences that never happened.
#[tokio::test]
async fn the_sequence_matches_the_order_of_the_file() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let sid = f.open(Some("scout")).await?;

    for _ in 0..5 {
        f.call(&sid, "bot.list", json!({}), None).await?;
    }

    let steps = f.steps("scout", &sid, 5).await;
    assert_eq!(steps.len(), 5, "expected five steps, got {}", steps.len());
    let seqs: Vec<u64> = steps.iter().map(|s| s.seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3, 4, 5],
        "the sequence does not match the order of the lines"
    );
    Ok(())
}

/// A session naming no Bot records nothing, which is a decision.
///
/// `session_open`'s `bot` is optional and a bare `botroster call` in a script
/// names none. There is nobody to attribute that work to, and inventing a place
/// for it would put a script's one-off calls in a teammate's history. Asserted
/// rather than commented, because the alternative — quietly writing them
/// somewhere — is the kind of thing that gets added later by someone who reads
/// the absence as an oversight.
#[tokio::test]
async fn a_session_with_no_bot_is_not_recorded() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let sid = f.open(None).await?;

    let outcome = f.call(&sid, "bot.list", json!({}), None).await?;
    assert!(
        matches!(outcome, Outcome::Result(_)),
        "the call failed, so this proves nothing about recording: {outcome:?}"
    );

    // Give the writer the same chance to write as every other test gives it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let anywhere = f
        .store
        .sessions(&BotId("scout".into()))
        .expect("listing is readable");
    assert!(
        anywhere.is_empty(),
        "a session that named no Bot was written into one anyway: {anywhere:?}"
    );
    Ok(())
}

/// A hub told to record nothing records nothing.
///
/// Every hub built before this existed, and every test in this workspace that
/// builds one directly, has no session log. They must be unchanged — and the
/// cheapest way for that to stop being true is for the recorder to acquire a
/// default.
#[tokio::test]
async fn a_hub_with_no_log_writes_nothing_and_still_works() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(BotStore::open(dir.path())?);
    let hub = Arc::new(
        Hub::with_policy(Policy::allow_all()).with_internal_tools(Arc::new(
            botrosterd::bot_tools::BotTools::new(Arc::clone(&store)),
        )),
    );
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));

    let (mut sock, _) = connect_async(&format!("ws://{addr}/v1/tools")).await?;
    sock.send(Message::Text(serde_json::to_string(&Hello::harness())?))
        .await?;
    let _ = sock.next().await;

    sock.send(Message::Text(
        Frame::Request(Request::new(
            RpcId::Num(1),
            Method::SessionOpen,
            Some(json!({"bot": "scout"})),
        ))
        .encode(),
    ))
    .await?;
    let sid: SessionId = loop {
        let Some(Ok(Message::Text(t))) = sock.next().await else {
            anyhow::bail!("socket closed");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == RpcId::Num(1) {
                let Outcome::Result(v) = r.outcome else {
                    anyhow::bail!("session/open failed");
                };
                break serde_json::from_value::<SessionOpenResult>(v)?.session_id;
            }
        }
    };

    sock.send(Message::Text(
        Frame::Request(
            Request::new(
                RpcId::Num(2),
                Method::ToolCall,
                Some(json!({"call_id": "c1", "tool_id": "bot.list", "args": {}})),
            )
            .in_session(sid.clone()),
        )
        .encode(),
    ))
    .await?;
    let answered = loop {
        let Some(Ok(Message::Text(t))) = sock.next().await else {
            anyhow::bail!("socket closed");
        };
        if let Frame::Response(r) = Frame::decode(&t)? {
            if r.id == RpcId::Num(2) {
                break r.outcome;
            }
        }
    };
    assert!(
        matches!(answered, Outcome::Result(_)),
        "a hub with no log could not serve a tool: {answered:?}"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        store
            .sessions(&BotId("scout".into()))
            .expect("listing is readable")
            .is_empty(),
        "a hub that was never given a log wrote a record anyway"
    );
    Ok(())
}

/// A call that goes out to the guest is recorded when it comes back.
///
/// The path every `fs.*`, `shell.exec` and `browser.*` call takes, and the one
/// no other test here reaches: those calls are decided in `tool_call` and end
/// somewhere else entirely, in `finish_relay`. A mutation that removed the
/// record from that function left every test in this file passing, which is
/// what this exists to stop.
///
/// Both endings, in one test. A tool that fails is an ordinary outcome — a read
/// of a file that is not there — and is the half somebody debugging a Bot comes
/// looking for, so a recorder that kept only successes would be worse than
/// useless: it would show a Bot that tried nothing.
#[tokio::test]
async fn a_call_that_reaches_the_guest_is_recorded_with_how_it_ended() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;

    // A real guest on a real workspace, so the result recorded is one a tool
    // actually produced.
    let work = tempfile::tempdir()?;
    f.attach_guest(work.path()).await?;
    let sid = f.open(Some("scout")).await?;
    f.bind_with_patience(&sid, "botroster-workspace").await?;

    let wrote = f
        .call(
            &sid,
            "fs.write",
            json!({"path": "notes.md", "contents": "a week of work"}),
            None,
        )
        .await?;
    assert!(
        matches!(wrote, Outcome::Result(_)),
        "the write failed, so this proves nothing: {wrote:?}"
    );

    let missing = f
        .call(&sid, "fs.read", json!({"path": "nothing-here.md"}), None)
        .await?;
    assert!(
        matches!(missing, Outcome::Error(_)),
        "reading a file that is not there succeeded: {missing:?}"
    );

    let steps = f.steps("scout", &sid, 2).await;
    assert_eq!(steps.len(), 2, "expected two steps, got {steps:?}");

    let write = &steps[0];
    assert_eq!(write.tool, "fs.write");
    assert_eq!(write.decided, Decided::Policy);
    let Ended::Ok(result) = &write.ended else {
        panic!("a successful write was recorded as {:?}", write.ended);
    };
    assert!(
        result.is_complete(),
        "a result this small should have been kept whole"
    );
    assert!(
        write.args.head.contains("notes.md"),
        "the record does not say what was written: {}",
        write.args.head
    );

    let read = &steps[1];
    assert_eq!(read.tool, "fs.read");
    let Ended::Failed(why) = &read.ended else {
        panic!("a failed read was recorded as {:?}", read.ended);
    };
    assert!(
        !why.head.is_empty(),
        "a failure was recorded with nothing said about it"
    );
    Ok(())
}

// ── replaying a record ────────────────────────────────────────────────────

/// The result the record holds for one step, or a panic saying what it holds
/// instead.
fn recorded_ok(step: &botrosterd::record::Step) -> String {
    match &step.ended {
        botrosterd::record::Ended::Ok(c) => c.head.clone(),
        other => panic!("step was recorded as {other:?}, not as a success"),
    }
}

/// **A replay touches nothing**, and the proof is a file that does not appear.
///
/// The property the whole feature rests on, and the one that cannot be checked
/// by asserting an absence of evidence: a guest that is never contacted
/// produces nothing to look at, so "no calls happened" is a test that passes
/// just as well when the plumbing is missing entirely.
///
/// So this is byte-level. A real `fs.write` is recorded against a real guest and
/// the file is confirmed on disk. The file is then deleted and the run
/// replayed. If a single call reached the guest, the file comes back. It must
/// not — and the recorded result must still be returned.
#[tokio::test]
async fn a_replay_answers_from_the_record_and_the_file_never_reappears() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let work = tempfile::tempdir()?;
    f.attach_guest(work.path()).await?;

    let live = f.open(Some("scout")).await?;
    f.bind_with_patience(&live, "botroster-workspace").await?;
    let wrote = f
        .call(
            &live,
            "fs.write",
            json!({"path": "notes.md", "contents": "a week of work"}),
            None,
        )
        .await?;
    assert!(matches!(wrote, Outcome::Result(_)), "{wrote:?}");

    let on_disk = work.path().join("notes.md");
    assert!(
        on_disk.exists(),
        "the recorded run did not write the file, so deleting it below proves nothing"
    );
    let recorded = f.steps("scout", &live, 1).await;
    assert_eq!(recorded.len(), 1);
    let expected = recorded_ok(&recorded[0]);

    // From here on, the evidence is its absence.
    std::fs::remove_file(&on_disk)?;

    let again = f.replay("scout", &live).await?;
    // Deliberately *not* bound to a tool server. A replaying session has
    // nothing to bind, and a call that escaped the replay branch would fail on
    // the missing binding rather than quietly writing the file — so both ways
    // of getting this wrong are visible rather than one.
    let replayed = f
        .call(
            &again,
            "fs.write",
            json!({"path": "notes.md", "contents": "a week of work"}),
            None,
        )
        .await?;

    match replayed {
        // `output`, because the response is a `ToolCallResult` and the record
        // holds what the tool returned rather than the frame it travelled in.
        // The frame's `call_id` belongs to *this* call and must not be the
        // recorded one.
        Outcome::Result(v) => {
            assert_eq!(
                serde_json::to_string(&v["output"])?,
                expected,
                "the replay did not return what was recorded"
            );
            assert_eq!(
                v["call_id"].as_str(),
                Some("call-7"),
                "the replay answered with the recorded call's id instead of this one's: {v}"
            );
        }
        Outcome::Error(e) => panic!("the replay failed: [{}] {}", e.code, e.message),
    }

    assert!(
        !on_disk.exists(),
        "replaying `fs.write` wrote the file, so a call reached the guest and this is not a \
         replay: {}",
        on_disk.display()
    );
    Ok(())
}

/// A run that asks for something else says what changed, and stops.
#[tokio::test]
async fn a_different_call_diverges_and_names_the_difference() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let live = f.open(Some("scout")).await?;
    f.call(&live, "bot.list", json!({}), None).await?;
    f.steps("scout", &live, 1).await;

    let again = f.replay("scout", &live).await?;
    let out = f
        .call(&again, "bot.send", json!({"to": "x", "text": "y"}), None)
        .await?;

    let Outcome::Error(e) = out else {
        panic!("a call the record does not contain was answered as though it did: {out:?}");
    };
    assert_eq!(e.code, botroster_proto::codes::DIVERGED, "{}", e.message);
    assert!(
        e.message.contains("bot.list") && e.message.contains("bot.send"),
        "the divergence does not say what was expected and what was asked for: {}",
        e.message
    );

    // And it stops, rather than reporting every later step as a difference too:
    // those would all be consequences of this one, burying the finding.
    let after = f.call(&again, "bot.list", json!({}), None).await?;
    let Outcome::Error(e2) = after else {
        panic!("the run carried on comparing after it had diverged");
    };
    assert_eq!(e2.code, botroster_proto::codes::DIVERGED);
    assert!(
        e2.message.contains("already diverged"),
        "the second failure should say the run is past comparing: {}",
        e2.message
    );
    Ok(())
}

/// The same tool with different arguments is a divergence too.
///
/// Compared on the hash of the canonical arguments rather than the kept prefix:
/// two long argument sets can share their first four kilobytes and differ
/// after, and calling those the same call is exactly what `Captured::sha256`
/// exists to prevent.
#[tokio::test]
async fn the_same_tool_with_different_arguments_diverges() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let live = f.open(Some("scout")).await?;
    f.call(&live, "bot.list", json!({"all": true}), None)
        .await?;
    f.steps("scout", &live, 1).await;

    let again = f.replay("scout", &live).await?;
    let out = f
        .call(&again, "bot.list", json!({"all": false}), None)
        .await?;

    let Outcome::Error(e) = out else {
        panic!("different arguments were treated as the same call: {out:?}");
    };
    assert_eq!(e.code, botroster_proto::codes::DIVERGED, "{}", e.message);
    assert!(
        e.message.contains("different arguments"),
        "the message does not say the arguments differed: {}",
        e.message
    );
    Ok(())
}

/// A step that was refused replays as refused.
///
/// Replaying a refusal as a success would change what the Bot saw, which is the
/// one thing a replay must never do: the run would take a branch it never took,
/// and every step after it would be a divergence the replay itself caused.
#[tokio::test]
async fn a_refused_step_replays_as_refused() -> anyhow::Result<()> {
    let policy = Policy {
        rules: vec![Rule::deny("bot.send", "not from a test")],
        fallback: Action::Allow,
        grants: std::collections::BTreeSet::new(),
        allow_private_browser_destinations: false,
    };
    let mut f = Fixture::start(policy).await?;
    let live = f.open(Some("scout")).await?;
    f.call(&live, "bot.send", json!({"to": "x", "text": "y"}), None)
        .await?;
    f.steps("scout", &live, 1).await;

    let again = f.replay("scout", &live).await?;
    let out = f
        .call(&again, "bot.send", json!({"to": "x", "text": "y"}), None)
        .await?;

    let Outcome::Error(e) = out else {
        panic!("a refusal replayed as a success: {out:?}");
    };
    assert_eq!(
        e.code,
        botroster_proto::codes::APPROVAL_DENIED,
        "a replayed refusal should still read as a refusal: {}",
        e.message
    );
    Ok(())
}

/// **A replay is not itself recorded.**
///
/// It did not happen, so a record of it is a record of work nobody did — sitting
/// in the same directory as the real ones and listed by `botroster bot record`
/// as though the Bot had done it twice. The replay branch returns before
/// anything is written, so this holds structurally; it is asserted because
/// "structurally" is a claim about code, and code changes.
#[tokio::test]
async fn replaying_writes_no_record_of_its_own() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let live = f.open(Some("scout")).await?;
    f.call(&live, "bot.list", json!({}), None).await?;
    f.steps("scout", &live, 1).await;

    let before = f
        .store
        .sessions(&BotId("scout".into()))
        .expect("listing is readable");
    assert_eq!(before.len(), 1, "expected the one real session: {before:?}");

    let again = f.replay("scout", &live).await?;
    f.call(&again, "bot.list", json!({}), None).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let after = f
        .store
        .sessions(&BotId("scout".into()))
        .expect("listing is readable");
    assert_eq!(
        after, before,
        "replaying wrote a record of a run that never happened"
    );
    assert_eq!(
        f.steps("scout", &live, 1).await.len(),
        1,
        "replaying appended to the record it was reading"
    );
    Ok(())
}

/// Replaying something never recorded is refused when the session opens.
///
/// A caller asking for a replay is asking for the guarantee that nothing
/// happens. Quietly handing them a live session instead is the worst available
/// answer, because they would find out by watching it do the work.
#[tokio::test]
async fn a_replay_of_nothing_is_refused_at_the_door() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let missing = SessionId::new("sess-never-happened");
    let refused = f
        .replay("scout", &missing)
        .await
        .expect_err("replaying a session that was never recorded should be refused");
    let said = refused.to_string();
    assert!(
        said.contains("no record") && said.contains("sess-never-happened"),
        "the refusal does not say what could not be found: {said}"
    );
    Ok(())
}

/// An ordinary session is untouched by any of this.
///
/// The anti-vacuity half: every test above would pass on a hub that had broken
/// normal sessions entirely.
#[tokio::test]
async fn a_session_that_is_not_replaying_still_runs_for_real() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let work = tempfile::tempdir()?;
    f.attach_guest(work.path()).await?;

    let live = f.open(Some("scout")).await?;
    f.bind_with_patience(&live, "botroster-workspace").await?;
    let wrote = f
        .call(
            &live,
            "fs.write",
            json!({"path": "real.md", "contents": "this one happened"}),
            None,
        )
        .await?;
    assert!(matches!(wrote, Outcome::Result(_)), "{wrote:?}");
    assert!(
        work.path().join("real.md").exists(),
        "an ordinary session stopped doing the work"
    );
    Ok(())
}

/// A result the record could not keep whole is not replayed, and says so.
///
/// The record keeps a bounded prefix of every value. A longer one survives as a
/// prefix, a length and a hash — enough to *identify* it and not enough to
/// reproduce it. Handing the prefix back would be worse than refusing: it is a
/// truncated serialisation, so it usually will not even parse, and if it
/// happened to, the Bot would be reasoning about a value nobody ever gave it
/// and every step after would diverge for a reason the replay invented.
///
/// Distinct from a divergence, and the message has to make that distinction:
/// the Bot did nothing different. The record is the thing that cannot answer.
#[tokio::test]
async fn a_result_too_long_to_keep_is_not_replayed() -> anyhow::Result<()> {
    let mut f = Fixture::start(Policy::allow_all()).await?;
    let work = tempfile::tempdir()?;
    f.attach_guest(work.path()).await?;

    let live = f.open(Some("scout")).await?;
    f.bind_with_patience(&live, "botroster-workspace").await?;

    // Comfortably past `record::KEPT_BYTES`, so the read below cannot be kept.
    let big = "x".repeat(botrosterd::record::KEPT_BYTES * 3);
    f.call(
        &live,
        "fs.write",
        json!({"path": "big.md", "contents": big}),
        None,
    )
    .await?;
    let read = f
        .call(&live, "fs.read", json!({"path": "big.md"}), None)
        .await?;
    assert!(matches!(read, Outcome::Result(_)), "{read:?}");

    let steps = f.steps("scout", &live, 2).await;
    assert_eq!(steps.len(), 2, "{steps:?}");
    let botrosterd::record::Ended::Ok(kept) = &steps[1].ended else {
        panic!("the read was recorded as {:?}", steps[1].ended);
    };
    assert!(
        !kept.is_complete(),
        "the result fitted after all, so this test is not exercising truncation"
    );

    // Replayed up to the read, which cannot be answered.
    let again = f.replay("scout", &live).await?;
    let first = f
        .call(
            &again,
            "fs.write",
            json!({"path": "big.md", "contents": "x".repeat(botrosterd::record::KEPT_BYTES * 3)}),
            None,
        )
        .await?;
    assert!(
        matches!(first, Outcome::Result(_)),
        "the write should replay: its own result is small. Got {first:?}"
    );

    let out = f
        .call(&again, "fs.read", json!({"path": "big.md"}), None)
        .await?;
    let Outcome::Error(e) = out else {
        panic!("a result the record only holds a prefix of was replayed as though whole: {out:?}");
    };
    assert_eq!(
        e.code,
        botroster_proto::codes::NOT_REPLAYABLE,
        "a record that cannot answer is not the Bot diverging: {}",
        e.message
    );
    assert!(
        e.message.contains(&kept.bytes.to_string()),
        "the message does not say how much was returned: {}",
        e.message
    );
    Ok(())
}
