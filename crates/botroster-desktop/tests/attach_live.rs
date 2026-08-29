//! Attaching a local file, against the shipped binary and a live stack.
//!
//! The property is not "a file was copied" but "a Bot can read it". The guest
//! is jailed to its workspace, so an attachment that lands anywhere else is a
//! file nobody can open, and the copy would still look like it worked. So
//! this drives the whole join: ask the binary where the workspace is, put a
//! file there, and read it back through the hub, which is the path `fs.read`
//! takes for the Bot.

mod common;

use common::up::Up;

const BODY: &str = "the attachment landed, and a Bot could read it";

#[tokio::test(flavor = "multi_thread")]
async fn an_attached_file_is_readable_by_the_guest() {
    let up = Up::start().expect("botroster up");
    let botroster = common::up::botroster();
    let dir = tempfile::tempdir().expect("a folder to attach from");
    let src = dir.path().join("notes.md");
    std::fs::write(&src, BODY).expect("write the source");

    let at = botroster_desktop::attach::put(&botroster, &up.hub, &up.home, &src)
        .await
        .expect("could not attach the file");
    assert_eq!(at, "attachments/notes.md");

    // Readable through the hub, which is the half a copy alone does not
    // prove: the guest resolves every path under its own root.
    let out = tokio::process::Command::new(&botroster)
        .arg("call")
        .arg("fs.read")
        .arg(format!(r#"{{"path":"{at}"}}"#))
        .env("BOTROSTER_HUB_URL", &up.hub)
        // Driven straight at the hub with no `--home`, so it has nowhere to
        // find the token the hub requires. The window's children are given it
        // by `hub::token_at`; this one is given it here.
        .envs(up.token())
        .output()
        .await
        .expect("botroster call");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains(BODY),
        "the guest could not read the attachment: {said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A second file of the same name does not replace the first, end to end.
#[tokio::test(flavor = "multi_thread")]
async fn two_files_of_one_name_both_survive() {
    let up = Up::start().expect("botroster up");
    let botroster = common::up::botroster();
    let dir = tempfile::tempdir().expect("a folder");
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("report.txt"), "first").unwrap();
    std::fs::write(b.join("report.txt"), "second").unwrap();

    let one = botroster_desktop::attach::put(&botroster, &up.hub, &up.home, &a.join("report.txt"))
        .await
        .expect("first");
    let two = botroster_desktop::attach::put(&botroster, &up.hub, &up.home, &b.join("report.txt"))
        .await
        .expect("second");
    assert_ne!(one, two, "the second attachment replaced the first");

    // Both readable through the hub, which is the only place that can say
    // whether they landed where the guest looks.
    for (path, want) in [(one, "first"), (two, "second")] {
        let out = tokio::process::Command::new(&botroster)
            .arg("call")
            .arg("fs.read")
            .arg(format!(r#"{{"path":"{path}"}}"#))
            .env("BOTROSTER_HUB_URL", &up.hub)
            .envs(up.token())
            .output()
            .await
            .expect("botroster call");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(want),
            "{path} did not hold {want}"
        );
    }
}
