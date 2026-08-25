//! The roster, read from the shipped binary.
//!
//! `roster::list` and `botroster bot ls --json` are two ends of one wire and each
//! has a unit test pinning its own end. This is the test that catches them
//! disagreeing, which is the failure that matters: a sidebar showing nothing
//! looks exactly like a person who has no Bots yet.

mod common;

use botroster_desktop::roster;

/// Create a Bot through the shipped binary, the way the CLI does.
fn make(home: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new(common::up::botroster())
        .arg("bot")
        .arg("--home")
        .arg(home)
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL")
        .output()
        .expect("could not run botroster");
    assert!(
        out.status.success(),
        "`botroster bot {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Any `botroster` command. `make` prepends `bot`, which is right for the tests
/// that only make Bots and wrong for the one that also makes a group and a
/// routine.
fn run(home: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new(common::up::botroster())
        .args(args)
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL")
        .output()
        .expect("could not run botroster");
    assert!(
        out.status.success(),
        "`botroster {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_reads_the_roster_the_binary_writes() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    make(
        home,
        &[
            "new",
            "Talent Scout",
            "--title",
            "Hiring",
            "--description",
            "Finds candidates",
        ],
    );
    make(home, &["new", "Expense Manager"]);
    make(home, &["hide", "Expense Manager"]);

    let visible = roster::list(&common::up::botroster(), home, false)
        .await
        .expect("the roster should be readable");
    assert_eq!(
        visible.len(),
        1,
        "a hidden Bot belongs out of the main list: {visible:?}"
    );
    let scout = &visible[0];
    assert_eq!(scout.name, "Talent Scout");
    assert_eq!(scout.title, "Hiring", "the sidebar shows the job");
    assert_eq!(scout.description, "Finds candidates");
    assert!(!scout.id.is_empty(), "the id is what the client addresses");
    assert!(!scout.hidden);

    // "Show hidden chats".
    let all = roster::list(&common::up::botroster(), home, true)
        .await
        .expect("the full roster should be readable");
    assert_eq!(all.len(), 2, "--all should include the hidden one: {all:?}");
    assert!(
        all.iter().any(|b| b.name == "Expense Manager" && b.hidden),
        "a hidden Bot must say so, or it renders identically to the rest: {all:?}"
    );
}

/// A home nobody has used is an empty roster, not an error and not a crash.
/// It is the first-run state.
#[tokio::test(flavor = "multi_thread")]
async fn an_untouched_home_has_an_empty_roster() {
    let home = tempfile::tempdir().expect("a home");
    let bots = roster::list(&common::up::botroster(), home.path(), false)
        .await
        .expect("an empty roster is not a failure");
    assert!(bots.is_empty(), "expected nobody, got {bots:?}");
}

/// A roster that cannot be read must say so. Showing an empty sidebar on
/// failure is indistinguishable from having no Bots, and the person would go
/// looking for their work rather than for the error.
#[tokio::test(flavor = "multi_thread")]
async fn a_roster_that_cannot_be_read_is_an_error_not_an_empty_list() {
    let home = tempfile::tempdir().expect("a home");
    let missing = home.path().join("there-is-no-botroster-here");
    let err = roster::list(&missing, home.path(), false)
        .await
        .expect_err("a missing binary is not an empty roster");
    assert!(
        err.to_string().contains("could not run"),
        "the error should name what it could not do, got {err}"
    );
}

/// The handoff between members is visible in one conversation, which is the
/// reason to put Bots in a group. `botroster group log` reads that thread back.
#[tokio::test(flavor = "multi_thread")]
async fn the_client_can_read_a_group_and_the_thread_it_preserves() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let botroster = common::up::botroster();

    assert!(
        roster::groups(&botroster, home)
            .await
            .expect("groups")
            .is_empty(),
        "a fresh home has no groups"
    );

    for who in ["Researcher", "Writer", "Reviewer"] {
        make(home, &["new", who]);
    }
    let out = std::process::Command::new(&botroster)
        .args([
            "group",
            "new",
            "Website Launch",
            "--members",
            "researcher,writer,reviewer",
        ])
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("BOTROSTER_HOME")
        .env_remove("BOTROSTER_HUB_URL")
        .output()
        .expect("could not run botroster");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let groups = roster::groups(&botroster, home).await.expect("groups");
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].name, "Website Launch");
    assert_eq!(
        groups[0].members.len(),
        3,
        "all three members should be listed: {groups:?}"
    );
    assert_eq!(
        groups[0].members[0].id, "researcher",
        "the coordinator must lead; it answers anything nobody was mentioned in"
    );
    // The name comes with it, because that is what a sidebar renders and it
    // stops matching the id after the first rename.
    assert!(
        !groups[0].members[0].name.is_empty(),
        "a member with no name leaves the sidebar showing a slug: {groups:?}"
    );

    // The thread is empty and readable; an unreadable thread would look the
    // same from outside without this check.
    let thread = roster::group_log(&botroster, home, "Website Launch")
        .await
        .expect("the thread should be readable");
    assert_eq!(thread, serde_json::json!([]));

    let missing = roster::group_log(&botroster, home, "no-such-group").await;
    assert!(missing.is_err(), "an unknown group should be an error");
}

/// A rename has to reach everywhere the Bot's name is shown.
///
/// Renaming keeps the id, because the id is what a group's membership and a
/// routine's owner are stored under. That creates a hazard: those places now
/// hold a string that is no longer the Bot's name, and a window rendering
/// them raw would show `talent-scout` in the sidebar under an entry reading
/// "Recruiting".
#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_bot_reaches_the_group_and_the_routine_that_hold_it() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let botroster = common::up::botroster();

    run(home, &["bot", "new", "Talent Scout"]);
    run(home, &["bot", "new", "Writer"]);
    run(
        home,
        &["group", "new", "Launch", "--members", "talent-scout,writer"],
    );
    run(
        home,
        &[
            "routine",
            "new",
            "talent-scout",
            "morning",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "check the pipeline",
        ],
    );

    run(
        home,
        &["bot", "set", "talent-scout", "--rename", "Recruiting"],
    );

    let groups = roster::groups(&botroster, home).await.expect("groups");
    let member = groups[0]
        .members
        .iter()
        .find(|m| m.id == "talent-scout")
        .unwrap_or_else(|| panic!("the group lost its member: {groups:?}"));
    assert_eq!(
        member.name, "Recruiting",
        "the sidebar would show the old name under the new one: {groups:?}"
    );
    assert_eq!(
        member.id, "talent-scout",
        "the id moved, so the group points at a Bot that is not there: {groups:?}"
    );

    let routines = botroster_desktop::settings::routines(&botroster, home)
        .await
        .expect("routines");
    assert_eq!(routines[0].bot, "talent-scout", "{routines:?}");
    assert_eq!(
        routines[0].bot_name, "Recruiting",
        "the settings panel would name a Bot nobody can find: {routines:?}"
    );
}
