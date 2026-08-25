//! Where the model settings live.
//!
//! Settings live in `$OPENBOT_HOME/config.toml`, flags override them per
//! invocation, and every command that needs a model resolves through one path.
//! Passing `--model --dialect --base-url --api-key-env` on every invocation is
//! unusable in practice: a routine fired by cron has nobody to type flags for
//! it.
//!
//! The API key is never stored in this file. The file names an environment
//! variable to read it from, because a key in a config file ends up in a
//! backup, a screen share, or a repository.

use std::path::{Path, PathBuf};
use std::time::Duration;

use openbot_agent::providers::{Dialect, HttpModel, HttpModelConfig, Scripted};
use openbot_agent::Model;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub model: ModelSettings,
    /// What may run without asking, what must be approved, and what is
    /// refused outright. This is the only way rules reach the policy engine
    /// that `openbot up` enforces.
    #[serde(default)]
    pub permission: PermissionSettings,
}

/// The `[permission]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PermissionSettings {
    /// Rules, in the shape given by `docs/SPEC.md` §12:
    /// `{ action = "allow" | "require_approval" | "deny", tool = "fs.*" }`,
    /// optionally narrowed with `when = { key = "path", glob = "/etc/*" }`
    /// and carrying the `reason` a person is shown.
    ///
    /// Order does not matter. Deny beats require_approval beats allow, so a
    /// permissive rule can never widen a restrictive one: adding an `allow`
    /// can only reduce prompts, never reduce safety.
    #[serde(default)]
    pub rules: Vec<toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSettings {
    /// Model id, e.g. `grok-4-5` or `claude-sonnet-5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Wire dialect: `anthropic`, or `openai` for everything else.
    #[serde(default = "default_dialect")]
    pub dialect: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Environment variable holding the key, never the key itself.
    #[serde(default = "default_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Stop a run after this many tokens in total. `None` is no limit.
    ///
    /// Set once here, every routine inherits it, which matters because a
    /// routine is the run nobody is watching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
}

fn default_dialect() -> String {
    "openai".into()
}
fn default_base_url() -> String {
    "https://api.x.ai/v1".into()
}
fn default_key_env() -> String {
    "XAI_API_KEY".into()
}
fn default_max_tokens() -> u32 {
    8192
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            id: None,
            dialect: default_dialect(),
            base_url: default_base_url(),
            api_key_env: default_key_env(),
            max_tokens: default_max_tokens(),
            token_budget: None,
        }
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

impl ModelOverrides {
    /// The resolved model settings: the file, with flags applied on top.
    ///
    /// Public so `openbot status` can report what a run would actually use,
    /// rather than what the file says.
    pub fn applied(&self, home: &Path) -> ModelSettings {
        self.apply(load(home).unwrap_or_default().model)
    }
}

/// The run-wide token budget, after flags override the file.
///
/// Read separately from [`build`] because it belongs to the agent loop, not to
/// the model connection: the same budget applies whatever provider is behind
/// it, including the scripted one.
pub fn token_budget(home: &Path, o: &ModelOverrides) -> Option<u64> {
    o.apply(load(home).unwrap_or_default().model).token_budget
}

/// The policy `openbot up` should enforce for this home.
///
/// # Errors
/// If a rule cannot be understood. Rules are never silently skipped; see
/// [`rule_from`] for why a dropped field here is a security failure rather
/// than a cosmetic one.
pub fn policy(home: &Path) -> anyhow::Result<openbotd::policy::Policy> {
    let cfg = load(home)?;
    let mut policy = openbotd::policy::Policy::default();
    for (i, raw) in cfg.permission.rules.iter().enumerate() {
        policy
            .rules
            .push(rule_from(raw).map_err(|e| anyhow::anyhow!("[permission] rule {}: {e}", i + 1))?);
    }
    Ok(policy)
}

/// One rule, or a refusal to guess.
///
/// A rule that cannot be understood stops the hub starting. Skipping it and
/// carrying on would be worse: a skipped `deny` leaves a person believing
/// something is forbidden when it is not, with no way to tell.
///
/// `pattern` gets a message of its own. It is the field name in the upstream
/// format this one is compatible with, and it narrows a rule to a command:
/// `{ action = "allow", tool = "bash", pattern = "git *" }`. OPENBOT narrows
/// with `when` instead, on a named argument, because it has no notion of a
/// tool's one principal argument and guessing wrong is not recoverable.
/// Accepting `pattern` and ignoring it would turn "allow git" into "allow
/// everything", the most dangerous thing this loader could do quietly.
fn rule_from(raw: &toml::Value) -> anyhow::Result<openbotd::policy::Rule> {
    if raw.get("pattern").is_some() {
        anyhow::bail!(concat!(
            "`pattern` narrows a rule to a command, and openbot narrows on a named ",
            // Single braces: `bail!` treats a bare literal as a format string,
            // where `{{` is an escape, but a `concat!` argument is not a
            // format string and prints as written.
            "argument instead. Write it as `when = { key = \"command\", ",
            "glob = \"git *\" }`. Ignoring it would turn an allow rule for one ",
            "command into an allow rule for every command."
        ));
    }
    raw.clone()
        .try_into::<openbotd::policy::Rule>()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Read every `[permission]` rule as written, for listing and editing.
///
/// # Errors
/// If the file cannot be read or is not valid TOML.
pub fn rules(home: &Path) -> anyhow::Result<Vec<openbotd::policy::Rule>> {
    load(home)?
        .permission
        .rules
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            rule_from(raw).map_err(|e| anyhow::anyhow!("[permission] rule {}: {e}", i + 1))
        })
        .collect()
}

/// Change the `[permission] rules` array, leaving the rest of the file alone.
///
/// This is a targeted edit of the parsed document, not a round trip through
/// [`Config`]. Saving the typed struct would silently drop any key serde does
/// not know about (a person's `[ui]` table, a setting added by a later
/// version), and a config editor that deletes the parts it does not
/// understand is worse than no config editor.
///
/// Comments and layout are not preserved: TOML is re-emitted from the parsed
/// document. Values and keys all survive, which is the part that changes
/// behaviour.
///
/// # Errors
/// If the file cannot be read, parsed, or written.
pub fn edit_rules(
    home: &Path,
    change: impl FnOnce(&mut Vec<toml::Value>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let p = path(home);
    let text = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(anyhow::anyhow!("could not read {}: {e}", p.display())),
    };
    let mut doc: toml::Table = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not valid config: {e}", p.display()))?;

    let permission = doc
        .entry("permission".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let permission = permission
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[permission] is not a table"))?;
    let mut list = match permission.remove("rules") {
        Some(toml::Value::Array(a)) => a,
        None => Vec::new(),
        Some(_) => anyhow::bail!("[permission] rules is not a list"),
    };
    change(&mut list)?;
    permission.insert("rules".to_owned(), toml::Value::Array(list));

    std::fs::create_dir_all(home)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&doc)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Read the config, or defaults if there is none.
///
/// A missing file is not an error: the defaults are usable and the first run
/// should not require ceremony. A malformed file is an error, because silently
/// ignoring it would route a run to a model the operator did not choose.
pub fn load(home: &Path) -> anyhow::Result<Config> {
    let p = path(home);
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s)
            .map_err(|e| anyhow::anyhow!("{} is not valid config: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(anyhow::anyhow!("could not read {}: {e}", p.display())),
    }
}

pub fn save(home: &Path, c: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(home)?;
    let p = path(home);
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(c)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Per-invocation overrides. Every field is optional: an absent flag means
/// "whatever the config says".
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ModelOverrides {
    /// Model id. Overrides the configured one.
    #[arg(long, env = "OPENBOT_MODEL", global = true)]
    pub model: Option<String>,

    /// Wire dialect: `anthropic` or `openai` (covers xAI, Groq, Ollama, …).
    #[arg(long, env = "OPENBOT_DIALECT", global = true)]
    pub dialect: Option<String>,

    /// API base URL.
    #[arg(long, env = "OPENBOT_BASE_URL", global = true)]
    pub base_url: Option<String>,

    /// Environment variable holding the API key. Never the key itself:
    /// flags land in shell history and process listings.
    #[arg(long, env = "OPENBOT_API_KEY_ENV", global = true)]
    pub api_key_env: Option<String>,

    #[arg(long, global = true)]
    pub max_tokens: Option<u32>,

    /// Stop a run once it has spent this many tokens, in and out together.
    ///
    /// Distinct from `--max-tokens`, which caps a single response. This caps
    /// the whole run, which is what an unattended routine needs: every turn
    /// resends the conversation, so cost grows faster than the step count
    /// suggests.
    #[arg(long, env = "OPENBOT_TOKEN_BUDGET", global = true)]
    pub token_budget: Option<u64>,
}

impl ModelOverrides {
    fn apply(&self, mut s: ModelSettings) -> ModelSettings {
        if let Some(v) = &self.model {
            s.id = Some(v.clone());
        }
        if let Some(v) = &self.dialect {
            s.dialect = v.clone();
        }
        if let Some(v) = &self.base_url {
            s.base_url = v.clone();
        }
        if let Some(v) = &self.api_key_env {
            s.api_key_env = v.clone();
        }
        if let Some(v) = self.token_budget {
            s.token_budget = Some(v);
        }
        if let Some(v) = self.max_tokens {
            s.max_tokens = v;
        }
        s
    }
}

/// The API key for a run: the environment first, then the credential store.
///
/// An empty `key_env` means "this endpoint wants no credential", the normal
/// case for a model served on localhost — Ollama, vLLM, LM Studio. Before it
/// meant anything, every endpoint was assumed to be a paid vendor, and the only
/// way to reach a local model was to invent a variable, set it to junk, and let
/// the header be ignored at the far end. That is a ritual rather than a check,
/// and it taught people to type nonsense into the field that holds credentials.
/// Spelled as an empty variable *name*, because the question is "which variable
/// holds the key" and "none" is a real answer to it.
///
/// The environment is tried first and deliberately wins. A stored key that
/// silently overrode an exported one makes "why is it using the wrong key" a
/// question with no answer visible from the shell — you would have to know the
/// store existed to start looking. This way round, `export` always does what it
/// appears to do, and the store answers only when nothing else did.
///
/// The store is consulted because the desktop window is the only surface a
/// fresh install has, and a key it collected used to live in the spawned
/// agent's environment and nowhere else: correct, and it meant retyping the key
/// at every launch. `secrets.json` is the same owner-only file connector tokens
/// already live in, so this adds a reader rather than a storage mechanism, and
/// `config.toml` still records the name and never the value.
///
/// Separate from [`build`] so the precedence can be asserted directly. Through
/// `build` it is only observable as a working or failing `HttpModel`, which
/// cannot tell "took the environment" from "took the store".
///
/// # Errors
/// If a name was configured and neither source has it. That stays an error:
/// meaning to use a key and forgetting to export it is a mistake worth
/// reporting, and folding it into "no key wanted" would turn every forgotten
/// key into a silent unauthenticated request that comes back 401 with nothing
/// pointing at the cause.
fn resolve_key(home: &Path, key_env: &str) -> anyhow::Result<String> {
    // Trimmed once and then used, rather than trimmed for the emptiness test
    // and looked up raw. A name that arrived with surrounding whitespace — from
    // a paste, or a field in the window — would otherwise pass the "not empty"
    // test and then be looked up under a name no environment can hold, failing
    // with a message quoting a variable that looks exactly right.
    let key_env = key_env.trim();
    if key_env.is_empty() {
        return Ok(String::new());
    }
    // The built-in's credential is a compile-time constant, not a variable, and
    // is answered here so it never has to be put into a process environment for
    // the ordinary lookup to find it. Checked before `std::env::var` on purpose:
    // the sentinel name cannot be a real variable, but answering it first makes
    // that a property of this code rather than of what a shell happens to hold.
    if key_env == BUILTIN_KEY_ENV {
        return builtin_key().map(str::to_owned).ok_or_else(|| {
            anyhow::anyhow!(
                "this build has no built-in model key. A release build bakes one in; a build from source does not. Name a model yourself:  openbot config set --model <id> --api-key-env <VAR>"
            )
        });
    }
    if let Ok(from_env) = std::env::var(key_env) {
        return Ok(from_env);
    }
    // Only reached when a name was configured, so a keyless local model never
    // goes looking for a secret called "".
    openbotd::secrets::SecretStore::open(home)
        .and_then(|store| store.get(key_env))
        .map(|found| found.expose().to_owned())
        .map_err(|_| {
            anyhow::anyhow!(
                "${key_env} is not set, and no key is stored under that name.\n\
                 Export your key there, or store it once:  \
                 openbot secret set {key_env}\n\
                 A model on localhost usually needs none: --api-key-env ''"
            )
        })
}

/// Would a run find its key?
///
/// Exists so `status` answers this the same way a run does. It used to ask
/// `std::env::var` itself, which was one definition of "the key is available"
/// too many the moment a second place to keep one existed: a remembered key
/// worked perfectly and `status` — the command somebody runs precisely to find
/// out whether things are set up — called it missing.
///
/// The value is read and dropped. Checking presence without reading would mean
/// a second lookup path, which is the thing this exists to prevent.
pub fn key_available(home: &Path, key_env: &str) -> bool {
    resolve_key(home, key_env).is_ok()
}

/// Resolve a model for a run.
///
/// `demo` short-circuits to a scripted stand-in so a deployment can be checked
/// without a key. `fallback` is the canned reply that stand-in gives.
/// The model this binary ships with, so a fresh download needs no setup.
///
/// A person who installs OPENBOT should be able to give it work immediately.
/// Before this they met a refusal telling them to go and get an account
/// somewhere; the model below is free of token charges and is already chosen,
/// so there is nothing to pick, nothing to paste and no config file.
///
/// # The key is baked in at build time, not written here
///
/// `option_env!` reads `OPENBOT_BUILTIN_KEY` when this crate is compiled. The
/// key therefore lives in the released binary and **not** in this repository,
/// which is public and Apache-2.0 — a literal here would be readable by anyone
/// browsing GitHub, and would be in the history for good even after it was
/// rotated out.
///
/// That is a mitigation and not a solution, and the difference is worth being
/// exact about: `strings` on any published installer still recovers it. The key
/// is shared by every copy of a given build, works against OpenRouter's paid
/// models as well as the free one, and belongs to whoever cut the release. Plan
/// on rotating it, and treat a build as the unit of rotation.
///
/// A build with no key set falls through to the ordinary "configure a model"
/// path, so a local `cargo build` behaves exactly as it did before.
pub fn builtin() -> Option<ModelSettings> {
    let key = option_env!("OPENBOT_BUILTIN_KEY")?;
    if key.trim().is_empty() {
        return None;
    }
    Some(ModelSettings {
        id: Some(BUILTIN_MODEL.to_owned()),
        dialect: "openai".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        // The name of the variable the run will read, not the value. `build`
        // takes the value from `option_env!` directly, so the key never lands
        // in `config.toml` or in a process environment somebody can dump.
        api_key_env: BUILTIN_KEY_ENV.to_owned(),
        max_tokens: default_max_tokens(),
        token_budget: None,
    })
}

/// What the built-in model is called on OpenRouter.
pub const BUILTIN_MODEL: &str = "stealth/ox-alpha";

/// A name that cannot collide with a real environment variable.
///
/// `resolve_key` looks a key up by variable name. The built-in has no variable —
/// it has a compile-time constant — so it is given a sentinel name that
/// `resolve_key` recognises and answers directly. Using something like
/// `OPENROUTER_API_KEY` would mean an unrelated variable in someone's shell
/// could silently replace the shipped credential.
pub const BUILTIN_KEY_ENV: &str = "<built-in>";

/// The compiled-in credential, if this build has one.
pub fn builtin_key() -> Option<&'static str> {
    option_env!("OPENBOT_BUILTIN_KEY")
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

/// The model id `config.toml` names, if it names one.
///
/// Read separately from [`build`] because "is anything configured" and "give me
/// a working model" are different questions, and only the first one can be
/// asked before deciding whether to go looking for a local one.
pub fn configured_model(home: &Path) -> Option<String> {
    load(home).ok()?.model.id
}

pub fn build(
    home: &Path,
    overrides: &ModelOverrides,
    demo: bool,
    fallback: &str,
) -> anyhow::Result<Arc<dyn Model>> {
    if demo {
        return Ok(Arc::new(Scripted::builder().say(fallback).build()));
    }
    let s = overrides.apply(load(home)?.model);

    // Nothing configured is no longer a refusal: the binary ships with a model.
    // An explicit choice still wins — `--model`, or a `config.toml` — because
    // somebody who named one meant it.
    let s = match (&s.id, builtin()) {
        (None, Some(shipped)) => overrides.apply(shipped),
        _ => s,
    };

    let id = s.id.ok_or_else(|| {
        anyhow::anyhow!(
            // Reached only after `discover` has already looked and found
            // nothing, so it can say so rather than implying configuration is
            // the only route. The local option is first because it is the one
            // that needs no account, and because a person who has just been
            // told "nothing is running here" is one `ollama pull` from done.
            "no model configured, and none is running on this machine.\n\
             The shortest way needs no account and no key: install Ollama, then\n\
             `ollama pull qwen3:1.7b`, and run this again — it will be found.\n\
             Or name one you already pay for:\n\
             \x20   openbot config set --model grok-4-5 --api-key-env XAI_API_KEY\n\
             Or just this once:  --model grok-4-5\n\
             Or check a deployment without one:  --demo"
        )
    })?;
    let key = resolve_key(home, &s.api_key_env)?;

    Ok(Arc::new(HttpModel::new(HttpModelConfig {
        dialect: s
            .dialect
            .parse::<Dialect>()
            .map_err(|e| anyhow::anyhow!(e))?,
        base_url: s.base_url,
        api_key: key,
        model: id,
        max_tokens: s.max_tokens,
        timeout: Duration::from_secs(300),
    })?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_defaults_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let c = load(d.path()).unwrap();
        assert_eq!(c.model.dialect, "openai");
        assert_eq!(c.model.api_key_env, "XAI_API_KEY");
        assert!(c.model.id.is_none());
    }

    #[test]
    fn a_malformed_config_is_an_error_not_a_silent_default() {
        // Ignoring it would route the run to a model nobody chose.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(path(d.path()), "this is not toml {{{").unwrap();
        let e = load(d.path()).unwrap_err().to_string();
        assert!(e.contains("not valid config"), "{e}");
    }

    #[test]
    fn config_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.model.id = Some("grok-4-5".into());
        c.model.max_tokens = 4096;
        save(d.path(), &c).unwrap();
        assert_eq!(load(d.path()).unwrap(), c);
    }

    #[test]
    fn flags_override_the_file_and_absent_flags_do_not() {
        let base = ModelSettings {
            id: Some("from-file".into()),
            dialect: "openai".into(),
            base_url: "https://file.example".into(),
            api_key_env: "FILE_KEY".into(),
            max_tokens: 100,
            token_budget: None,
        };
        let o = ModelOverrides {
            model: Some("from-flag".into()),
            dialect: None,
            base_url: None,
            api_key_env: None,
            max_tokens: Some(200),
            token_budget: Some(50_000),
        };
        let merged = o.apply(base);
        assert_eq!(merged.id.as_deref(), Some("from-flag"));
        assert_eq!(merged.max_tokens, 200);
        // The run-wide budget is a flag like any other, and distinct from the
        // per-response cap above.
        assert_eq!(merged.token_budget, Some(50_000));
        // Untouched fields keep the file's values.
        assert_eq!(merged.base_url, "https://file.example");
        assert_eq!(merged.api_key_env, "FILE_KEY");
    }

    #[test]
    fn the_api_key_itself_is_never_written_to_disk() {
        let d = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.model.id = Some("m".into());
        c.model.api_key_env = "MY_SECRET_ENV".into();
        save(d.path(), &c).unwrap();

        let raw = std::fs::read_to_string(path(d.path())).unwrap();
        // The variable name is fine; a key would end up in backups and
        // screen shares.
        assert!(raw.contains("MY_SECRET_ENV"));
        assert!(!raw.to_lowercase().contains("api_key ="), "{raw}");
    }

    #[test]
    fn demo_needs_no_configuration_at_all() {
        let d = tempfile::tempdir().unwrap();
        let m = build(d.path(), &ModelOverrides::default(), true, "ok").unwrap();
        assert_eq!(m.name(), "scripted");
    }

    #[test]
    fn an_unconfigured_model_says_how_to_configure_it() {
        let d = tempfile::tempdir().unwrap();
        let e = match build(d.path(), &ModelOverrides::default(), false, "") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a model was built without one being configured"),
        };
        assert!(e.contains("openbot config set"), "unhelpful: {e}");
        assert!(e.contains("--demo"), "unhelpful: {e}");
    }

    #[test]
    fn a_missing_key_names_the_variable_it_wanted() {
        let d = tempfile::tempdir().unwrap();
        let o = ModelOverrides {
            model: Some("m".into()),
            api_key_env: Some("DEFINITELY_NOT_SET_12345".into()),
            ..Default::default()
        };
        let e = match build(d.path(), &o, false, "") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a model was built without its key present"),
        };
        assert!(e.contains("DEFINITELY_NOT_SET_12345"), "{e}");
    }

    /// A model on localhost takes no credential, and saying so must be enough
    /// to build one. Before this the only way through was to name a variable
    /// and set it to something meaningless, which taught people to type
    /// nonsense into the field that holds keys.
    #[test]
    fn an_empty_key_variable_means_the_endpoint_wants_no_key() {
        let d = tempfile::tempdir().unwrap();
        let o = ModelOverrides {
            model: Some("qwen3:1.7b".into()),
            base_url: Some("http://localhost:11434/v1".into()),
            api_key_env: Some(String::new()),
            ..Default::default()
        };
        build(d.path(), &o, false, "").expect("a local model with no key variable should build");
    }

    /// The other half of the rule above, and the reason it is spelled as an
    /// empty variable *name*. Meaning to use a key and forgetting to export it
    /// is a mistake, and it has to keep failing loudly — otherwise the change
    /// above would turn every unset key into a silent unauthenticated request
    /// that comes back 401 from the vendor with nothing pointing at the cause.
    #[test]
    fn a_named_but_unset_variable_is_still_an_error() {
        let d = tempfile::tempdir().unwrap();
        let o = ModelOverrides {
            model: Some("m".into()),
            api_key_env: Some("   ALSO_DEFINITELY_NOT_SET_98765".into()),
            ..Default::default()
        };
        let e = match build(d.path(), &o, false, "") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an unset key variable was treated as `no key wanted`"),
        };
        // Padded on purpose. The message has to quote the name that was
        // actually looked up, because a message quoting `$   FOO` sends
        // somebody to check an environment variable they do not have.
        assert!(
            e.contains("$ALSO_DEFINITELY_NOT_SET_98765"),
            "the error does not name the variable it looked up: {e}"
        );
    }

    /// A remembered key is found when the environment has nothing to say.
    ///
    /// This is what makes the window's "keep this key" mean anything: without
    /// it the key reached one spawned process and had to be retyped at the next
    /// launch.
    #[test]
    fn a_remembered_key_is_used_when_the_environment_has_none() {
        const VAR: &str = "STORE_ONLY_KEY_NOTHING_EXPORTS_44821";
        const VALUE: &str = "stored-not-a-real-key";
        assert!(
            std::env::var(VAR).is_err(),
            "{VAR} is set here, so this test would pass without proving anything"
        );

        let d = tempfile::tempdir().unwrap();
        openbotd::secrets::SecretStore::open(d.path())
            .unwrap()
            .set(VAR, openbotd::secrets::Secret::new(VALUE))
            .unwrap();

        assert_eq!(resolve_key(d.path(), VAR).unwrap(), VALUE);
    }

    /// An exported variable beats a remembered one.
    ///
    /// The alternative makes "why is it using the wrong key" unanswerable from
    /// the shell: you would have to know the store existed before you could
    /// start looking. `PATH` is used because it is set everywhere, so the test
    /// never has to mutate the environment — which would race the other tests
    /// in this binary.
    #[test]
    fn an_exported_key_wins_over_a_remembered_one() {
        let d = tempfile::tempdir().unwrap();
        openbotd::secrets::SecretStore::open(d.path())
            .unwrap()
            .set("PATH", openbotd::secrets::Secret::new("the-stored-one"))
            .unwrap();

        let got = resolve_key(d.path(), "PATH").unwrap();
        assert_ne!(
            got, "the-stored-one",
            "the store overrode an exported variable"
        );
        assert_eq!(got, std::env::var("PATH").unwrap());
    }

    /// What `status` reports and what a run does are the same question.
    ///
    /// They were two lookups for as long as there was only one place to keep a
    /// key, and the moment there were two, `status` started calling a working
    /// setup broken — for both a remembered key and a keyless endpoint.
    #[test]
    fn status_counts_a_remembered_key_as_present() {
        const VAR: &str = "STATUS_ONLY_KEY_NOTHING_EXPORTS_31007";
        assert!(std::env::var(VAR).is_err(), "{VAR} is set here");

        let d = tempfile::tempdir().unwrap();
        assert!(
            !key_available(d.path(), VAR),
            "a key was reported before anything stored one"
        );

        openbotd::secrets::SecretStore::open(d.path())
            .unwrap()
            .set(VAR, openbotd::secrets::Secret::new("stored"))
            .unwrap();

        assert!(
            key_available(d.path(), VAR),
            "a remembered key is reported as missing, so `status` contradicts a working run"
        );
        assert!(
            key_available(d.path(), ""),
            "a keyless endpoint is reported as missing a key it never wanted"
        );
    }

    /// A keyless endpoint never consults the store.
    ///
    /// The store is keyed by the variable's name, and an endpoint that wants no
    /// credential has no name — so the lookup would be for a secret called "",
    /// which the store is right to refuse. Returning empty before reaching it
    /// keeps a local model's setup from depending on a credential file at all.
    #[test]
    fn a_keyless_endpoint_does_not_reach_for_the_store() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(resolve_key(d.path(), "   ").unwrap(), "");
        assert!(
            !d.path().join("secrets.json").exists(),
            "a keyless model touched the credential store"
        );
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    fn home_with(toml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), toml).unwrap();
        dir
    }

    /// A permissive rule cannot widen a restrictive one, whichever order they
    /// are written in.
    ///
    /// `openbot permission add` lets a person put both a deny and an allow in
    /// front of the same tool, which is when precedence matters.
    ///
    /// Both orders are checked because they fail differently: `add` appends,
    /// so an allow written later sits after the deny, and the danger is an
    /// engine that takes the last match rather than the strongest. A test
    /// with only that order would also pass on an engine that took the first.
    #[test]
    fn an_allow_cannot_undo_a_deny_whichever_order_they_are_in() {
        let deny_then_allow = "[permission]
rules = [
  { action = \"deny\", tool = \"shell.exec\", reason = \"no shell on this account\" },
  { action = \"allow\", tool = \"shell.exec\" },
]
";
        let allow_then_deny = "[permission]
rules = [
  { action = \"allow\", tool = \"shell.exec\" },
  { action = \"deny\", tool = \"shell.exec\", reason = \"no shell on this account\" },
]
";
        for (order, toml) in [
            ("the allow written after the deny", deny_then_allow),
            ("the allow written before the deny", allow_then_deny),
        ] {
            let home = home_with(toml);
            let policy = policy(home.path()).expect("the policy should load");
            let verdict = format!(
                "{:?}",
                policy.evaluate("shell.exec", &serde_json::json!({ "cmd": "rm -rf /" }))
            );
            assert!(
                verdict.contains("Deny"),
                "{order}: a permissive rule widened a restrictive one: {verdict}"
            );
            assert!(
                verdict.contains("no shell on this account"),
                "{order}: the refusal lost the reason a person wrote: {verdict}"
            );
        }
    }

    /// A rule in the config changes what the hub enforces.
    ///
    /// The engine's precedence is tested elsewhere; this holds the path from
    /// a person's config file to that engine.
    #[test]
    fn a_deny_rule_in_the_config_reaches_the_engine() {
        let home = home_with(
            "[permission]
rules = [{ action = \"deny\", tool = \"fs.write\", reason = \"read-only account\" }]
",
        );
        let policy = policy(home.path()).expect("the policy should load");
        let verdict = format!(
            "{:?}",
            policy.evaluate("fs.write", &serde_json::json!({ "path": "x.md" }))
        );
        assert!(
            verdict.contains("Deny"),
            "a deny rule in the config did not reach the engine: {verdict}"
        );
        assert!(
            verdict.contains("read-only account"),
            "the reason a person wrote did not travel with the refusal: {verdict}"
        );

        // One rule does not disturb everything else.
        let read = format!(
            "{:?}",
            policy.evaluate("fs.read", &serde_json::json!({ "path": "x.md" }))
        );
        assert!(
            read.contains("Allow"),
            "adding one rule changed an unrelated verdict: {read}"
        );
    }

    /// Narrowing on an argument survives the round trip, or `deny fs.write
    /// when path=/etc/*` would forbid every write instead of one directory.
    #[test]
    fn a_rule_can_narrow_on_an_argument() {
        let home = home_with(
            "[permission]
rules = [{ action = \"deny\", tool = \"fs.write\", reason = \"system files\", when = { key = \"path\", glob = \"/etc/*\" } }]
",
        );
        let policy = policy(home.path()).expect("the policy should load");
        let etc = format!(
            "{:?}",
            policy.evaluate("fs.write", &serde_json::json!({ "path": "/etc/passwd" }))
        );
        assert!(
            etc.contains("Deny"),
            "the narrowed rule did not fire: {etc}"
        );
        let mine = format!(
            "{:?}",
            policy.evaluate("fs.write", &serde_json::json!({ "path": "notes.md" }))
        );
        assert!(
            !mine.contains("Deny"),
            "a rule narrowed to /etc/* forbade an unrelated file: {mine}"
        );
    }

    /// `pattern` must be an error, never ignored. It is the upstream format's
    /// way of narrowing a rule to a command; OPENBOT narrows on a named
    /// argument. Accepting the field and dropping it would turn
    /// `allow shell.exec pattern="git *"` into `allow shell.exec`: one command
    /// silently becoming every command.
    #[test]
    fn a_filter_that_cannot_be_honoured_stops_the_hub_rather_than_being_skipped() {
        let home = home_with(
            "[permission]
rules = [{ action = \"allow\", tool = \"shell.exec\", pattern = \"git *\" }]
",
        );
        let err = policy(home.path()).expect_err("a rule with an un-honoured filter must not load");
        let text = format!("{err:#}");
        assert!(
            text.contains("when"),
            "the error should say how to write it: {text}"
        );
        assert!(
            text.contains("every command"),
            "the error should say what ignoring it would cost: {text}"
        );
        assert!(
            text.contains("rule 1"),
            "the error should say which rule: {text}"
        );
    }

    /// The `pattern` refusal shows a rule somebody can paste.
    ///
    /// The message's whole job is handing back the correct form. It is built
    /// with `concat!` to keep this file's indentation out of it, which changes
    /// how `bail!` treats it: a bare literal is a format string where `{{` is
    /// an escape, a `concat!` is not, so doubled braces would be shown as
    /// written.
    #[test]
    fn the_pattern_refusal_hands_back_a_rule_that_can_be_pasted() {
        let home = home_with(
            "[permission]
rules = [{ tool = \"shell.exec\", decision = \"allow\", pattern = \"git *\" }]
",
        );
        let err = policy(home.path()).expect_err("`pattern` must be refused");
        let shown = format!("{err:#}");
        assert!(
            shown.contains(r#"when = { key = "command", glob = "git *" }"#),
            "the replacement rule is not something a person could copy: {shown}"
        );
        assert!(
            !shown.contains("{{"),
            "the braces are doubled, so the rule shown cannot be pasted: {shown}"
        );
    }

    /// `ask` and `require_approval` are the same rule.
    ///
    /// Not a courtesy alias. The product uses both words for this and gives a
    /// person no way to know which surface wants which: `run --approve ask`,
    /// `routine tick --approve ask` and the approval dialog all say ask, and
    /// only the rules file said `require_approval`. The README's own example
    /// said `ask` and was rejected by the parser that reads it, which is how
    /// this was found.
    #[test]
    fn ask_and_require_approval_are_the_same_rule() {
        let with_ask = home_with(
            "[permission]
rules = [{ action = \"ask\", tool = \"shell.exec\" }]
",
        );
        let with_long = home_with(
            "[permission]
rules = [{ action = \"require_approval\", tool = \"shell.exec\" }]
",
        );
        let a = policy(with_ask.path()).expect("`ask` is the word the rest of the product teaches");
        let b = policy(with_long.path()).expect("the canonical spelling still works");
        assert_eq!(
            a.rules, b.rules,
            "the two spellings must produce the same policy"
        );
    }

    /// The name a rule is printed under is a name a rule may be written under.
    ///
    /// `permission ls` used to render `{:?}` lowercased, which turns
    /// `RequireApproval` into "requireapproval" — a word no config may contain
    /// and no error message mentions, printed by the one command people run to
    /// check what they wrote. Anything round-tripping through serde is
    /// necessarily a legal spelling.
    #[test]
    fn every_action_prints_under_a_name_a_config_may_use() {
        use openbotd::policy::Action;
        for action in [Action::Allow, Action::RequireApproval, Action::Deny] {
            let shown = serde_json::to_value(action).unwrap();
            let shown = shown.as_str().expect("an action serialises to a string");
            let home = home_with(&format!(
                "[permission]
rules = [{{ action = \"{shown}\", tool = \"fs.read\" }}]
"
            ));
            let parsed = policy(home.path())
                .unwrap_or_else(|e| panic!("`{shown}` is printed but not accepted: {e}"));
            // The last rule, not the first: `policy` starts from
            // `Policy::default()` and appends the file's rules after it, so
            // index 0 is a shipped default. Indexing 0 made this pass for
            // `allow` by coincidence and fail for the other two, which is how
            // the layout came to light.
            let mine = parsed.rules.last().expect("the file's rule was appended");
            assert_eq!(mine.action, action, "`{shown}` read back as something else");
            assert_eq!(
                mine.tool, "fs.read",
                "the appended rule is the one under test"
            );
        }
    }

    /// A rule that cannot be parsed stops the hub too. Skipping it would
    /// leave a person believing something is forbidden when it is not.
    #[test]
    fn a_malformed_rule_is_refused_rather_than_dropped() {
        let home = home_with(
            "[permission]
rules = [{ action = \"maybe\", tool = \"fs.write\" }]
",
        );
        assert!(
            policy(home.path()).is_err(),
            "an unknown action was accepted"
        );
    }

    /// A home with no config is the shipped default, not an empty policy. An
    /// empty rule list with a permissive fallback would be an open door.
    #[test]
    fn no_config_means_the_shipped_default_not_an_open_door() {
        let home = tempfile::tempdir().unwrap();
        let policy = policy(home.path()).expect("the policy should load");
        let shell = format!(
            "{:?}",
            policy.evaluate("shell.exec", &serde_json::json!({ "command": "rm -rf /" }))
        );
        assert!(
            !shell.contains("Allow"),
            "an unconfigured home ran a shell command without asking: {shell}"
        );
    }
}

#[cfg(test)]
mod builtin_tests {
    use super::*;

    /// No credential is written down in this repository.
    ///
    /// The shipped key is injected at build time from a CI secret, and this is
    /// what keeps it that way. A literal here would be readable by anyone
    /// browsing a public Apache-2.0 repository, and — the part that is easy to
    /// miss — would stay in the git history after it was rotated out, so the
    /// rotation would not actually retire it.
    ///
    /// Scans the crate's own source rather than trusting the author of the next
    /// change to remember.
    #[test]
    fn no_api_key_is_written_into_this_repository() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("readable source dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for (n, line) in text.lines().enumerate() {
                    // The shapes a real provider key takes, assembled from
                    // fragments rather than written whole.
                    //
                    // This looks fussy and is not: the first version spelled
                    // them out and this scan flagged its own pattern list,
                    // which is the scanner working. A checker for a forbidden
                    // string cannot contain that string, or it fails on itself
                    // and the next person deletes the test rather than the key.
                    let prefixes = [
                        format!("sk-{}-v1-", "or"),
                        format!("sk-{}-", "ant"),
                        format!("{}-", "xai"),
                    ];
                    let looks_like_a_key = prefixes.iter().any(|p| line.contains(p.as_str()));
                    // A doc comment naming the shape is not a key.
                    let is_prose = line.trim_start().starts_with("//");
                    if looks_like_a_key && !is_prose {
                        offenders.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a credential appears to be written into the source. It must come from \
             OPENBOT_BUILTIN_KEY at build time instead, or it ships in the repository and \
             survives rotation in the history: {offenders:?}"
        );
    }

    /// A build with no key shipped behaves exactly as it did before.
    ///
    /// A fork, or anyone's local `cargo build`, has no CI secret. That must be
    /// an ordinary OPENBOT that asks you to name a model, not a broken one —
    /// and it is what every test in this workspace runs against.
    #[test]
    fn a_build_without_a_shipped_key_offers_no_builtin() {
        if builtin_key().is_some() {
            // This build has one baked in, so the other assertion is the
            // meaningful one. Said out loud rather than silently skipped.
            eprintln!("this build carries a shipped key; nothing to check here");
            return;
        }
        assert!(
            builtin().is_none(),
            "a build with no credential offered a built-in model anyway, which would fail at \
             the first request with an authentication error instead of asking for a model"
        );
    }

    /// The built-in points at the model it claims to.
    ///
    /// Cheap, and it is the one thing a typo would silently break: a wrong slug
    /// fails at the provider with "model not found", far from here.
    #[test]
    fn the_builtin_names_the_model_and_endpoint_it_should() {
        assert_eq!(BUILTIN_MODEL, "stealth/ox-alpha");
        // The sentinel cannot collide with a real environment variable, which
        // is what stops an unrelated variable in somebody's shell silently
        // replacing the shipped credential.
        assert!(
            BUILTIN_KEY_ENV.contains('<') && BUILTIN_KEY_ENV.contains('>'),
            "the built-in key name must be unusable as a real variable: {BUILTIN_KEY_ENV}"
        );
        assert!(
            std::env::var(BUILTIN_KEY_ENV).is_err(),
            "the sentinel is settable as a real variable, so a shell could override the \
             shipped credential"
        );
    }
}
