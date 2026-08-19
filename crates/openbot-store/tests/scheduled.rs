//! Retention for snapshots a schedule took, and only those.
//!
//! A timer that trims by total count eventually deletes a snapshot somebody
//! took by hand before a risky change, silently. Prune is the only
//! irreversible operation in the store, and a background timer is the caller
//! with nobody watching, so it must be scoped to its own label.

use openbot_store::Store;

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(d.path()).unwrap();
    (d, s)
}

#[test]
fn a_schedule_trims_its_own_history_and_nothing_else() {
    let (_d, s) = store();
    let v = s.volume("u").unwrap();

    // One hand-taken snapshot, then several scheduled ones on top of it.
    std::fs::write(v.workspace().join("work.md"), "the thing that matters").unwrap();
    let kept_by_hand = v.snapshot("before the risky migration").unwrap();
    for i in 0..6 {
        std::fs::write(v.workspace().join("work.md"), format!("edit {i}")).unwrap();
        v.snapshot("scheduled").unwrap();
    }
    assert_eq!(v.snapshots().unwrap().len(), 7);

    let removed = v.prune_labelled("scheduled", 2).unwrap();
    assert_eq!(
        removed, 4,
        "trimmed the wrong number of scheduled snapshots"
    );

    let left = v.snapshots().unwrap();
    assert_eq!(left.len(), 3, "{left:?}");
    assert!(
        left.iter().any(|s| s.id == kept_by_hand.id),
        "the hand-taken snapshot was trimmed by the schedule: {left:?}"
    );
    assert_eq!(
        left.iter().filter(|s| s.label == "scheduled").count(),
        2,
        "the schedule kept the wrong number of its own"
    );

    // The hand-taken snapshot still restores.
    std::fs::write(v.workspace().join("work.md"), "scribbled over").unwrap();
    v.restore(&kept_by_hand.id).unwrap();
    assert_eq!(
        std::fs::read_to_string(v.workspace().join("work.md")).unwrap(),
        "the thing that matters"
    );
}

#[test]
fn trimming_a_label_nothing_carries_removes_nothing() {
    let (_d, s) = store();
    let v = s.volume("u").unwrap();
    std::fs::write(v.workspace().join("a.txt"), "x").unwrap();
    for _ in 0..3 {
        v.snapshot("by hand").unwrap();
    }

    assert_eq!(v.prune_labelled("scheduled", 1).unwrap(), 0);
    assert_eq!(
        v.snapshots().unwrap().len(),
        3,
        "a schedule that has never run deleted somebody's snapshots"
    );
}
