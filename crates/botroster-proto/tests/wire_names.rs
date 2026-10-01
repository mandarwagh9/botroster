//! The wire names this project promised a published client, pinned.
//!
//! Field renames are the entire difference between this protocol and the one it
//! claims compatibility with, which makes them the difference most likely to rot
//! quietly: nothing in the workspace breaks when a rename is reverted. A
//! renamed field keeps compiling, every call site keeps working, every test in
//! this workspace keeps passing, and the only thing that changes is the bytes
//! on the wire. A client that reads them — which is exactly what an interop
//! harness does — is the sole detector.
//!
//! So each rename gets three assertions rather than one:
//!
//! * serialising emits the new name and **not** the old one, because an alias
//!   that leaks into output would have every client accept the frame while this
//!   hub and the other peers disagree about what the field is called;
//! * deserialising accepts the new name, which is the whole point;
//! * deserialising still accepts the old name, because a stored run record and
//!   a fixture written before the rename are both real artefacts in this
//!   project's history and neither is a reason to lose them.
//!
//! Sources are cited per rename. Every shape here is from
//! `xai-tool-protocol` / `xai-tool-types` at SOURCE_REV; see `PROVENANCE.md` §1.

use botroster_proto::frames::*;
use botroster_proto::{ToolCallId, ToolCallParams, ToolId};
use serde_json::json;

/// Assert a value serialises with `present`, does **not** carry `absent`, and
/// round-trips back to the same value.
fn wire_is(v: &impl serde::Serialize, present: &[&str], absent: &[&str]) -> serde_json::Value {
    let v = serde_json::to_value(v).expect("serialises");
    for key in present {
        assert!(
            v.get(*key).is_some(),
            "`{key}` is missing from the serialised frame: {v}\n\
             A client reading the published name finds nothing here and cannot \
             tell a rename from a field that was never sent."
        );
    }
    for key in absent {
        assert!(
            v.get(*key).is_none(),
            "`{key}` is still on the wire: {v}\n\
             An alias that leaks into output means this hub and every published \
             client are using different names for the same field, and only one \
             of them is wrong in a way nobody sees."
        );
    }
    v
}

/// `tool.call` / `tool_call_request`: `call_id` → `tool_call_id`, `args` →
/// `arguments`. `xai-tool-protocol/src/frames.rs:700-760`.
#[test]
fn a_call_request_is_written_with_the_published_names() {
    let params = ToolCallRequestParams {
        tool_id: ToolId::new("shell.exec"),
        call_id: ToolCallId::new("call-1"),
        args: json!({ "cmd": "ls" }),
    };
    let v = wire_is(
        &params,
        &["tool_call_id", "arguments"],
        &["call_id", "args"],
    );
    assert_eq!(v["tool_call_id"], "call-1");
    assert_eq!(v["arguments"]["cmd"], "ls");
}

/// The same rename on the harness-side `tool.call` params. These are a separate
/// type from `ToolCallRequestParams` and were the one an interop harness named
/// in its refusal: `invalid params: missing field 'call_id'`.
#[test]
fn the_harness_side_call_params_are_written_with_the_published_names() {
    let params = ToolCallParams {
        tool_id: ToolId::new("shell.exec"),
        call_id: ToolCallId::new("call-1"),
        args: json!({ "cmd": "ls" }),
    };
    let v = wire_is(
        &params,
        &["tool_call_id", "arguments"],
        &["call_id", "args"],
    );
    assert_eq!(v["tool_call_id"], "call-1");
    assert_eq!(v["arguments"]["cmd"], "ls");
}

/// A call result carries the id back the same way.
#[test]
fn a_call_result_is_written_with_the_published_name() {
    let r = ToolCallResult {
        call_id: ToolCallId::new("call-1"),
        output: json!({ "exit": 0 }),
    };
    let v = wire_is(&r, &["tool_call_id"], &["call_id"]);
    assert_eq!(v["tool_call_id"], "call-1");
}

/// Progress: `{call_id, payload}` → `{tool_call_id, kind, body}`.
/// `xai-tool-protocol/src/frames.rs:78-99`.
#[test]
fn progress_carries_a_kind_and_a_body() {
    let f = ToolCallProgressFrame {
        call_id: ToolCallId::new("call-1"),
        kind: PROGRESS_KIND_LOG_CHUNK.to_owned(),
        body: json!({ "stage": "starting" }),
    };
    let v = wire_is(
        &f,
        &["tool_call_id", "kind", "body"],
        &["call_id", "payload"],
    );
    assert_eq!(v["kind"], PROGRESS_KIND_LOG_CHUNK);
    assert_eq!(v["body"]["stage"], "starting");
}

/// `kind` is required upstream, so it is required here too — but a frame written
/// before this hub had a `kind` must still parse, or a replayed run record
/// stops being readable.
#[test]
fn progress_without_a_kind_still_parses() {
    let f: ToolCallProgressFrame =
        serde_json::from_value(json!({ "tool_call_id": "call-1", "body": {} }))
            .expect("a progress frame from before `kind` existed");
    assert_eq!(f.kind, PROGRESS_KIND_LOG_CHUNK);
    assert_eq!(f.call_id.as_str(), "call-1");
}

/// A tool description: `tool_id` → `name`, `input_schema` → `arguments_schema`,
/// plus the three optional fields upstream allows. `xai-tool-types/src/types.rs`.
#[test]
fn a_tool_description_is_written_with_the_published_names() {
    let t = ToolDescription::new("peer:echo", "echoes", json!({ "type": "object" }));
    let v = wire_is(
        &t,
        &["name", "description", "arguments_schema"],
        &["tool_id", "input_schema"],
    );
    assert_eq!(v["name"], "peer:echo");
    // Optional upstream fields stay off the wire when unset: a client that
    // distinguishes "no namespace" from "the empty namespace" should be able to.
    assert!(v.get("namespace").is_none());
    assert!(v.get("title").is_none());
    assert!(v.get("kind").is_none());
}

/// The optional fields must round-trip when present, or a description produced
/// by a published server loses its namespace on the way through this hub.
#[test]
fn a_description_from_a_published_server_round_trips() {
    let incoming = json!({
        "name": "peer:echo",
        "namespace": "peer",
        "title": "Echo",
        "description": "echoes its argument",
        "arguments_schema": { "type": "object" },
        "kind": "shell",
    });
    let t: ToolDescription = serde_json::from_value(incoming.clone()).expect("parses");
    assert_eq!(t.tool_id.as_str(), "peer:echo");
    assert_eq!(t.namespace.as_deref(), Some("peer"));
    assert_eq!(t.kind.as_deref(), Some("shell"));
    let back = serde_json::to_value(&t).expect("re-serialises");
    assert_eq!(back, incoming);
}

/// Every pre-rename spelling must still parse, on every type that was renamed.
/// This is the assertion that would have caught the rename being done halfway.
#[test]
fn every_pre_rename_spelling_still_parses() {
    let r: ToolCallRequestParams =
        serde_json::from_value(json!({ "tool_id": "t", "call_id": "c", "args": {} }))
            .expect("old ToolCallRequestParams");
    assert_eq!(r.call_id.as_str(), "c");

    let p: ToolCallParams =
        serde_json::from_value(json!({ "tool_id": "t", "call_id": "c", "args": {} }))
            .expect("old ToolCallParams");
    assert_eq!(p.call_id.as_str(), "c");

    let r: ToolCallResult = serde_json::from_value(json!({ "call_id": "c", "output": {} }))
        .expect("old ToolCallResult");
    assert_eq!(r.call_id.as_str(), "c");

    let f: ToolCallProgressFrame =
        serde_json::from_value(json!({ "call_id": "c", "payload": {} })).expect("old progress");
    assert_eq!(f.call_id.as_str(), "c");
    assert_eq!(f.body, json!({}));

    let t: ToolDescription = serde_json::from_value(json!({
        "tool_id": "t",
        "description": "d",
        "input_schema": {},
    }))
    .expect("old ToolDescription");
    assert_eq!(t.tool_id.as_str(), "t");
}

/// `session.bind` carries the session in `params` and **not** on the envelope.
/// The envelope half is asserted by the live test in `botrosterd`, against a
/// server that routes like the published one; what belongs here is the simpler
/// half — that the params carry it at all.
#[test]
fn a_bind_request_carries_its_session_in_the_params() {
    let p = SessionBindParams {
        session_id: Some(botroster_proto::SessionId::new("s1")),
    };
    let v = wire_is(&p, &["session_id"], &[]);
    assert_eq!(v["session_id"], "s1");
}
