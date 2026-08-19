//! Permission rules, through the binary the window drives.
//!
//! The property that matters is not that a rule can be written but that the
//! one written here is the one the hub enforces. `openbot-cli`'s own tests
//! prove the second half; this proves the client's end agrees with it.

mod common;

use openbot_desktop::policy::{self, Action, Rule, When};

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_written_from_the_window_reads_back_as_written() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    assert!(
        policy::list(&openbot, home).await.expect("list").is_empty(),
        "a fresh home configures no rules; the shipped default applies"
    );

    policy::add(
        &openbot,
        home,
        &Rule {
            action: Action::Deny,
            tool: "shell.exec".into(),
            when: None,
            reason: Some("read-only account".into()),
        },
    )
    .await
    .expect("add a deny");

    policy::add(
        &openbot,
        home,
        &Rule {
            action: Action::RequireApproval,
            tool: "fs.write".into(),
            when: Some(When {
                key: "path".into(),
                glob: "/etc/*".into(),
            }),
            reason: Some("system files".into()),
        },
    )
    .await
    .expect("add a narrowed ask");

    let rules = policy::list(&openbot, home).await.expect("list");
    assert_eq!(rules.len(), 2, "{rules:?}");
    assert_eq!(rules[0].action, Action::Deny);
    assert_eq!(
        rules[0].reason.as_deref(),
        Some("read-only account"),
        "the reason a person wrote must reach the approval they will read"
    );
    let when = rules[1].when.as_ref().expect("the narrowing survived");
    assert_eq!(when.key, "path");
    assert_eq!(
        when.glob, "/etc/*",
        "a narrowing lost in transit turns `deny writes to /etc` into `deny every write`"
    );

    policy::remove(&openbot, home, 1).await.expect("remove");
    let rules = policy::list(&openbot, home).await.expect("list");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].tool, "fs.write", "the wrong rule was removed");
}

/// The binary refuses a rule that stops a call without saying why, and the
/// client passes that refusal through rather than inventing a reason or
/// writing a blank one.
#[tokio::test(flavor = "multi_thread")]
async fn a_rule_that_stops_a_call_without_a_reason_is_refused() {
    let home = tempfile::tempdir().expect("a home");
    let openbot = common::up::openbot();

    let err = policy::add(
        &openbot,
        home.path(),
        &Rule {
            action: Action::Deny,
            tool: "shell.exec".into(),
            when: None,
            reason: None,
        },
    )
    .await
    .expect_err("a deny with no reason should be refused");
    assert!(
        format!("{err:#}").contains("--reason"),
        "the refusal should say what is missing: {err}"
    );
    assert!(
        policy::list(&openbot, home.path())
            .await
            .expect("list")
            .is_empty(),
        "a refused rule must not be half-written"
    );
}

/// Removing something that is not there is an error a person can act on, not
/// a silent no-op that leaves them thinking a rule is gone.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_rule_that_is_not_there_says_so() {
    let home = tempfile::tempdir().expect("a home");
    let openbot = common::up::openbot();
    let err = policy::remove(&openbot, home.path(), 3)
        .await
        .expect_err("there is no rule 3");
    assert!(format!("{err:#}").contains("no rule 3"), "{err}");
}
