//! Connectors and routines, read from the shipped binary.

mod common;

use openbot_desktop::settings;

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
async fn the_window_can_see_what_runs_on_a_schedule() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let openbot = common::up::openbot();

    assert!(
        settings::routines(&openbot, home)
            .await
            .expect("routines")
            .is_empty(),
        "a fresh home schedules nothing"
    );

    run(home, &["bot", "new", "Account Health"]);
    run(
        home,
        &[
            "routine",
            "new",
            "Account Health",
            "morning",
            "--cron",
            "0 9 * * *",
            "--instructions",
            "review the portfolio",
        ],
    );

    let all = settings::routines(&openbot, home).await.expect("routines");
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].id, "morning");
    assert_eq!(all[0].bot, "account-health");
    assert!(
        all[0].trigger.contains("9:00"),
        "the trigger should read as words, not a cron string: {:?}",
        all[0].trigger
    );
    assert!(all[0].next.is_some(), "a scheduled routine has a next run");
    assert!(all[0].enabled);

    // **A paused routine must say so.** It keeps its definition and stops
    // running, so on screen it is identical to a working one, which is how
    // somebody discovers months later that the run nobody watches has not run.
    run(home, &["routine", "pause", "Account Health", "morning"]);
    let all = settings::routines(&openbot, home).await.expect("routines");
    assert!(
        !all[0].enabled,
        "a paused routine still reported itself as enabled: {all:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_can_see_which_apps_are_connected() {
    let home = tempfile::tempdir().expect("a home");
    let openbot = common::up::openbot();
    assert!(
        settings::connectors(&openbot, home.path())
            .await
            .expect("connectors")
            .is_empty(),
        "a fresh home has nothing connected"
    );
}
