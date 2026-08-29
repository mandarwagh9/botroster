//! Whether the peer on the other end had to be entitled to be there.
//!
//! `upgrade.rs` closes this path for a web page, by the one header a browser
//! must send and cannot suppress. That leaves a program: a page cannot set
//! request headers on a WebSocket, but anything running on this machine can
//! open the socket and send whatever `hello` it likes. Until this file, it was
//! then handed `dev_principal()` — and since an approval is authorised on
//! socket identity, a peer that opens its own session is the owner of it and is
//! the one the hub asks for permission. It could approve its own `shell.exec`.
//!
//! The bar this raises is *anything that can open a socket* to *anything that
//! can read this user's files*, and no further. A program running as this user
//! reads `hub.token` exactly as the desktop client does. `Hello::token` says so
//! at length, and nothing here should be read as claiming otherwise.

use std::sync::Arc;
use std::time::Duration;

use botroster_proto::{codes, Hello, HelloAck, RpcError};
use botrosterd::hub::{Admission, Hub};
use botrosterd::policy::Policy;
use botrosterd::server::Server;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// A hub on an ephemeral port, admitting whoever `admission` says.
async fn hub_url(admission: Admission) -> anyhow::Result<String> {
    let hub = Arc::new(Hub::with_policy(Policy::allow_all()).admitting(admission));
    let (listener, addr) = Server::bind("127.0.0.1:0").await?;
    tokio::spawn(Arc::new(Server::new(hub)).serve(listener));
    Ok(format!("ws://{addr}/v1/tools"))
}

/// A `hello` presenting exactly what is asked for and nothing incidental.
///
/// Built field by field rather than through `Hello::harness()`, which fills
/// `token` from `hub_token()` — the environment, or the developer's own
/// `~/.botroster/hub.token`. A test whose subject is *which token was sent*
/// cannot let the machine it runs on supply one: the no-token cases would pass
/// or fail depending on whether the person running them had `botroster up`
/// going in another terminal.
fn hello_with(token: Option<&str>) -> Hello {
    let mut h = Hello::harness();
    h.token = token.map(str::to_owned);
    h
}

/// What the hub said to a `hello`: an ack, or the error it refused with.
async fn handshake(url: &str, hello: Hello) -> anyhow::Result<Result<HelloAck, RpcError>> {
    let (mut sock, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(url),
    )
    .await??;
    sock.send(Message::Text(serde_json::to_string(&hello)?))
        .await?;
    match sock.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<HelloAck>(&t) {
            Ok(a) => Ok(Ok(a)),
            // The refusal is written to the wire before the close, so that a
            // client gets a diagnosable reason rather than a bare disconnect.
            Err(_) => Ok(Err(serde_json::from_str::<RpcError>(&t)?)),
        },
        other => anyhow::bail!("expected a text frame answering the hello, got {other:?}"),
    }
}

#[tokio::test]
async fn a_hub_that_requires_a_token_refuses_a_peer_that_presents_none() -> anyhow::Result<()> {
    let url = hub_url(Admission::Token("s3cret".into())).await?;

    let Err(e) = handshake(&url, hello_with(None)).await? else {
        panic!(
            "a peer presenting no token was admitted to a hub that requires one. It now owns a \
             session, and a session's owner is who the hub asks before running shell.exec."
        );
    };
    assert_eq!(e.code, codes::UNAUTHENTICATED, "{}", e.message);
    Ok(())
}

#[tokio::test]
async fn a_hub_that_requires_a_token_refuses_a_wrong_one() -> anyhow::Result<()> {
    let url = hub_url(Admission::Token("s3cret".into())).await?;

    for wrong in [
        "",
        "s3cre",       // a prefix: the case a byte-at-a-time guess produces
        "s3cret ",     // trailing space, which `hub_token_in` trims on read
        "s3crets",     // a longer string sharing the whole token as its prefix
        "S3CRET",      // right bytes, wrong case
        "not-the-one", // nothing alike
    ] {
        let Err(e) = handshake(&url, hello_with(Some(wrong))).await? else {
            panic!("{wrong:?} was admitted to a hub whose token is \"s3cret\"");
        };
        assert_eq!(
            e.code,
            codes::UNAUTHENTICATED,
            "for {wrong:?}: {}",
            e.message
        );
    }
    Ok(())
}

/// The anti-vacuity half. A hub that refused everything would satisfy both
/// tests above and would be a worse outcome than the hole it closes: the CLI,
/// the desktop client and the guest all connect here.
#[tokio::test]
async fn the_token_the_hub_asked_for_is_admitted() -> anyhow::Result<()> {
    let url = hub_url(Admission::Token("s3cret".into())).await?;

    match handshake(&url, hello_with(Some("s3cret"))).await? {
        Ok(ack) => assert!(!ack.connection_id.as_str().is_empty()),
        Err(e) => panic!("the correct token was refused: {} ({})", e.message, e.code),
    }
    Ok(())
}

/// Every hub built before this existed admitted everyone, and a great many
/// still are: `botrosterd` on a home with no token file, and every test in this
/// workspace that builds a `Hub` directly. Requiring a token there would have
/// been a silent breaking change discovered by other people.
#[tokio::test]
async fn a_hub_admitting_anyone_still_takes_a_peer_with_no_token() -> anyhow::Result<()> {
    let url = hub_url(Admission::Anyone).await?;

    match handshake(&url, hello_with(None)).await? {
        Ok(ack) => assert!(!ack.connection_id.as_str().is_empty()),
        Err(e) => panic!(
            "a hub admitting anyone refused a peer with no token: {}",
            e.message
        ),
    }
    // And a peer that presents one anyway — the shipped clients always do, from
    // whatever home they found — is not refused for the trouble.
    match handshake(&url, hello_with(Some("whatever"))).await? {
        Ok(ack) => assert!(!ack.connection_id.as_str().is_empty()),
        Err(e) => panic!(
            "a hub admitting anyone refused an unsolicited token: {}",
            e.message
        ),
    }
    Ok(())
}

/// A peer this hub does not admit learns nothing about it.
///
/// The check sits ahead of every other one in `register`, so a `hello` that is
/// wrong in two ways is answered for the one that does not describe the hub.
/// Reverse the order and this reports the protocol version to a caller that was
/// never entitled to ask.
#[tokio::test]
async fn an_unadmitted_peer_is_not_told_what_else_was_wrong() -> anyhow::Result<()> {
    let url = hub_url(Admission::Token("s3cret".into())).await?;

    let mut hello = hello_with(None);
    hello.protocol_version = "0.0.0-not-a-version".into();

    let Err(e) = handshake(&url, hello).await? else {
        panic!("a peer with no token and an impossible protocol version was admitted");
    };
    assert_eq!(e.code, codes::UNAUTHENTICATED, "{}", e.message);
    assert!(
        !e.message.contains("protocol_version"),
        "the refusal told an unadmitted peer which protocol versions this hub speaks: {}",
        e.message
    );
    Ok(())
}

/// The refusal names the file, because the overwhelmingly likely cause is not
/// an attacker: it is a second terminal addressing a different home. A bare
/// "unauthorised" sends that person looking for a password that does not exist.
#[tokio::test]
async fn the_refusal_says_what_to_do_about_it() -> anyhow::Result<()> {
    let url = hub_url(Admission::Token("s3cret".into())).await?;

    let Err(e) = handshake(&url, hello_with(None)).await? else {
        panic!("admitted");
    };
    assert!(
        e.message.contains(botroster_proto::HUB_TOKEN_FILE),
        "the refusal does not name the file that holds the token: {}",
        e.message
    );
    assert!(
        e.message.contains(botroster_proto::HUB_TOKEN_ENV),
        "the refusal does not name the variable that overrides it: {}",
        e.message
    );
    Ok(())
}

/// `Admission::from_home` is the switch `botrosterd` runs on: write a token into
/// the home and the daemon starts requiring it, with no flag to discover.
#[tokio::test]
async fn a_home_with_a_token_requires_it_and_a_home_without_one_does_not() -> anyhow::Result<()> {
    let d = tempfile::tempdir()?;

    assert!(
        matches!(Admission::from_home(d.path()), Admission::Anyone),
        "a home with no token file must not start refusing its own daemon's peers"
    );

    botroster_proto::write_hub_token(d.path(), "from-the-home")?;
    let url = hub_url(Admission::from_home(d.path())).await?;

    assert!(
        handshake(&url, hello_with(None)).await?.is_err(),
        "the home holds a token and the hub built from it admitted a peer without one"
    );
    assert!(
        handshake(&url, hello_with(Some("from-the-home")))
            .await?
            .is_ok(),
        "the hub refused the token its own home holds"
    );
    Ok(())
}
