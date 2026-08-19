//! Skills, read from the shipped binary.
//!
//! The unit tests beside the reader parse JSON copied from a real run. This
//! asks the real run for it, which is the only thing that fails when the
//! command's output changes; a hand-copied fixture stays green forever.

mod common;

use openbot_desktop::skills;

fn run(home: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new(common::up::openbot())
        .args(args)
        .arg("--home")
        .arg(home)
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HOME")
        .env_remove("OPENBOT_HUB_URL")
        .output()
        .expect("could not run openbot");
    assert!(
        out.status.success(),
        "`openbot {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_can_offer_the_skills_a_bot_would_actually_use() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    // A first run, before anybody writes one. Empty, not broken: this is the
    // state the window is in on the day it is installed.
    let empty = skills::catalog(&openbot, home)
        .await
        .expect("an empty home");
    assert_eq!(
        empty,
        skills::Catalog::default(),
        "a home with no skills should read as empty, not fail: {empty:?}"
    );

    run(
        home,
        &[
            "skill",
            "new",
            "refund-a-customer",
            "--description",
            "How to issue a refund",
        ],
    );

    let cat = skills::catalog(&openbot, home).await.expect("the catalog");
    assert_eq!(cat.skills.len(), 1, "{cat:?}");
    assert_eq!(cat.skills[0].name, "refund-a-customer");
    assert_eq!(
        cat.skills[0].description, "How to issue a refund",
        "the sentence a person picks from has to survive the trip: {cat:?}"
    );
    assert!(cat.problems.is_empty(), "nothing is broken yet: {cat:?}");
}

/// **A skill that does not load is the one worth showing.** `openbot skill new`
/// says "created" and the file is on disk, so a person believes the procedure
/// is in force; the Bot has been ignoring it since the moment it stopped
/// parsing. If this never reached the window, the window would be the place
/// that hid it.
#[tokio::test(flavor = "multi_thread")]
async fn a_half_written_skill_reaches_the_window_rather_than_vanishing() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    run(
        home,
        &["skill", "new", "works", "--description", "This one loads"],
    );

    // Written by hand rather than by `skill new`, because `skill new` cannot
    // produce a broken one, which is the point: these appear when somebody
    // edits a file afterwards.
    let broken = home.join("skills").join("half-written");
    std::fs::create_dir_all(&broken).expect("a place for it");
    std::fs::write(broken.join("SKILL.md"), "no frontmatter at all\n").expect("write it");

    let cat = skills::catalog(&openbot, home).await.expect("the catalog");
    assert_eq!(
        cat.skills.len(),
        1,
        "the working skill is still offered: {cat:?}"
    );
    assert_eq!(cat.skills[0].name, "works");
    assert_eq!(
        cat.problems.len(),
        1,
        "the broken one was dropped on the way to the window: {cat:?}"
    );
    assert!(
        cat.problems[0].path.contains("half-written"),
        "a problem has to name the file, or nobody can fix it: {cat:?}"
    );
    assert!(
        !cat.problems[0].why.trim().is_empty(),
        "and say what is wrong with it: {cat:?}"
    );
}
