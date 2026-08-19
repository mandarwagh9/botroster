//! The values the window ships with must be the ones that work.
//!
//! The connect panel's fields are pre-filled, and those defaults are what
//! every first Connect uses: the one path taken by somebody who has never
//! used this before and has nothing to compare against when it fails.
//!
//! These tests exist because defaults are exactly the kind of value every
//! other test supplies for itself: the shell tests pass a real hub URL from
//! the harness, and the page tests stub `connect`, so a wrong shipped default
//! (wrong scheme, wrong port) would otherwise go unnoticed.

use std::process::Command;

fn index_html() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("index.html");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// The `value="…"` of one input in the shipped markup.
fn shipped_value(html: &str, id: &str) -> String {
    let anchor = format!(r#"id="{id}""#);
    let at = html
        .find(&anchor)
        .unwrap_or_else(|| panic!("no input with id `{id}` in index.html"));
    // The attribute may sit on either side of the id, and the tag may wrap.
    let open = html[..at].rfind('<').expect("an open tag");
    let close = at + html[at..].find('>').expect("a close bracket");
    let tag = &html[open..close];
    let v = tag
        .find(r#"value=""#)
        .unwrap_or_else(|| panic!("`{id}` has no value attribute: {tag}"));
    let rest = &tag[v + r#"value=""#.len()..];
    rest[..rest.find('"').expect("a closing quote")].to_owned()
}

/// What the binary says its own default is.
fn openbot_default_hub() -> String {
    // Asked of the runtime the window drives, the same way `readme.rs` asks
    // it; a constant copied into a test would agree with itself forever.
    // Never of `openbot-app` itself: that binary opens a window.
    let openbot = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        })
        .join(if cfg!(windows) {
            "openbot.exe"
        } else {
            "openbot"
        });
    let out = Command::new(&openbot)
        .args(["tools", "--help"])
        .env("NO_COLOR", "1")
        .env_remove("OPENBOT_HUB_URL")
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", openbot.display()));
    let text = String::from_utf8_lossy(&out.stdout);
    let at = text
        .find("[default: ")
        .unwrap_or_else(|| panic!("`openbot tools --help` no longer prints a default:\n{text}"));
    let rest = &text[at + "[default: ".len()..];
    rest[..rest.find(']').expect("a closing bracket")]
        .trim()
        .to_owned()
}

/// The window's default hub must be openbot's default hub.
///
/// Not merely "a ws:// URL": opening the window and clicking Connect against
/// a plain `openbot up` must work, which is only true if the two agree exactly.
#[test]
fn the_connect_panel_defaults_to_the_hub_openbot_actually_starts() {
    let shipped = shipped_value(&index_html(), "hub-url");
    let real = openbot_default_hub();
    assert_eq!(
        shipped, real,
        "the window offers `{shipped}` and `openbot up` serves `{real}`, so the \
         first Connect anybody makes will fail"
    );
}

/// A `--hub` that is not a WebSocket URL fails before it reaches the network,
/// which is why the scheme half of the above matters on its own.
#[test]
fn the_shipped_hub_default_is_a_websocket_url() {
    let shipped = shipped_value(&index_html(), "hub-url");
    assert!(
        shipped.starts_with("ws://") || shipped.starts_with("wss://"),
        "`{shipped}` is not a WebSocket URL; openbot answers `URL scheme not \
         supported` without trying"
    );
}

/// No field the window fills may contain a tilde.
///
/// `~` is shell syntax. Nothing expands it on the way to a subprocess, so
/// openbot takes it literally: `openbot bot --home '~/.openbot' ls` succeeds and
/// leaves a directory called `~` in the working directory, holding every Bot
/// the person makes.
///
/// Checked across the markup and the script, because a default may come from
/// either.
#[test]
fn nothing_the_window_hands_to_openbot_contains_a_tilde() {
    let html = index_html();
    for id in ["openbot-path", "home-path", "hub-url"] {
        // A field may legitimately have no `value` at all; what it must not
        // have is a tilde in one.
        let tag_has_value = {
            let at = html.find(&format!(r#"id="{id}""#)).expect("the input");
            let open = html[..at].rfind('<').expect("an open tag");
            let close = at + html[at..].find('>').expect("a close bracket");
            html[open..close].contains(r#"value=""#)
        };
        if tag_has_value {
            let v = shipped_value(&html, id);
            assert!(
                !v.contains('~'),
                "`{id}` ships `{v}`, which openbot would take literally"
            );
        }
    }

    let js = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join("main.js"),
    )
    .expect("main.js");
    // A tilde opening a string literal, rather than anywhere in the file, so
    // prose in comments cannot trip the check.
    for quote in ["\"", "'"] {
        let opener = format!("{quote}~");
        assert!(
            !js.contains(&opener),
            "main.js has a string literal starting with a tilde, which openbot takes literally; search for {opener}"
        );
    }
}

/// Every transcript kind has a rule in the stylesheet that ships with it.
///
/// `Chunk::kind` decides the class the page puts on a line, and the page
/// styles on that class. A kind with no rule is neither a crash nor a visible
/// error: it is a tool call that stops looking like a tool call, or a thought
/// that reads as the Bot speaking. Nothing else would notice.
///
/// A new kind stops the build at `Kind::as_str`; this is the other half,
/// checking the join to the stylesheet that renders it. Reads the shipped
/// `styles.css`: the artefact, not a copy of it.
#[test]
fn every_message_kind_is_styled() {
    let css = include_str!("../ui/styles.css");
    let missing: Vec<&str> = openbot_app::Kind::ALL
        .iter()
        .map(|k| k.as_str())
        .filter(|name| !css.contains(&format!(".msg.{name}")))
        .collect();
    assert!(
        missing.is_empty(),
        "these transcript kinds render as undecorated text, because nothing \
         styles them: {missing:?}"
    );
}

/// The wire names are what the page matches on, so they are pinned here too:
/// renaming a variant is free, renaming its class is not.
#[test]
fn the_wire_name_of_each_kind_is_stable() {
    let names: Vec<&str> = openbot_app::Kind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        names,
        ["agent", "user", "thought", "tool", "progress", "result"],
        "a transcript class changed; `styles.css` and `appendChunk` match on these"
    );
}

/// An option this build cannot classify is presented as a refusal.
///
/// `PermissionOptionKind` is `#[non_exhaustive]`: ACP may add a kind that is
/// neither `allow_*` nor `reject_*`. A prefix match in the page would style
/// such a kind as the permitted choice, in the dialog whose own rule is that
/// allow and deny must not look the same.
///
/// Erring towards danger makes the person read the card again. Erring the
/// other way makes them click through it.
#[test]
fn an_unclassifiable_permission_option_is_styled_as_a_refusal() {
    use agent_client_protocol::schema::v1::PermissionOptionKind as K;
    use openbot_app::refuses;

    assert!(!refuses(K::AllowOnce));
    assert!(!refuses(K::AllowAlways));
    assert!(refuses(K::RejectOnce));
    assert!(refuses(K::RejectAlways));

    // The wildcard cannot be reached from here yet. `#[non_exhaustive]` means
    // a future variant cannot be written in this crate, and the exact `=1.5.0`
    // requirement means the schema cannot grow one underneath; deserialising
    // an invented name fails rather than producing an unknown variant. The
    // arm is a guard for the day the schema is bumped.
    //
    // The assertion that does bite lives where the value is used:
    // `an_unrecognised_option_kind_is_still_styled_as_a_refusal` in `page.rs`
    // feeds the page a kind it has never seen and checks the button.
    assert!(
        serde_json::from_value::<K>(serde_json::json!("cancel")).is_err(),
        concat!(
            "the schema grew a kind; the wildcard in `refuses` is now ",
            "reachable, and this test should assert on it directly"
        )
    );
}

/// Every update the adapter can send is one the window renders.
///
/// A cross-crate join with nothing holding it together: `openbot acp` decides
/// which `SessionUpdate` variants to emit, `render` in this crate decides
/// which to turn into transcript lines, and the two lists live in different
/// crates that never reference each other. An update the adapter sends and
/// the shell does not handle falls into `render`'s wildcard and disappears:
/// no error, no log, just a message that never reaches the window.
///
/// `SessionUpdate` is `#[non_exhaustive]`, so the shell cannot match it
/// totally and the compiler cannot enforce this. Reading both sources is what
/// is left, the same approach `every_message_kind_is_styled` takes: check the
/// artefact rather than a copy of it.
#[test]
fn every_update_the_adapter_sends_is_rendered() {
    fn variants(src: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut rest = src;
        while let Some(at) = rest.find("SessionUpdate::") {
            rest = &rest[at + "SessionUpdate::".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
        out
    }

    let sends = variants(include_str!("../../openbot-cli/src/acp/serve.rs"))
        .union(&variants(include_str!("../../openbot-cli/src/acp/mod.rs")))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let renders = variants(include_str!("../src/lib.rs"));

    assert!(
        !sends.is_empty() && !renders.is_empty(),
        "neither side was read, so this proves nothing: {sends:?} / {renders:?}"
    );
    let unhandled: Vec<_> = sends.difference(&renders).collect();
    assert!(
        unhandled.is_empty(),
        "`openbot acp` sends these and `render` has no arm for them, so they \
         vanish into its wildcard: {unhandled:?}"
    );

    // Having an arm is not the same as producing a line. `Plan` has an arm
    // that returns `None`, so the check above alone would stay green if the
    // adapter started sending `SessionUpdate::Plan` while the window showed
    // nothing.
    //
    // These are the variants whose arm is an intentional drop. The adapter
    // sending one is a message that reaches the client and never reaches the
    // person, which is the failure this test exists for.
    const DROPPED: [&str; 1] = ["Plan"];
    let silent: Vec<_> = DROPPED.iter().filter(|d| sends.contains(**d)).collect();
    assert!(
        silent.is_empty(),
        "`openbot acp` sends these and `render` intentionally drops them, so they \
         never reach the window: {silent:?}"
    );
}

/// A tool that has not finished is not a tool that failed.
///
/// `ToolCallStatus` is `#[non_exhaustive]` and openbot sends `InProgress` on
/// the opening `ToolCall`, so a binary `status == Completed` read would draw
/// a running tool as a failed one. Anything other than `Completed` or
/// `Failed` produces no line at all: a missing result is an absence a person
/// can notice, and a red ✗ is a claim they will believe.
#[test]
fn a_tool_update_that_is_not_a_result_renders_nothing() {
    use agent_client_protocol::schema::v1;
    use openbot_app::render;

    let with = |status| {
        let fields = v1::ToolCallUpdateFields::default().status(status);
        render(
            "s1",
            v1::SessionUpdate::ToolCallUpdate(v1::ToolCallUpdate::new(
                v1::ToolCallId::new("c1"),
                fields,
            )),
        )
    };

    let done = with(v1::ToolCallStatus::Completed).expect("a completed call is a result");
    assert!(done.text.starts_with('✓'), "{}", done.text);
    let failed = with(v1::ToolCallStatus::Failed).expect("a failed call is a result");
    assert!(failed.text.starts_with('✗'), "{}", failed.text);

    // The case a binary read gets wrong.
    assert!(
        with(v1::ToolCallStatus::InProgress).is_none(),
        "a running tool was drawn as a finished one"
    );
    assert!(with(v1::ToolCallStatus::Pending).is_none());
}
