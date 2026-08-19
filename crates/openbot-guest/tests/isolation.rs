//! The guest must have no code path that can read the credential store.
//!
//! This is risk #2 in `docs/SPEC.md`, and the mitigation there is structural:
//! not "reviewers will notice", but "the code to do it cannot be written". The
//! credential store is `openbotd::secrets`, and the guest keeps away from it by
//! not depending on `openbotd` at all. This test is what enforces that.
//!
//! The two model-facing crates are protected differently:
//!
//! * `openbot-agent` is protected by cargo. `openbotd -> openbot-bots ->
//!   openbot-agent` already exists, so an edge back to `openbotd` would be a
//!   dependency cycle and would not compile.
//! * `openbot-guest` is protected by nothing else. `openbotd` does not depend on
//!   the guest, so there is no cycle to prevent it. Adding
//!   `openbotd = { workspace = true }` to `crates/openbot-guest/Cargo.toml`
//!   compiles, passes every other test, and silently ends the guarantee, after
//!   which any tool in the sandbox is one `use` away from the tokens.
//!
//! The test reads the workspace's manifests and walks the internal dependency
//! graph, because the realistic way this breaks is not a direct edge someone
//! would notice in review but a shared helper crate that grows a `openbotd`
//! dependency two hops away.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> has two ancestors")
        .to_path_buf()
}

fn manifest(p: &Path) -> toml::Value {
    let s = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    s.parse().unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every workspace member, as (package name, manifest path).
fn members(root: &Path) -> Vec<(String, PathBuf)> {
    let ws = manifest(&root.join("Cargo.toml"));
    let listed = ws["workspace"]["members"]
        .as_array()
        .expect("workspace.members is a list");
    listed
        .iter()
        .map(|m| {
            let dir = root.join(m.as_str().expect("a member path is a string"));
            let path = dir.join("Cargo.toml");
            let name = manifest(&path)["package"]["name"]
                .as_str()
                .expect("every package is named")
                .to_owned();
            (name, path)
        })
        .collect()
}

/// The workspace crates a package depends on to build and ship.
///
/// `[dev-dependencies]` are intentionally excluded: a test may use anything
/// it likes. The invariant is about the shipped binary.
fn normal_deps(path: &Path, workspace: &BTreeSet<String>) -> BTreeSet<String> {
    let m = manifest(path);
    let mut out = BTreeSet::new();
    // Build-dependencies count: a build script runs in the same process tree
    // and can read anything the crate can.
    for table in ["dependencies", "build-dependencies"] {
        let Some(t) = m.get(table).and_then(|t| t.as_table()) else {
            continue;
        };
        for (key, spec) in t {
            // A dependency can be renamed: `store = { package = "openbotd" }`
            // links openbotd under another name, and checking only the key
            // would let it through.
            let real = spec
                .get("package")
                .and_then(|p| p.as_str())
                .unwrap_or(key.as_str());
            if workspace.contains(real) {
                out.insert(real.to_owned());
            }
        }
    }
    out
}

/// The path by which `from` reaches `to`, if there is one.
fn route(graph: &BTreeMap<String, BTreeSet<String>>, from: &str, to: &str) -> Option<Vec<String>> {
    let mut seen: BTreeSet<&str> = [from].into_iter().collect();
    let mut queue: VecDeque<Vec<String>> = [vec![from.to_owned()]].into();
    while let Some(path) = queue.pop_front() {
        let tail = path.last().expect("a path is never empty");
        if tail == to {
            return Some(path);
        }
        for next in graph.get(tail).into_iter().flatten() {
            if seen.insert(next) {
                let mut longer = path.clone();
                longer.push(next.clone());
                queue.push_back(longer);
            }
        }
    }
    None
}

fn graph() -> BTreeMap<String, BTreeSet<String>> {
    let root = workspace_root();
    let members = members(&root);
    let names: BTreeSet<String> = members.iter().map(|(n, _)| n.clone()).collect();
    members
        .iter()
        .map(|(name, path)| (name.clone(), normal_deps(path, &names)))
        .collect()
}

#[test]
fn the_guest_cannot_reach_the_credential_store() {
    let g = graph();
    // If this trips, the test is reading the wrong manifests rather than
    // proving anything; an empty graph would "pass" every assertion below.
    assert!(
        g.contains_key("openbot-guest") && g.contains_key("openbotd"),
        "did not find the workspace: {:?}",
        g.keys().collect::<Vec<_>>()
    );

    if let Some(path) = route(&g, "openbot-guest", "openbotd") {
        panic!(
            "the guest can now reach the credential store, via {}.\n\
             `openbotd::secrets` holds the tokens in plaintext, and the guest is \
             the side that runs model-chosen tool calls against untrusted \
             pages. Keeping those apart is not a convention; it is the reason \
             a prompt injection cannot exfiltrate a credential. If this \
             dependency is genuinely needed, the thing it needs belongs in a \
             crate that does not carry secrets.",
            path.join(" → ")
        );
    }
}

#[test]
fn the_agent_cannot_reach_the_credential_store_either() {
    let g = graph();
    // Currently true because cargo forbids the cycle, not by explicit choice.
    // Pinned so that if the graph is ever rearranged such that the cycle no
    // longer exists, this fails rather than silently becoming unenforced.
    assert!(
        route(&g, "openbot-agent", "openbotd").is_none(),
        "the agent reaches openbotd; the token is supposed to stay out of \
         anything that talks to a model"
    );
}

#[test]
fn the_walk_actually_follows_indirect_routes() {
    // Guards against a search that only compares direct edges, which would
    // pass forever while a two-hop path to the secrets existed. `openbotd`
    // depends on `openbot-agent` through `openbot-bots` and not directly, so a
    // route of three names proves the search walks indirect routes.
    let g = graph();
    let path = route(&g, "openbotd", "openbot-agent").expect("openbotd reaches openbot-agent");
    assert!(
        path.len() > 2,
        "expected an indirect route, got a direct one: {path:?}; if the graph \
         changed, pick another known-indirect pair rather than deleting this"
    );
    assert!(
        route(&g, "openbot-proto", "openbotd").is_none(),
        "found a phantom route"
    );
}

#[test]
fn the_credential_store_is_where_this_test_thinks_it_is() {
    // The test is worthless if secrets move to a crate the guest already
    // depends on. Pin the location, so that such a move fails here and has to
    // be considered rather than discovered later.
    let root = workspace_root();
    assert!(
        root.join("crates/openbotd/src/secrets.rs").exists(),
        "the credential store moved; this boundary test is now guarding the \
         wrong crate"
    );
    for crate_name in ["openbot-proto", "openbot-store"] {
        let dir = root.join("crates").join(crate_name).join("src");
        assert!(
            !dir.join("secrets.rs").exists(),
            "{crate_name} grew a credential store, and the guest depends on it"
        );
    }
}
