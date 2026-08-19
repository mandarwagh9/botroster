//! Credentials, held by the control plane and never given to the guest.
//!
//! This is the security core of the design, and it rests on one invariant:
//!
//! > A connector's token never leaves this process. The guest asks the hub to
//! > call a tool; the hub makes the outbound request with the credential
//! > attached. The guest, a sandbox running model-directed code that is
//! > reachable by prompt injection from any page it visits, is never given the
//! > token.
//!
//! # Redaction is a type, not a discipline
//!
//! Secrets rarely leak through a deliberate `println!("{token}")`. They leak
//! when a struct holding one derives `Debug` and is later included in an
//! error, a log line, a panic message, or a serialised event. So [`Secret`]
//! refuses to render itself: its `Debug` and `Display` print `[redacted]`,
//! and it has no `Serialize` at all. Getting the bytes out requires calling
//! [`Secret::expose`], which is greppable and rare.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A credential.
///
/// Intentionally not `Serialize`, and its `Debug` and `Display` output is
/// redacted. Everything that could put it in a log is closed by construction
/// rather than by convention.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the bytes. Every call site is a place a secret can escape, so this
    /// name is meant to be conspicuous in review and easy to grep for.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A stable, non-reversible hint for telling two secrets apart in a UI.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(self.0.as_bytes());
        format!("{:x}", d)[..8].to_owned()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no secret named `{0}`")]
    NotFound(String),
    #[error("the secret store is unreadable: {0}")]
    Corrupt(String),
    #[error("a secret name must be non-empty and contain no path separators")]
    BadName,
    #[error("a secret cannot be empty")]
    Empty,
    #[error(
        "this value contains a control character, which cannot go in an HTTP header \
         (if it came from a file, it may have a stray newline in the middle)"
    )]
    NotHeaderSafe,
}

pub type Result<T> = std::result::Result<T, SecretError>;

/// Secrets on disk, keyed by name.
///
/// Stored in plaintext with restrictive permissions, the same posture as
/// `~/.aws/credentials` or a `.env` file. This is documented in the README.
/// An OS keychain backend is the intended next step.
pub struct SecretStore {
    path: PathBuf,
}

impl SecretStore {
    pub fn open(home: &Path) -> Result<Self> {
        fs::create_dir_all(home)?;
        // `create_dir_all` asks for 0777 and lets the umask decide, which on a
        // stock Linux (umask 022) and on a CI runner means 0755: world
        // readable and world traversable. A 0600 file inside it is still
        // unreadable, but the directory is what makes the temporary file below
        // reachable at all, and there is nothing in an OPENBOT home another
        // user has any business listing.
        //
        // Done on every open, not only on create, so a home that predates this
        // is repaired rather than left as it was found.
        restrict_dir(home)?;
        Ok(Self {
            path: home.join("secrets.json"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<BTreeMap<String, String>> {
        match fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| SecretError::Corrupt(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, m: &BTreeMap<String, String>) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        // Created 0600, rather than created 0644 and tightened afterwards.
        // `fs::write` followed by a chmod puts every token in the store on
        // disk world-readable for the width of two syscalls. Asking for the
        // mode at creation closes that window.
        {
            use std::io::Write as _;
            let mut f = create_private(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(m).expect("map serialises"))?;
        }
        // The mode above applies only when this call is the one that creates
        // the file. A tmp left behind by a crashed write is reused with the
        // permissions it already had, so tighten it explicitly too.
        restrict(&tmp)?;
        fs::rename(&tmp, &self.path)?;
        // Set it again on the destination: a rename over an existing file
        // keeps that file's inode on some platforms, and its permissions with
        // it.
        restrict(&self.path)?;
        Ok(())
    }

    /// Store a secret.
    ///
    /// The value is checked here rather than at the point of use, because the
    /// point of use is an outbound HTTP header two subsystems away, where a
    /// stray byte surfaces as an unexplained connection failure. `echo $TOKEN |
    /// openbot secret set ...` appends a newline; that is the common case, so it
    /// is trimmed rather than rejected. A control character inside the value is
    /// refused: it cannot be intended and cannot be sent.
    pub fn set(&self, name: &str, value: Secret) -> Result<()> {
        let name = check_name(name)?;
        let trimmed = value.expose().trim_matches(|c| c == '\n' || c == '\r');
        if trimmed.is_empty() {
            return Err(SecretError::Empty);
        }
        if trimmed.chars().any(|c| c.is_control()) {
            return Err(SecretError::NotHeaderSafe);
        }
        let mut m = self.read()?;
        m.insert(name, trimmed.to_owned());
        self.write(&m)
    }

    pub fn get(&self, name: &str) -> Result<Secret> {
        self.read()?
            .get(name)
            .map(|v| Secret::new(v.clone()))
            .ok_or_else(|| SecretError::NotFound(name.to_owned()))
    }

    /// Names only. There is no API that returns every value at once, because
    /// the only reason to want one is to print them.
    pub fn names(&self) -> Result<Vec<String>> {
        Ok(self.read()?.keys().cloned().collect())
    }

    pub fn fingerprint(&self, name: &str) -> Result<String> {
        Ok(self.get(name)?.fingerprint())
    }

    /// Replace any stored credential appearing in `text` with `${its-name}`.
    ///
    /// For text that is about to leave this process. A credential reaches a
    /// header and nowhere else, but the far side decides what its error says,
    /// and some services and proxies reflect the `Authorization` they were
    /// sent. A 401 body holding the token would be logged by the hub and
    /// returned as the tool call's failure, which puts it in front of the
    /// model and then in `conversation.jsonl`. The broker exists so that a Bot
    /// uses a credential and never reads one; an upstream error must not be
    /// the way around that.
    ///
    /// Short values are left alone. An exact match on eight or more
    /// characters is a token; a match on three could be a word, and mangling
    /// an error message to protect a credential that short is a bad trade. A
    /// credential that short cannot be defended here.
    ///
    /// Reads the store on each call. This runs on an error path, never in a
    /// loop, and a cached copy of every secret held in memory for the life of
    /// the process is what this module exists to avoid.
    pub fn scrub(&self, text: &str) -> String {
        const SHORTEST: usize = 8;
        let mut out = text.to_owned();
        for name in self.names().unwrap_or_default() {
            let Ok(secret) = self.get(&name) else {
                continue;
            };
            let value = secret.expose();
            if value.len() < SHORTEST || !out.contains(value) {
                continue;
            }
            out = out.replace(value, &format!("${{{name}}}"));
        }
        out
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let mut m = self.read()?;
        if m.remove(name).is_none() {
            return Err(SecretError::NotFound(name.to_owned()));
        }
        self.write(&m)
    }
}

fn check_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.contains('/') || n.contains('\\') || n.contains("..") {
        return Err(SecretError::BadName);
    }
    Ok(n.to_owned())
}

/// Make a file readable only by its owner.
#[cfg(unix)]
fn restrict(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Make a directory usable only by its owner.
#[cfg(unix)]
fn restrict_dir(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Create a file that is private from the moment it exists.
#[cfg(unix)]
fn create_private(p: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)?)
}

/// Windows has no mode bits, so this does nothing. On Windows `secrets.json`
/// carries no permissions of its own: every ACL entry is inherited, and the
/// parent directory is the entire protection.
///
/// Under a home in the user's profile that is adequate (SYSTEM, Administrators
/// and the user, nobody else). Under a home in a shared location such as
/// `C:\Users\Public` it is not: `NT AUTHORITY\INTERACTIVE` inherits Modify,
/// so every interactively logged-on user can read and replace a stored
/// credential. `--home` accepts any path, and this precondition is not
/// enforced, for three reasons:
///
/// * Warning when the home is outside `%USERPROFILE%` fires on an ordinary
///   `D:\openbot` and stays quiet on a widened directory inside the profile; a
///   check that is loudest where it is least needed gets ignored.
/// * Reading the ACL by shelling to `icacls` is a security decision made by
///   parsing localised output.
/// * Evaluating access properly means `AuthzAccessCheck` and the `windows`
///   crate: a real dependency, for one check, on one platform.
///
/// On Windows this file is as private as the directory holding it, and
/// nothing here checks that directory.
#[cfg(not(unix))]
fn restrict(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dir(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn create_private(p: &Path) -> Result<fs::File> {
    Ok(fs::File::create(p)?)
}

/// Substitute `${name}` references from the store.
///
/// Used for connector headers: a connector is configured as
/// `Authorization: Bearer ${linear-token}` and the token is only resolved at
/// the moment of the outbound call, inside this process.
pub fn interpolate(template: &str, store: &SecretStore) -> Result<Secret> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // An unterminated reference is literal text, not a silent
            // truncation of the header.
            out.push_str(&rest[start..]);
            return Ok(Secret::new(out));
        };
        let name = &after[..end];
        out.push_str(store.get(name)?.expose());
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(Secret::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, SecretStore) {
        let d = tempfile::tempdir().unwrap();
        let s = SecretStore::open(d.path()).unwrap();
        (d, s)
    }

    const TOKEN: &str = "sk-live-do-not-leak-me-0123456789";

    /// `scrub` takes a credential out of text that is about to leave.
    ///
    /// The name is put in its place rather than a row of asterisks: an error
    /// that says `${linear-token}` tells the reader which credential the far
    /// side rejected, which a blanket redaction would throw away.
    #[test]
    fn scrub_replaces_a_stored_value_with_its_name() {
        let (_d, s) = store();
        s.set("linear-token", Secret::new("sk-live-abcdef123456"))
            .unwrap();

        let out = s.scrub(r#"{"error":"bad","sent":"Bearer sk-live-abcdef123456"}"#);
        assert!(!out.contains("sk-live-abcdef123456"), "{out}");
        assert!(out.contains("${linear-token}"), "{out}");
        assert!(
            out.contains(r#""error":"bad""#),
            "the rest of the text was lost: {out}"
        );
    }

    #[test]
    fn scrub_replaces_every_occurrence() {
        let (_d, s) = store();
        s.set("tok", Secret::new("sk-live-abcdef123456")).unwrap();
        let out = s.scrub("sk-live-abcdef123456 and again sk-live-abcdef123456");
        assert!(!out.contains("sk-live-abcdef123456"), "{out}");
        assert_eq!(out.matches("${tok}").count(), 2, "{out}");
    }

    /// A short value is intentionally left alone.
    ///
    /// An exact match on eight characters or more is a token. A match on three
    /// is a word, and rewriting every error that happens to contain it would
    /// do more harm than good. Asserted so the threshold is a decision rather
    /// than a number that can be quietly lowered until messages come apart.
    #[test]
    fn scrub_leaves_a_short_value_alone() {
        let (_d, s) = store();
        s.set("pin", Secret::new("1234")).unwrap();
        let text = "the file 1234.txt could not be read";
        assert_eq!(s.scrub(text), text);
    }

    #[test]
    fn scrub_leaves_text_holding_no_secret_untouched() {
        let (_d, s) = store();
        s.set("tok", Secret::new("sk-live-abcdef123456")).unwrap();
        let text = "HTTP 500: the upstream fell over";
        assert_eq!(s.scrub(text), text);
    }

    #[test]
    fn debug_and_display_never_show_the_value() {
        let s = Secret::new(TOKEN);
        // The common leak: a struct derives Debug and ends up in an error, a
        // log line, or a panic message.
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert_eq!(format!("{s}"), "[redacted]");
        assert!(!format!("{s:?} {s}").contains("sk-live"));

        // And when nested in something else that derives Debug.
        #[derive(Debug)]
        #[allow(dead_code)] // the point is what Debug prints, not reading it
        struct Holder {
            token: Secret,
        }
        let h = Holder {
            token: Secret::new(TOKEN),
        };
        let rendered = format!("{h:?}");
        assert!(!rendered.contains("sk-live"), "{rendered}");
        assert!(rendered.contains("[redacted]"));
    }

    /// A secret must not be serialisable, so a struct holding one cannot be
    /// turned into an event, a transcript, or an HTTP body by accident.
    ///
    /// The guarantee is the absence of an impl, which an ordinary test cannot
    /// assert directly (an unbounded generic function compiles the same
    /// whether or not the impl exists). This uses autoref specialisation:
    /// method resolution prefers a by-value receiver over one reached by
    /// autoref, so `Probe<T>` answers `true` only when `T: Serialize` makes
    /// that impl apply, and falls through to the reference impl otherwise.
    /// Adding `impl Serialize for Secret` flips it and fails here.
    #[test]
    fn a_secret_cannot_be_serialised_at_all() {
        use std::marker::PhantomData;

        struct Probe<T>(PhantomData<T>);

        trait Serialisable {
            fn answer(&self) -> bool {
                true
            }
        }
        impl<T: serde::Serialize> Serialisable for Probe<T> {}

        trait NotSerialisable {
            fn answer(&self) -> bool {
                false
            }
        }
        impl<T> NotSerialisable for &Probe<T> {}

        // Both calls are written identically: the borrow is what makes the
        // fallback reachable when no by-value impl applies, and the only
        // difference between the two lines should be the type.
        #[allow(clippy::needless_borrow)]
        let secret_is_serialisable = (&Probe::<Secret>(PhantomData)).answer();
        #[allow(clippy::needless_borrow)]
        let string_is_serialisable = (&Probe::<String>(PhantomData)).answer();

        assert!(
            !secret_is_serialisable,
            "`Secret` implements Serialize; a struct holding one can be written into an event, a transcript, or an HTTP body"
        );

        // The probe itself must be able to say yes, or the assertion above is
        // satisfied by a mechanism that never says anything else.
        assert!(
            string_is_serialisable,
            "the probe cannot detect Serialize at all, so it proves nothing"
        );
    }

    #[test]
    fn getting_the_value_requires_saying_so() {
        let s = Secret::new(TOKEN);
        // Conspicuous and greppable.
        assert_eq!(s.expose(), TOKEN);
    }

    #[test]
    fn a_fingerprint_identifies_without_revealing() {
        let a = Secret::new(TOKEN);
        let b = Secret::new(TOKEN);
        let c = Secret::new("something else");
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
        assert_eq!(a.fingerprint().len(), 8);
        assert!(!TOKEN.contains(&a.fingerprint()));
    }

    #[test]
    fn secrets_round_trip_through_the_store() {
        let (_d, s) = store();
        s.set("linear-token", Secret::new(TOKEN)).unwrap();
        assert_eq!(s.get("linear-token").unwrap().expose(), TOKEN);
        assert_eq!(s.names().unwrap(), vec!["linear-token"]);
    }

    #[test]
    fn a_trailing_newline_is_trimmed_rather_than_stored() {
        let (_d, s) = store();
        // `echo $TOKEN | openbot secret set ...` is how most of these arrive. The
        // newline would end up inside an HTTP header value and surface as a
        // connection error a long way from its cause.
        s.set("t", Secret::new(format!("{TOKEN}\n"))).unwrap();
        assert_eq!(s.get("t").unwrap().expose(), TOKEN);
        s.set("t", Secret::new(format!("{TOKEN}\r\n"))).unwrap();
        assert_eq!(s.get("t").unwrap().expose(), TOKEN);
    }

    #[test]
    fn a_control_character_inside_the_value_is_refused() {
        let (_d, s) = store();
        assert!(matches!(
            s.set("t", Secret::new("abc\ndef")),
            Err(SecretError::NotHeaderSafe)
        ));
        assert!(matches!(
            s.set("t", Secret::new("abc\0def")),
            Err(SecretError::NotHeaderSafe)
        ));
    }

    #[test]
    fn an_empty_value_is_refused_rather_than_stored_as_a_working_credential() {
        let (_d, s) = store();
        // Storing "" would produce `Bearer ` at the call, and a 401 that looks
        // like a revoked token rather than an empty one.
        assert!(matches!(
            s.set("t", Secret::new("")),
            Err(SecretError::Empty)
        ));
        assert!(matches!(
            s.set("t", Secret::new("\n")),
            Err(SecretError::Empty)
        ));
    }

    #[test]
    fn a_missing_secret_is_named_in_the_error_but_no_value_is() {
        let (_d, s) = store();
        let e = s.get("nope").unwrap_err().to_string();
        assert!(e.contains("nope"));
        assert!(!e.contains("sk-"));
    }

    #[test]
    fn a_name_cannot_be_a_path() {
        let (_d, s) = store();
        for bad in ["", "  ", "../escape", "a/b", "a\\b"] {
            assert!(
                matches!(s.set(bad, Secret::new("x")), Err(SecretError::BadName)),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn removing_is_explicit_about_a_name_that_was_not_there() {
        let (_d, s) = store();
        s.set("a", Secret::new("x")).unwrap();
        s.remove("a").unwrap();
        assert!(matches!(s.remove("a"), Err(SecretError::NotFound(_))));
    }

    #[test]
    fn interpolation_resolves_only_at_the_moment_of_use() {
        let (_d, s) = store();
        s.set("linear-token", Secret::new(TOKEN)).unwrap();
        let header = interpolate("Bearer ${linear-token}", &s).unwrap();
        assert_eq!(header.expose(), format!("Bearer {TOKEN}"));
        // The result is itself a Secret, so it cannot be logged on its way out.
        assert_eq!(format!("{header:?}"), "[redacted]");
    }

    #[test]
    fn interpolating_a_missing_secret_fails_rather_than_sending_an_empty_header() {
        let (_d, s) = store();
        // Substituting nothing would send `Bearer ` and produce a confusing
        // 401 instead of a clear configuration error.
        assert!(matches!(
            interpolate("Bearer ${absent}", &s),
            Err(SecretError::NotFound(_))
        ));
    }

    #[test]
    fn an_unterminated_reference_stays_literal() {
        let (_d, s) = store();
        let out = interpolate("Bearer ${oops", &s).unwrap();
        assert_eq!(out.expose(), "Bearer ${oops");
    }

    #[test]
    fn several_references_in_one_template() {
        let (_d, s) = store();
        s.set("a", Secret::new("AAA")).unwrap();
        s.set("b", Secret::new("BBB")).unwrap();
        assert_eq!(
            interpolate("${a}-middle-${b}", &s).unwrap().expose(),
            "AAA-middle-BBB"
        );
    }

    #[test]
    fn there_is_no_api_that_dumps_every_value() {
        let (_d, s) = store();
        s.set("a", Secret::new(TOKEN)).unwrap();
        // `names()` exists; a `values()` intentionally does not, because the
        // only reason to want one is to print them.
        let listed = s.names().unwrap();
        assert_eq!(listed, vec!["a"]);
        assert!(!format!("{listed:?}").contains("sk-live"));
    }

    #[cfg(unix)]
    mod permissions {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn mode(p: &std::path::Path) -> u32 {
            fs::metadata(p).unwrap().permissions().mode() & 0o777
        }

        /// Pins the end state regardless of how it is reached.
        #[test]
        fn a_secret_file_is_readable_only_by_its_owner() {
            let (_d, s) = store();
            s.set("a", Secret::new(TOKEN)).unwrap();
            assert_eq!(mode(s.path()), 0o600, "secrets.json is not private");
        }

        /// `create_dir_all` asks for 0777 and the umask trims it to 0755, so
        /// without `restrict_dir` the home is world-traversable, which is what
        /// makes the temporary file reachable at all.
        #[test]
        fn the_home_directory_is_not_open_to_other_users() {
            let d = tempfile::tempdir().unwrap();
            let home = d.path().join("openbot-home");
            let _s = SecretStore::open(&home).unwrap();
            assert_eq!(mode(&home), 0o700, "the openbot home is not private");
        }

        /// `create_private` has to hand back a file that is already private;
        /// a chmod afterwards leaves a window.
        #[test]
        fn a_private_file_is_private_from_the_moment_it_exists() {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("fresh");
            let f = create_private(&p).unwrap();
            // Checked before anything else touches it, and before the handle
            // is dropped: no `restrict` has run at this point.
            assert_eq!(mode(&p), 0o600, "created world-readable");
            drop(f);
        }

        /// A tmp left behind by a crashed write is reused rather than
        /// recreated, and `.mode()` applies only to creation, so the explicit
        /// `restrict` has to hold on its own.
        #[test]
        fn a_stale_temporary_file_does_not_keep_its_old_permissions() {
            let (_d, s) = store();
            let tmp = s.path().with_extension("json.tmp");
            fs::write(&tmp, b"{}").unwrap();
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();
            s.set("a", Secret::new(TOKEN)).unwrap();
            assert_eq!(mode(s.path()), 0o600, "inherited a stale tmp's mode");
        }
    }
}
