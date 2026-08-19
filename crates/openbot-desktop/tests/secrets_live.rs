//! Credentials supplied from the window, against the shipped binary.
//!
//! The interesting assertions are the negative ones. A store whose values can
//! be read back, or whose errors quote what you typed, is not a store, and
//! neither failure would show up in a test that only checked the value went in
//! and the name came out.

mod common;

use openbot_desktop::secrets;

/// Distinctive enough that finding it anywhere is unambiguous.
const VALUE: &str = "sk-live-NEVER-SHOW-THIS-4f2a9c";

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_can_be_supplied_from_the_window_and_never_read_back() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    assert!(
        secrets::list(&openbot, home)
            .await
            .expect("list")
            .is_empty(),
        "a fresh home holds no credentials"
    );

    secrets::set(&openbot, home, "stripe-token", VALUE)
        .await
        .expect("the window could not store a credential");

    let held = secrets::list(&openbot, home).await.expect("list");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].name, "stripe-token");
    assert!(
        !held[0].fingerprint.is_empty(),
        "a fingerprint is what tells two tokens apart, or confirms a rotation"
    );

    // **The listing must not carry the value.** This is the one listing in the
    // product where the interesting field is deliberately absent, and a client
    // that showed it would undo the reason the store exists.
    let rendered = format!("{held:?}");
    assert!(
        !rendered.contains(VALUE),
        "the credential came back out of the store: {rendered}"
    );
    assert!(
        !held[0].fingerprint.contains(VALUE),
        "the fingerprint is not a disguise for the value"
    );

    // Rotating changes the fingerprint, or it could not answer the only
    // question anyone asks it.
    secrets::set(&openbot, home, "stripe-token", "sk-live-rotated-9911")
        .await
        .expect("rotate");
    let after = secrets::list(&openbot, home).await.expect("list");
    assert_ne!(
        after[0].fingerprint, held[0].fingerprint,
        "the fingerprint did not change when the value did"
    );

    secrets::remove(&openbot, home, "stripe-token")
        .await
        .expect("remove");
    assert!(
        secrets::list(&openbot, home)
            .await
            .expect("list")
            .is_empty(),
        "forgetting a credential should forget it"
    );
}

/// The case people forget: the failure path. An error that quotes what you
/// typed puts the credential in a log, a bug report and a screenshot at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_failure_does_not_echo_the_value() {
    let home = tempfile::tempdir().expect("a home");
    let openbot = common::up::openbot();

    // A name the store will not take. What it refuses on is the binary's
    // business; that it refuses without repeating the value is ours.
    let err = secrets::set(&openbot, home.path(), "../escape", VALUE).await;
    if let Err(err) = err {
        let text = format!("{err:#}");
        assert!(
            !text.contains(VALUE),
            "the error carried the credential: {text}"
        );
    }

    // And an empty name is refused here rather than spawning anything at all.
    let err = secrets::set(&openbot, home.path(), "   ", VALUE)
        .await
        .expect_err("a credential needs a name");
    let text = format!("{err:#}");
    assert!(
        !text.contains(VALUE),
        "the error carried the credential: {text}"
    );
}

/// Values are destined for an `Authorization` header, so the store refuses
/// control characters, and the refusal explains itself, including the guess
/// that the value came out of a file with a trailing newline. That is the
/// store being right; a client must not paper over it by stripping the
/// character and storing something the person did not type.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_that_cannot_go_in_a_header_is_refused_with_a_reason() {
    let home = tempfile::tempdir().expect("a home");
    let openbot = common::up::openbot();

    let err = secrets::set(
        &openbot,
        home.path(),
        "pem",
        "line one
line two",
    )
    .await
    .expect_err("a control character should be refused");
    let text = format!("{err:#}");
    assert!(
        text.contains("control character"),
        "the refusal should say what is wrong with it: {text}"
    );
    assert!(
        !text.contains("line two"),
        "the refusal carried part of the value: {text}"
    );
    assert!(
        secrets::list(&openbot, home.path())
            .await
            .expect("list")
            .is_empty(),
        "a refused value must not be half-stored"
    );
}

/// Everything a header *can* carry has to survive byte for byte. The same
/// value stored twice fingerprinting the same is the proof it was not
/// trimmed, re-encoded or normalised on the way in.
#[tokio::test(flavor = "multi_thread")]
async fn an_awkward_but_legal_value_is_stored_as_given() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    let awkward = "Bearer  two spaces  and-a-trailing-space ";
    secrets::set(&openbot, home, "one", awkward)
        .await
        .expect("store");
    secrets::set(&openbot, home, "two", awkward)
        .await
        .expect("store again");
    let held = secrets::list(&openbot, home).await.expect("list");
    let one = held.iter().find(|e| e.name == "one").expect("one");
    let two = held.iter().find(|e| e.name == "two").expect("two");
    assert_eq!(
        one.fingerprint, two.fingerprint,
        "the same value fingerprinted differently, so something changed it"
    );

    // And a value that differs only in trailing whitespace is a different
    // value; trimming it silently would store something nobody typed.
    secrets::set(&openbot, home, "three", awkward.trim_end())
        .await
        .expect("store trimmed");
    let held = secrets::list(&openbot, home).await.expect("list");
    let three = held.iter().find(|e| e.name == "three").expect("three");
    assert_ne!(
        one.fingerprint, three.fingerprint,
        "a trailing space was silently trimmed off a credential"
    );
}
