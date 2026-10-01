//! `PROVENANCE.md` claims this crate is not wire-compatible with upstream, and
//! then says exactly where. Those two halves have to stay true together.
//!
//! The false claim this file exists to prevent is not hypothetical and it is
//! not subtle: `botroster-proto` carried "wire-compatible with the published
//! Grok Build protocol" in the README, in this crate's module docs, and in the
//! provenance table, while disagreeing with that protocol on the first field
//! either end reads. A reader who believed any of the three would build against
//! a peer that cannot work.
//!
//! Two things can rot independently, and what is checked here is not symmetric:
//!
//! 1. **A divergence stops being real** — someone renames `call_id` to
//!    `tool_call_id` and the table is now describing a difference that is gone.
//!    This is fully checked: the table's BOTROSTER column is read out of the
//!    live types, and a mismatch fails.
//! 2. **A real divergence stops being recorded** — someone adds a field the
//!    table never mentions. This is **not** checked, and cannot be under the
//!    project's decision to stay independent (D1 = A): with no upstream crate
//!    in this tree there is nothing to diff against, so "a divergence appeared"
//!    is not an event any test can observe. It is a reading task, and it is
//!    done by re-reading both trees when the pinned `SOURCE_REV` moves.
//!
//! One exception to (2) is checked, because it is a closed set rather than an
//! open one: the error numbers are enumerated on both sides, so the collisions
//! among them are computed by set difference and cannot be forgotten. That
//! asymmetry is deliberate and is the reason the error rows get their own tests.
//!
//! The same limit applies to the upstream column generally: it is a recorded
//! fact about a pinned external revision, not something a test can read. If
//! upstream syncs and moves a field, only a human re-reading its tree will find
//! out — which is why [`the_divergences_are_pinned_to_a_source_revision`] exists
//! as a reminder that a revision, not a version string, is what the table rests
//! on.
//!
//! Nothing here proves *upstream*'s spelling. What this file holds both sides to
//! is our own half, read out of the live types.

use botroster_proto::codes;
use botroster_proto::frames::ToolCallRequestParams;
use botroster_proto::{ConnectionId, HelloAck, Method, ToolCallId, ToolId, UserId};
use serde_json::json;

fn provenance() -> String {
    // CARGO_MANIFEST_DIR is crates/botroster-proto.
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("PROVENANCE.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Rows of the divergence table, as `(whole_line, cell_text)` pairs.
///
/// Deliberately dumb: a row is a line starting with `|`, and a cell is its text
/// between backticks. A cleverer parser would be a second thing to keep correct,
/// and the only property that matters here is "does some row mention both
/// spellings".
fn rows(md: &str) -> Vec<String> {
    md.lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .map(str::to_owned)
        .collect()
}

fn recorded(md: &str, ours: &str, theirs: &str) -> bool {
    rows(md)
        .iter()
        .any(|r| r.contains(&format!("`{ours}`")) && r.contains(&format!("`{theirs}`")))
}

/// A table row that names both spellings, or the test says which pair is
/// missing — the failure has to name the divergence, or fixing it means
/// re-deriving the whole list from the error message.
fn assert_recorded(md: &str, what: &str, ours: &str, theirs: &str) {
    assert!(
        recorded(md, ours, theirs),
        "PROVENANCE.md records no divergence row naming both `{ours}` (ours) and \
         `{theirs}` (upstream's) for {what}. Either the divergence is gone — then \
         this row should be deleted — or it is real and undocumented, which is the \
         false claim this file exists to prevent."
    );
}

fn ack_json() -> serde_json::Value {
    serde_json::to_value(HelloAck {
        connection_id: ConnectionId::new("conn-1"),
        user_id: UserId::new("user-1"),
        hub_version: "0.0.0".to_owned(),
        supported_protocol_versions: vec![],
        capabilities: vec![],
    })
    .unwrap()
}

fn call_json() -> serde_json::Value {
    serde_json::to_value(ToolCallRequestParams {
        tool_id: ToolId::new("fs.read"),
        call_id: ToolCallId::new("call-1"),
        args: json!({}),
    })
    .unwrap()
}

/// The recorded divergences are real: our types use our spelling, and the
/// spelling they replaced is genuinely absent rather than merely unused.
///
/// This half is what stops the table from outliving the code.
#[test]
fn the_divergences_the_table_records_are_real() {
    // Slice B1 matched this one: the ack now serialises the field under the name
    // the published protocol uses, so it is no longer a divergence and the table
    // row for it is gone. This assertion exists to notice if it ever moves back,
    // in which case the table needs the row again.
    let ack = ack_json();
    assert!(
        ack.get("computer_hub_version").is_some() && ack.get("hub_version").is_none(),
        "the hello ack version field moved; if this is a divergence again, \
         PROVENANCE.md §1 needs its row back"
    );
    // The old spelling still parses, so a stored ack or fixture written before
    // B1 keeps reading.
    let legacy: serde_json::Value = serde_json::json!({
        "connection_id": "conn-1",
        "user_id": "user-1",
        "hub_version": "0.5.1",
        "supported_protocol_versions": ["botroster-1"],
    });
    let back: botroster_proto::HelloAck =
        serde_json::from_value(legacy).expect("the pre-B1 spelling must still parse");
    assert_eq!(back.hub_version, "0.5.1");

    let call = call_json();
    // Slice B2 matched this pair as well, on the same terms as the ack above:
    // the wire now carries the published names, the Rust fields did not move,
    // and the old spellings still parse so a stored run record or a fixture
    // written before the rename keeps reading.
    assert!(
        call.get("tool_call_id").is_some() && call.get("call_id").is_none(),
        "the tool call id field moved; if it is a divergence again, PROVENANCE.md §1 \
         needs its row back"
    );
    assert!(
        call.get("arguments").is_some() && call.get("args").is_none(),
        "the tool call arguments field moved; if it is a divergence again, \
         PROVENANCE.md §1 needs its row back"
    );
    let legacy_call: ToolCallRequestParams =
        serde_json::from_value(json!({ "tool_id": "fs.read", "call_id": "call-1", "args": {} }))
            .expect("the pre-B2 spellings must still parse");
    assert_eq!(legacy_call.call_id.as_str(), "call-1");

    assert_eq!(
        (codes::FORBIDDEN, codes::APPROVAL_DENIED),
        (-32004, -32005),
        "the error numbers moved; PROVENANCE.md's divergence table is stale"
    );
    assert_eq!(
        Method::ApprovalRequest.as_wire_str(),
        "approval.request",
        "the approval method name moved; PROVENANCE.md's divergence table is stale"
    );
}

/// Numbers `xai-tool-protocol::error_codes::ERROR_CODES` occupies, with the
/// meaning it gives each, read from that file at the pinned `SOURCE_REV`.
///
/// Recorded here rather than derived, because the project's decision to stay
/// independent (D1 = A) means there is no upstream crate in this tree to read at
/// test time. The consequence is real and stated in the file header: this list is
/// a snapshot that a future upstream sync can invalidate silently.
const UPSTREAM_APPLICATION_CODES: &[(i32, &str)] = &[
    (-32001, "timeout"),
    (-32002, "unauthorized"),
    (-32003, "forbidden"),
    (-32004, "connection_lost"),
    (-32005, "tool_server_gone"),
    (-32006, "session_not_found"),
    (-32008, "session_draining"),
];

/// Every code this crate defines in the application range, paired with the name
/// it goes by in `botroster_proto::codes`.
fn our_application_codes() -> Vec<(i32, &'static str)> {
    vec![
        (
            botroster_proto::WORKSPACE_UNAVAILABLE_CODE,
            "WORKSPACE_UNAVAILABLE",
        ),
        (codes::SESSION_NOT_FOUND, "SESSION_NOT_FOUND"),
        (codes::NO_SERVER_BOUND, "NO_SERVER_BOUND"),
        (codes::FORBIDDEN, "FORBIDDEN"),
        (codes::APPROVAL_DENIED, "APPROVAL_DENIED"),
        (codes::TAKEN_OVER, "TAKEN_OVER"),
        (codes::UNAUTHENTICATED, "UNAUTHENTICATED"),
        (codes::DIVERGED, "DIVERGED"),
        (codes::NOT_REPLAYABLE, "NOT_REPLAYABLE"),
        (codes::TOOL_FAILED, "TOOL_FAILED"),
    ]
}

/// Every error number we share with upstream is recorded, with both meanings.
///
/// The collision is the dangerous kind, not the mismatch. A peer that maps
/// numbers reads our `FORBIDDEN` as "the connection dropped" and our
/// `APPROVAL_DENIED` as "the tool server went away", and both invite a retry of a
/// call that was deliberately refused. So completeness matters more here than
/// anywhere else in this file, which is why it is checked by set difference rather
/// than left to a human reading the table.
#[test]
fn every_error_number_we_share_with_upstream_is_recorded_with_both_meanings() {
    let md = provenance();
    let mut unrecorded = Vec::new();

    for (n, ours) in our_application_codes() {
        let Some((_, theirs)) = UPSTREAM_APPLICATION_CODES.iter().find(|(m, _)| *m == n) else {
            continue;
        };
        let number = n.to_string();
        if !recorded(&md, &number, ours) || !recorded(&md, &number, theirs) {
            unrecorded.push(format!("{number}: ours {ours}, theirs {theirs}"));
        }
    }

    assert!(
        unrecorded.is_empty(),
        "these error numbers are shared with upstream and PROVENANCE.md does not \
         record the collision with both meanings. A peer that maps numbers would read \
         our code as something else entirely:\n  {}",
        unrecorded.join("\n  ")
    );
}

/// Every number the error-number table lists is one upstream actually occupies.
///
/// The other direction, and the one that produced a false row. `-32007` appears
/// nowhere in the upstream tree at the pinned revision — upstream's table jumps
/// from `-32006` to `-32008` — but an earlier draft of `PROVENANCE.md` listed it
/// as upstream's, with a note in our column rather than a code name. In a
/// provenance document one fabricated row discredits the accurate ones beside
/// it.
///
/// Scoped to the table rather than to a pairing of names, because the first
/// version of this test looked for our code name beside the number and so
/// missed exactly the row it was written for: that row's BOTROSTER column read
/// "refused, naming the token file to read", which names no code at all. Reading
/// the numbers out of the table is what actually catches it.
#[test]
fn the_error_number_table_lists_no_number_upstream_leaves_free() {
    let md = provenance();
    let mut listed = Vec::new();
    let mut in_error_table = false;

    for line in md.lines() {
        if !line.trim_start().starts_with('|') {
            // A blank line ends a table; prose between tables is not one.
            if line.trim().is_empty() {
                in_error_table = false;
            }
            continue;
        }
        if line.contains("error number") {
            in_error_table = true;
            continue;
        }
        if !in_error_table {
            continue;
        }
        for tok in line.split(|c: char| !(c.is_ascii_digit() || c == '-')) {
            if tok.len() == 6 && tok.starts_with("-320") {
                let Ok(n) = tok.parse::<i32>() else { continue };
                listed.push(n);
            }
        }
    }

    assert!(
        !listed.is_empty(),
        "found no error-number table in PROVENANCE.md, so this test is not looking at \
         anything. A gate that passes because it found nothing is the failure mode \
         this repository's own review.sh warns about."
    );

    let fabricated: Vec<_> = listed
        .iter()
        .filter(|n| !UPSTREAM_APPLICATION_CODES.iter().any(|(m, _)| m == *n))
        .collect();

    assert!(
        fabricated.is_empty(),
        "the error-number table lists {fabricated:?}, which upstream defines no code at \
         the pinned SOURCE_REV — so there is nothing there to collide with. Upstream's \
         table goes -32006, then -32008.",
    );
}

/// The divergences are written down. This half is what stops a corrected claim
/// decaying back into a bare assertion nobody can check.
#[test]
fn every_divergence_is_written_down_in_provenance() {
    let md = provenance();
    // No assertion for the tool call id or arguments fields: both are matched
    // now, so there is no divergence to record and a row naming both spellings
    // would be describing a difference that is gone.
    assert_recorded(
        &md,
        "the approval request method",
        "approval.request",
        "permission_request",
    );
    // Our side has no reply *method*: the decision comes back as the JSON-RPC
    // result of `approval.request`, carrying `ApprovalDecision`. Upstream sends a
    // second frame instead, so the divergence is in the shape, not a name.
    assert_recorded(
        &md,
        "the approval reply mechanism",
        "ApprovalDecision",
        "hook_reply",
    );
    assert_recorded(&md, "the tool id charset", "fs.read", "[a-zA-Z0-9_-]+");
}

/// The revision the table was measured against is pinned, so "check the current
/// upstream" is not a scavenger hunt through a moving target.
///
/// Upstream syncs every few days and did not bump its own protocol version
/// across any of it, so the revision is the only thing that makes a row in this
/// table mean a specific set of types.
#[test]
fn the_divergences_are_pinned_to_a_source_revision() {
    let md = provenance();
    assert!(
        md.contains("559751fdcec02d413e4c57c8832ab275e4f44980"),
        "PROVENANCE.md does not pin the SOURCE_REV the divergence table was measured \
         against. Without it the table silently rots: upstream publishes a sync every \
         few days and never bumps PROTOCOL_VERSION, so the version string cannot \
         stand in for the revision."
    );
}

/// The table exists and says what it is.
///
/// Separate from the row checks because "no table" and "a table missing a row"
/// are different failures with different fixes, and a reader hitting this needs
/// to be told which one they have.
#[test]
fn provenance_says_plainly_that_it_is_not_wire_compatible() {
    let md = provenance();
    assert!(
        md.contains("559751fdcec02d413e4c57c8832ab275e4f44980")
            && md.to_lowercase().contains("not wire-compatible"),
        "PROVENANCE.md must state the divergence in one plain sentence, not only in a \
         table. The table is the evidence; the sentence is the claim a skimming reader \
         actually takes away."
    );
    assert!(
        !md.contains("to stay wire-compatible"),
        "PROVENANCE.md still claims the reimplementation stays wire-compatible. That \
         sentence is the false claim: it fails on `computer_hub_version` vs \
         `hub_version`, before any newer upstream feature is considered."
    );
}
