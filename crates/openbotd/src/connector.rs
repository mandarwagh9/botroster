//! Connectors: remote MCP servers the hub calls on the guest's behalf.
//!
//! ```text
//! guest ──"call linear__create_issue"──► hub ──► Linear MCP server
//!                                         ▲
//!                            Authorization: Bearer …
//!                            attached here, never sent inward
//! ```
//!
//! The guest asks for a tool by name. The hub resolves the connector, attaches
//! the credential, makes the request, and returns only the result. A guest
//! compromised by prompt injection from a web page it visited can invoke a
//! connector (that is what approvals are for), but it cannot read the token
//! and cannot use it anywhere else.
//!
//! Tools are namespaced `<connector>__<tool>`, matching MCP convention, so two
//! connectors offering `create_issue` do not collide.

use std::sync::Arc;
use std::time::Duration;

use openbot_proto::frames::ToolDescription;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::hub::InternalTools;
use crate::secrets::{interpolate, SecretStore};

/// How long to wait for a remote tool call. Generous: a tool may be doing real
/// work on the other side.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for a connector to describe itself at startup.
///
/// Intentionally shorter than [`CALL_TIMEOUT`]. Discovery is on the path to
/// the hub accepting its first connection, so a dead connector must cost
/// seconds, not minutes. Discovery runs concurrently across connectors, so
/// this is the worst case for any number of them.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The separator between a connector's id and the remote tool name.
pub const NAMESPACE: &str = "__";

/// A configured remote MCP server.
///
/// The credential is not here, only a template naming one, so a connector
/// definition can be listed, logged, and shared without leaking anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connector {
    /// Short name, used as the tool prefix.
    pub id: String,
    /// MCP endpoint.
    pub url: String,
    /// Header value template, e.g. `Bearer ${linear-token}`. Resolved against
    /// the secret store at the moment of the call.
    #[serde(default)]
    pub authorization: String,
}

impl Connector {
    /// Everything that must be true before this definition is written down.
    ///
    /// Called at `add` time and at load, because a file edited by hand is a
    /// normal way for one of these to arrive.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_id(&self.id)?;

        if self.url.is_empty() {
            anyhow::bail!("connector `{}` has no url", self.id);
        }
        let url: url::Url = self
            .url
            .parse()
            .map_err(|e| anyhow::anyhow!("`{}` is not a valid url: {e}", self.url))?;
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!(
                "a connector url must be http or https, not `{}`",
                url.scheme()
            );
        }
        // A credential in the URL would be written into connectors.json in
        // plaintext, echoed back in reqwest's error messages, and logged by the
        // remote's own access log. The Authorization template exists so that
        // cannot happen; refuse the other route rather than documenting it.
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!(
                "connector `{}`: put credentials in --authorization with a ${{secret}} \
                 reference, not in the url — a url is logged by everything it touches",
                self.id
            );
        }
        if self.url.contains("${") {
            anyhow::bail!(
                "connector `{}`: secrets are not substituted into the url, only into \
                 the authorization header",
                self.id
            );
        }

        // The one thing that turns this file into a credential store.
        if !self.authorization.is_empty() && !self.authorization.contains("${") {
            anyhow::bail!(
                "connector `{}`: --authorization must reference a stored secret, e.g. \
                 \"Bearer ${{{}-token}}\". Store the value first with `openbot secret set \
                 {}-token`; a literal token here would be written to disk in plaintext \
                 in a file meant to be shareable.",
                self.id,
                self.id,
                self.id
            );
        }
        if self.authorization.contains(['\n', '\r']) {
            anyhow::bail!(
                "connector `{}`: a header value cannot contain a newline",
                self.id
            );
        }
        Ok(())
    }

    /// The secret names this connector needs, so `add` can check they exist and
    /// `ls` can show what a definition depends on.
    pub fn secret_refs(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = self.authorization.as_str();
        while let Some(start) = rest.find("${") {
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else { break };
            out.push(after[..end].to_owned());
            rest = &after[end + 1..];
        }
        out
    }
}

/// Namespaces the guest and the hub already own.
///
/// Kept in sync with what is served by
/// `every_namespace_served_is_one_a_connector_cannot_claim`, which reads the
/// catalogue over the wire from a running computer.
///
/// A connector id becomes a tool-name prefix, and model vendors reject dots
/// in function names, so `fs.read` goes to a model as `fs__read`, which is
/// exactly what a connector called `fs` offering `read` produces. Two tools
/// with one wire name means a call can reach the wrong system. Refused here,
/// where the person can pick another name, rather than defended everywhere
/// downstream.
pub const RESERVED: &[&str] = &["fs", "shell", "browser", "bot", "openbot", "secret"];

/// A connector id has to survive being used as a tool-name prefix.
fn check_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("a connector id cannot be empty");
    }
    // `split_once("__")` takes the first separator, so an id containing one
    // would make every tool it offers unroutable, a failure that looks like a
    // broken remote from every angle except this one.
    if id.contains(NAMESPACE) {
        anyhow::bail!("a connector id cannot contain `{NAMESPACE}`: `{id}`");
    }
    if id.chars().any(|c| c.is_whitespace()) {
        anyhow::bail!("a connector id cannot contain whitespace: `{id}`");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("a connector id may use letters, digits, `-` and `_`: `{id}`");
    }
    if RESERVED.contains(&id.to_ascii_lowercase().as_str()) {
        anyhow::bail!(
            "`{id}` is reserved: the guest already serves `{id}.*`, and a model sees both \
             as `{id}__<tool>` because vendors do not allow dots in a tool name. Pick \
             another id — `{id}-cloud`, say."
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Connectors {
    #[serde(default)]
    pub connectors: Vec<Connector>,
}

impl Connectors {
    pub fn load(home: &std::path::Path) -> anyhow::Result<Self> {
        let p = home.join("connectors.json");
        let me: Self = match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("{} is not readable: {e}", p.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        // Name the file. `check` explains what is wrong with an id, which is
        // enough while typing one into `connector add`. At boot nobody is
        // typing: a definition that was legal when written can stop being
        // legal when a release reserves a namespace, and the hub then refuses
        // to start with a message about an id and no clue where it lives.
        // There is no `connector rename`, so the way out is named too.
        me.check().map_err(|e| {
            anyhow::anyhow!(
                concat!(
                    "{0}
",
                    "  the definition is in {1}
",
                    "  no rename exists: `openbot connector rm <id>`, then add it again
",
                    "  under a name that is not reserved, or edit that file"
                ),
                e,
                p.display()
            )
        })?;
        Ok(me)
    }

    /// Validate every definition and reject duplicate ids.
    ///
    /// Two connectors sharing an id is the one collision namespacing cannot
    /// protect against: their tools would be indistinguishable in the catalogue
    /// and calls would silently go to whichever was registered first.
    pub fn check(&self) -> anyhow::Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.connectors {
            c.validate()?;
            if !seen.insert(c.id.as_str()) {
                anyhow::bail!(
                    "two connectors are both called `{}`; ids are tool-name prefixes \
                     and have to be unique",
                    c.id
                );
            }
        }
        Ok(())
    }

    pub fn add(&mut self, c: Connector) -> anyhow::Result<()> {
        c.validate()?;
        if self.connectors.iter().any(|x| x.id == c.id) {
            anyhow::bail!("a connector called `{}` already exists", c.id);
        }
        self.connectors.push(c);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> anyhow::Result<Connector> {
        match self.connectors.iter().position(|c| c.id == id) {
            Some(i) => Ok(self.connectors.remove(i)),
            None => anyhow::bail!("no connector called `{id}`"),
        }
    }

    pub fn save(&self, home: &std::path::Path) -> anyhow::Result<()> {
        self.check()?;
        std::fs::create_dir_all(home)?;
        let p = home.join("connectors.json");
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }
}

/// Serves connector tools from the hub.
pub struct ConnectorTools {
    connectors: Vec<Connector>,
    secrets: Arc<SecretStore>,
    client: reqwest::Client,
    /// Catalogue discovered at startup. Discovering per call would make every
    /// call two round trips and let a remote outage look like a missing tool.
    catalog: Vec<ToolDescription>,
}

impl ConnectorTools {
    /// Build, discovering each connector's tools.
    ///
    /// Connectors are probed concurrently and with a short deadline: this runs
    /// before the hub can serve anyone, and one unreachable integration must
    /// not delay every other. A connector that does not answer is logged and
    /// offers no tools, but keeps its namespace (see [`Self::serves`]).
    pub async fn discover(connectors: Vec<Connector>, secrets: Arc<SecretStore>) -> Self {
        // No client-wide timeout: discovery and calls want very different
        // deadlines, so each request sets its own.
        let client = reqwest::Client::new();

        let probes = connectors.iter().map(|c| {
            let client = client.clone();
            let secrets = Arc::clone(&secrets);
            async move {
                let r = tokio::time::timeout(
                    DISCOVERY_TIMEOUT,
                    list_tools(&client, c, &secrets, DISCOVERY_TIMEOUT),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("no answer in {DISCOVERY_TIMEOUT:?}")));
                (c, r)
            }
        });

        let mut catalog = Vec::new();
        for (c, r) in futures_util::future::join_all(probes).await {
            match r {
                Ok(tools) => {
                    tracing::info!(connector = %c.id, count = tools.len(), "connector ready");
                    catalog.extend(tools);
                }
                Err(e) => tracing::warn!(
                    connector = %c.id,
                    error = %e,
                    "connector unreachable; its tools are not offered until it answers"
                ),
            }
        }
        Self {
            connectors,
            secrets,
            client,
            catalog,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
    }

    /// What was discovered, for the operator to see.
    pub fn tool_names(&self) -> Vec<String> {
        self.catalog
            .iter()
            .map(|t| t.tool_id.as_str().to_owned())
            .collect()
    }

    fn split(name: &str) -> Option<(&str, &str)> {
        name.split_once(NAMESPACE)
    }
}

#[async_trait::async_trait]
impl InternalTools for ConnectorTools {
    fn catalog(&self) -> Vec<ToolDescription> {
        self.catalog.clone()
    }

    /// Namespace ownership, not catalogue membership.
    ///
    /// The default implementation asks whether the tool is in `catalog()`,
    /// which would mean a connector that was down at startup disowns its
    /// tools, and the hub, finding nobody to serve them, would route them to
    /// the guest and answer "unknown tool". The operator would go looking for
    /// a typo instead of a dead integration. A configured connector owns
    /// `<id>__*` whether or not it answered discovery.
    fn serves(&self, tool: &str) -> bool {
        match Self::split(tool) {
            Some((prefix, _)) => self.connectors.iter().any(|c| c.id == prefix),
            None => false,
        }
    }

    async fn invoke(
        &self,
        _as_bot: Option<&str>,
        tool: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let (prefix, remote) =
            Self::split(tool).ok_or_else(|| format!("`{tool}` is not a connector tool"))?;
        let c = self
            .connectors
            .iter()
            .find(|c| c.id == prefix)
            .ok_or_else(|| format!("no connector `{prefix}`"))?;

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": remote, "arguments": args },
        });
        // Attempted even if discovery failed: a connector that was down when
        // the hub started may be up now, and refusing on a stale probe would
        // require a restart to recover.
        let v = request(&self.client, c, &self.secrets, &body, CALL_TIMEOUT)
            .await
            .map_err(|e| format!("{prefix} is not answering: {e}"))?;

        if let Some(err) = v.get("error") {
            return Err(format!(
                "{prefix} refused the call: {}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            ));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Ask a connector what it can do.
async fn list_tools(
    client: &reqwest::Client,
    c: &Connector,
    secrets: &SecretStore,
    timeout: Duration,
) -> anyhow::Result<Vec<ToolDescription>> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let v = request(client, c, secrets, &body, timeout).await?;
    let list = v
        .pointer("/result/tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow::anyhow!("no tools in the reply"))?;

    Ok(list
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            Some(ToolDescription::new(
                // Namespaced, so two connectors offering `create_issue` do not
                // collide and the model can tell them apart.
                openbot_proto::ToolId::new(format!("{}{NAMESPACE}{}", c.id, name)),
                t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                t.get("inputSchema")
                    .cloned()
                    .unwrap_or(json!({"type": "object"})),
            ))
        })
        .collect())
}

/// One JSON-RPC round trip, with the credential attached here and nowhere else.
async fn request(
    client: &reqwest::Client,
    c: &Connector,
    secrets: &SecretStore,
    body: &Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let mut req = client.post(&c.url).timeout(timeout).json(body);
    if !c.authorization.is_empty() {
        // Resolved at this instant, used immediately, never stored on the
        // guest side of the wire.
        let header = interpolate(&c.authorization, secrets)?;
        req = req.header("Authorization", header.expose());
    }

    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Scrubbed before truncating. Cutting first can slice a token in
        // half, and half a token is both still a leak and no longer a match
        // for the scrubber.
        anyhow::bail!(
            "HTTP {status}: {}",
            secrets.scrub(&text).chars().take(300).collect::<String>()
        );
    }
    Ok(serde_json::from_str(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Secret;

    fn conn(id: &str) -> Connector {
        Connector {
            id: id.into(),
            url: "https://example.invalid/mcp".into(),
            authorization: "Bearer ${linear-token}".into(),
        }
    }

    #[test]
    fn tool_names_are_namespaced_by_connector() {
        assert_eq!(
            ConnectorTools::split("linear__create_issue"),
            Some(("linear", "create_issue"))
        );
        // A bare name is not a connector tool.
        assert_eq!(ConnectorTools::split("fs.read"), None);
    }

    /// A definition that has become illegal says where it lives.
    ///
    /// `validate` runs at load as well as at `add`, so a connector that was
    /// legal when it was written stops the hub starting once a release
    /// reserves its namespace. At `add` time the person is typing the id and
    /// "pick another" is enough; at boot an id with no file beside it is a
    /// hunt.
    #[test]
    fn a_connector_that_became_illegal_names_its_file_and_the_way_out() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("connectors.json"),
            r#"{"connectors":[{"id":"secret","url":"https://example.invalid/mcp",
                "authorization":"Bearer ${tok}"}]}"#,
        )
        .unwrap();

        let err = Connectors::load(d.path()).expect_err("a reserved id must not load");
        let shown = format!("{err}");
        assert!(shown.contains("reserved"), "the reason was lost: {shown}");
        assert!(
            shown.contains("connectors.json"),
            "the error names an id but not the file holding it: {shown}"
        );
        assert!(
            shown.contains("connector rm"),
            "there is no rename, so the error must name the way out: {shown}"
        );
    }

    #[test]
    fn a_connector_definition_holds_no_credential() {
        let c = conn("linear");
        // Definitions get listed, logged, and shared. Only the name of a
        // secret may appear in one.
        let rendered = format!("{c:?}") + &serde_json::to_string(&c).unwrap();
        assert!(rendered.contains("${linear-token}"));
        assert!(!rendered.contains("sk-"));
    }

    #[test]
    fn a_literal_token_in_the_authorization_header_is_refused() {
        // The one command that could turn a shareable file into a credential
        // store.
        let mut c = conn("linear");
        c.authorization = "Bearer sk-live-0123456789".into();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("openbot secret set"), "unhelpful refusal: {e}");
    }

    #[test]
    fn credentials_in_the_url_are_refused() {
        let mut c = conn("linear");
        c.url = "https://user:hunter2@example.invalid/mcp".into();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("--authorization"), "{e}");

        // And the near-miss where someone assumes interpolation applies here.
        let mut c = conn("linear");
        c.url = "https://example.invalid/mcp?key=${linear-token}".into();
        assert!(c
            .validate()
            .unwrap_err()
            .to_string()
            .contains("not substituted"));
    }

    #[test]
    fn an_id_that_would_collide_with_a_built_in_namespace_is_refused() {
        // `fs.read` and a connector `fs` offering `read` both reach a model as
        // `fs__read`. Which one a call reached would depend on catalogue
        // order: a request to a remote service could silently read a local
        // file instead.
        for reserved in ["fs", "shell", "browser", "bot", "FS", "Browser"] {
            let c = Connector {
                id: reserved.into(),
                ..conn("x")
            };
            let e = c.validate().unwrap_err().to_string();
            assert!(e.contains("reserved"), "`{reserved}` was accepted: {e}");
            assert!(e.contains("-cloud"), "the refusal suggests nothing: {e}");
        }
        // A name that merely starts with one is fine.
        assert!(Connector {
            id: "fs-cloud".into(),
            ..conn("x")
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn an_id_that_would_break_routing_is_refused() {
        for bad in ["", "two__words", "has space", "why?"] {
            let c = Connector {
                id: bad.into(),
                ..conn("x")
            };
            assert!(c.validate().is_err(), "`{bad}` was accepted as an id");
        }
        for good in ["linear", "my-notion", "github_v2", "n8n"] {
            let c = Connector {
                id: good.into(),
                ..conn("x")
            };
            assert!(c.validate().is_ok(), "`{good}` was refused");
        }
    }

    #[test]
    fn a_non_http_url_is_refused() {
        let mut c = conn("linear");
        c.url = "file:///etc/passwd".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn two_connectors_cannot_share_an_id() {
        let mut cs = Connectors::default();
        cs.add(conn("linear")).unwrap();
        let e = cs.add(conn("linear")).unwrap_err().to_string();
        assert!(e.contains("already exists"), "{e}");

        // And the same collision arriving by hand-edited file is caught too.
        let hand_edited = Connectors {
            connectors: vec![conn("linear"), conn("linear")],
        };
        assert!(hand_edited
            .check()
            .unwrap_err()
            .to_string()
            .contains("unique"));
    }

    #[test]
    fn a_definition_names_the_secrets_it_needs() {
        let mut c = conn("linear");
        assert_eq!(c.secret_refs(), vec!["linear-token"]);
        c.authorization = "${a} and ${b}".into();
        assert_eq!(c.secret_refs(), vec!["a", "b"]);
    }

    #[test]
    fn connectors_round_trip_on_disk_without_a_token() {
        let d = tempfile::tempdir().unwrap();
        let secrets = SecretStore::open(d.path()).unwrap();
        secrets
            .set("linear-token", Secret::new("sk-live-secret-value"))
            .unwrap();

        let cs = Connectors {
            connectors: vec![conn("linear")],
        };
        cs.save(d.path()).unwrap();

        let raw = std::fs::read_to_string(d.path().join("connectors.json")).unwrap();
        assert!(
            !raw.contains("sk-live-secret-value"),
            "a token was written into the connector file: {raw}"
        );
        assert_eq!(Connectors::load(d.path()).unwrap().connectors.len(), 1);
    }

    #[test]
    fn a_missing_connector_file_is_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(Connectors::load(d.path()).unwrap().connectors.is_empty());
    }

    #[tokio::test]
    async fn a_connector_owns_its_namespace_even_when_it_never_answered() {
        // Unreachable, so discovery fails and it advertises nothing.
        let d = tempfile::tempdir().unwrap();
        let secrets = Arc::new(SecretStore::open(d.path()).unwrap());
        secrets.set("linear-token", Secret::new("sk-x")).unwrap();
        let tools = ConnectorTools::discover(
            vec![Connector {
                id: "linear".into(),
                url: "http://127.0.0.1:1/mcp".into(),
                authorization: "Bearer ${linear-token}".into(),
            }],
            secrets,
        )
        .await;

        assert!(tools.catalog().is_empty());
        // But the hub still routes its tools here, so the caller is told the
        // integration is down rather than that the tool does not exist.
        assert!(tools.serves("linear__create_issue"));
        assert!(!tools.serves("fs.read"));
        assert!(!tools.serves("bot.send"));
    }
}

#[cfg(test)]
mod upstream_errors {
    use super::*;
    use crate::secrets::Secret;
    use std::io::{Read, Write};

    /// A service that reflects the Authorization header back in its error
    /// body, as some real APIs and proxies do.
    fn echoing_service() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match s.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                // Drain the body first: replying while the client is still
                // writing resets the connection and the test never reaches the
                // response at all.
                let len: usize = text
                    .to_lowercase()
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| {
                        rest.split(
                            "
",
                        )
                        .next()
                    })
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let mut body_in = vec![0u8; len];
                let _ = s.read_exact(&mut body_in);
                let auth = text
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("authorization:"))
                    .unwrap_or("(none)")
                    .to_owned();
                let body = format!(
                    "{{\"error\":\"rejected credential\",\"sent\":\"{}\"}}",
                    auth.replace('"', "")
                );
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://{addr}")
    }

    /// An upstream error does not carry the credential back.
    ///
    /// A credential reaches exactly one place, the `Authorization` header, and
    /// the far side decides what its error body says. Some services and
    /// proxies reflect the header they were sent, and this error is not
    /// private: the hub logs it, and it is returned as the tool call's
    /// failure, which puts it in the model's context and then in
    /// `conversation.jsonl` permanently.
    ///
    /// The service here reflects the header because that is the case to
    /// defend against, not because it is the common one.
    #[tokio::test]
    async fn an_upstream_error_does_not_carry_the_credential_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::open(dir.path()).unwrap();
        store
            .set(
                "linear-token",
                Secret::new(String::from("sk-live-SENTINEL-9f3a7c")),
            )
            .unwrap();
        let c = Connector {
            id: "linear".into(),
            url: echoing_service(),
            authorization: "Bearer ${linear-token}".into(),
        };
        let err = request(
            &reqwest::Client::new(),
            &c,
            &store,
            &serde_json::json!({}),
            Duration::from_secs(5),
        )
        .await
        .expect_err("a 401 is an error");
        let shown = format!("{err}");
        assert!(
            !shown.contains("sk-live-SENTINEL-9f3a7c"),
            "the credential came back inside the upstream error: {shown}"
        );
        // Scrubbed, not merely absent: if the body had failed to arrive at all
        // this would pass while proving nothing.
        assert!(
            shown.contains("rejected credential"),
            "the upstream error itself was lost, so nothing was scrubbed: {shown}"
        );
        assert!(
            shown.contains("${linear-token}"),
            "the credential was removed without saying what stood there: {shown}"
        );
    }
}
