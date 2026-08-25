//! `botroster status`: one screen that answers "why is nothing happening?"
//!
//! Everything here is also available from `servers`, `tools`, `config show`
//! and `routine ls`. Someone whose nightly digest did not arrive should not
//! have to know that list.
//!
//! Each line is a question people ask, and the ones that answer "no" are the
//! point:
//!
//! * is the hub even up
//! * is a computer attached to it
//! * is a model configured, and is its key actually in the environment
//! * is anything due that has not run
//! * has a routine paused itself because nobody has looked in a fortnight
//!
//! It works when the hub is down, because that is exactly when it is needed.

use std::path::Path;

use crate::render::Style;

pub struct Status {
    /// Whether `config.toml` parses and its rules are all understood.
    ///
    /// Checked with the same call `botroster up` makes, so this screen and the
    /// thing it reports on cannot disagree. They did: `applied` reads the file
    /// with `load(home).unwrap_or_default()`, so a file that will not parse
    /// silently became the defaults, and a command whose one-line help is "Is
    /// anything wrong?" answered by listing a model nobody had configured.
    pub config: Result<(), String>,
    pub hub_url: String,
    pub hub: Result<Vec<String>, String>,
    pub model: Option<String>,
    pub key_env: String,
    pub key_present: bool,
    pub bots: usize,
    pub routines_enabled: usize,
    pub routines_paused: usize,
    pub due_now: Vec<String>,
    /// Routines whose most recent run failed, with what it said.
    ///
    /// "2 enabled" is equally true of a routine that has thrown the same
    /// error every night for a week, and a run history nobody opens is the
    /// same as no run history. The failure has to be surfaced here.
    pub failing: Vec<(String, String)>,
    /// How many snapshots the computer has, and how long since the newest.
    ///
    /// A schedule that has quietly stopped looks exactly like one that is
    /// working, right up until the rollback.
    pub snapshots: Option<(usize, chrono::Duration)>,
    /// Whether a guest currently holds the durable volume these snapshots
    /// belong to.
    ///
    /// A computer can be running and not be on this volume: `botroster up
    /// --workspace <dir>` puts the guest on a plain directory, and nothing
    /// snapshots that. Reporting "snapshots 1, newest just now" for a volume
    /// the live computer is not touching would misstate the one claim a
    /// person checks before trusting a rollback.
    ///
    /// Read from the volume's attach lock, which asks the OS who holds it
    /// rather than believing a file: the same check `botroster computer status`
    /// prints as "attached".
    pub volume_attached: bool,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
    pub idle_days: i64,
}

/// Gather everything, tolerating each part being unavailable.
pub async fn gather(hub_url: &str, home: &Path, opts: &crate::config::ModelOverrides) -> Status {
    // Best-effort, with a short deadline: a status command that hangs because
    // the thing it is reporting on is down is worse than useless.
    let hub = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        botroster_agent::HubClient::connect(hub_url),
    )
    .await
    {
        Ok(Ok((client, _))) => match client.list_servers().await {
            Ok(list) => Ok(list
                .into_iter()
                .map(|s| s.server_id.as_str().to_owned())
                .collect()),
            Err(e) => Err(e.to_string()),
        },
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("no answer in 3s".into()),
    };

    // Before anything else, because every line below is read through it: the
    // rules govern what a Bot may do, and a file that does not parse is not a
    // conservative fallback but a different policy from the one on disk.
    let config = crate::config::policy(home)
        .map(|_| ())
        .map_err(|e| e.to_string());

    let settings = opts.applied(home);
    let key_env = settings.api_key_env.trim().to_owned();
    // Asked the way a run asks, rather than re-implemented here. Two things
    // count as having a key that `std::env::var` alone does not see: an empty
    // variable name, which is a configured answer meaning "this endpoint wants
    // no credential", and a key kept in the credential store. Either way a
    // `status` that disagreed with a working agent would send somebody to fix
    // something that is not broken — which is the opposite of what this screen
    // is for.
    let key_present = crate::config::key_available(home, &key_env);

    let now = chrono::Utc::now();
    let (mut bots, mut enabled, mut paused, mut due, mut last_seen) = (0, 0, 0, Vec::new(), None);
    let mut failing: Vec<(String, String)> = Vec::new();
    if let Ok(store) = botroster_bots::BotStore::open(home) {
        bots = store.list(true).map(|b| b.len()).unwrap_or(0);
        if let Ok(rs) = store.all_routines() {
            for r in &rs {
                if r.enabled {
                    enabled += 1;
                } else {
                    paused += 1;
                }
                // The latest run, not any failure ever: one that failed
                // yesterday and succeeded today is working, and reporting it
                // is how a warning becomes something people scroll past. A
                // retryable run is not a failure either: the computer was
                // busy and the routine will be retried.
                if let Some(last) = r.runs.last() {
                    if !last.ok && !last.retryable {
                        failing.push((format!("{}/{}", r.bot, r.id), last.summary.clone()));
                    }
                }
            }
        }
        due = store
            .due(now)
            .unwrap_or_default()
            .into_iter()
            .map(|r| format!("{}/{}", r.bot, r.id))
            .collect();
        last_seen = store.idle_since();
    }

    Status {
        config,
        hub_url: hub_url.to_owned(),
        hub,
        model: settings.id.clone(),
        key_env,
        key_present,
        bots,
        routines_enabled: enabled,
        routines_paused: paused,
        due_now: due,
        failing,
        snapshots: newest_snapshot(home),
        volume_attached: volume_attached(home),
        last_seen,
        idle_days: 14,
    }
}

/// How many snapshots the computer has, and the age of the newest.
///
/// Read from the store rather than from any record the schedule keeps: what
/// matters is whether snapshots exist and are recent, which is true however
/// they got there and false however convinced the schedule is that it is
/// running.
fn newest_snapshot(home: &Path) -> Option<(usize, chrono::Duration)> {
    let store = botroster_store::Store::open(home).ok()?;
    let id = store.volumes().ok()?.into_iter().next()?;
    let volume = store.volume(&id).ok()?;
    let snaps = volume.snapshots().ok()?;
    let newest = snaps.last()?;
    let taken = volume.taken_at(&newest.id)?;
    let age = chrono::Utc::now() - chrono::DateTime::<chrono::Utc>::from(taken);
    Some((snaps.len(), age))
}

/// Is a guest holding the durable volume right now?
///
/// `false` covers two different situations (nothing is running, and something
/// is running somewhere else), which is why the caller pairs it with whether
/// the hub answered before saying anything.
fn volume_attached(home: &Path) -> bool {
    let Ok(store) = botroster_store::Store::open(home) else {
        return false;
    };
    let Some(id) = store.volumes().ok().and_then(|v| v.into_iter().next()) else {
        return false;
    };
    store.volume(&id).is_ok_and(|v| v.is_attached())
}

/// The first line of an error, for a screen that is meant to be scannable.
///
/// A tool failure can carry a page of output; a status screen that reprints it
/// stops being a status screen. `botroster routine show` has the whole thing.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= 80 {
        return line.to_owned();
    }
    line.chars().take(79).collect::<String>() + "…"
}

/// "3 minutes ago", roughly. Precision here is not the point.
fn human_age(d: chrono::Duration) -> String {
    let mins = d.num_minutes();
    match mins {
        m if m < 2 => "just now".into(),
        m if m < 90 => format!("{m} min ago"),
        _ if d.num_hours() < 48 => format!("{} hours ago", d.num_hours()),
        _ => format!("{} days ago", d.num_days()),
    }
}

/// Render it. Kept separate from gathering so the wording is testable.
pub fn render(s: &Status, st: Style) -> String {
    let mut out = String::new();
    let row = |out: &mut String, label: &str, value: String| {
        out.push_str(&format!(
            "  {}  {}\n",
            st.dim(&crate::pad_label(label)),
            value
        ));
    };

    // First, and only when broken. A working config needs no line - this screen
    // is a list of what is wrong - but a broken one has to come before the model
    // and permission lines, because those are being rendered from defaults that
    // are not what the file says.
    if let Err(why) = &s.config {
        row(
            &mut out,
            "config",
            format!(
                "{}
{}",
                st.red("will not load — nothing below reflects your file"),
                st.dim(&format!("                {why}"))
            ),
        );
    }

    match &s.hub {
        Ok(servers) if servers.is_empty() => row(
            &mut out,
            "hub",
            format!(
                "{}  {}",
                s.hub_url,
                st.yellow("up, but no computer is attached — is botroster-guest running?")
            ),
        ),
        Ok(servers) => row(
            &mut out,
            "hub",
            format!("{}  {}", s.hub_url, st.green(&servers.join(", "))),
        ),
        Err(why) => row(
            &mut out,
            "hub",
            format!("{}  {}", s.hub_url, st.red(&format!("unreachable — {why}"))),
        ),
    }

    match (&s.model, s.key_present) {
        (None, _) => row(
            &mut out,
            "model",
            st.yellow("none configured — `botroster config set --model …`, or use --demo"),
        ),
        // Said rather than left blank. A model with an empty key variable would
        // otherwise print its name followed by nothing, which reads as a row
        // that failed to render rather than as a deliberate arrangement.
        (Some(m), true) if s.key_env.trim().is_empty() => row(
            &mut out,
            "model",
            format!("{m}  {}", st.dim("no key needed")),
        ),
        (Some(m), true) => row(&mut out, "model", format!("{m}  {}", st.dim(&s.key_env))),
        (Some(m), false) => row(
            &mut out,
            "model",
            format!(
                "{m}  {}",
                st.red(&format!("{} is not set in this shell", s.key_env))
            ),
        ),
    }

    row(&mut out, "bots", s.bots.to_string());

    let routines = match (s.routines_enabled, s.routines_paused) {
        (0, 0) => st.dim("none").to_string(),
        (e, 0) => format!("{e} enabled"),
        (e, p) => format!("{e} enabled, {p} paused"),
    };
    row(&mut out, "routines", routines);

    // Loudest thing on the screen, because it is the one that has already gone
    // wrong. Everything else here answers "why is nothing happening"; this
    // answers "something happened and it did not work".
    for (name, why) in &s.failing {
        row(
            &mut out,
            "failing",
            st.red(&format!("{name} — {}", first_line(why))),
        );
    }

    match s.snapshots {
        // Only shown once there is a computer to protect. A fresh install
        // saying "0 snapshots" is noise.
        None => {}
        Some((n, age)) => {
            let ago = human_age(age);
            // A computer is serving and it is not on this volume, so these
            // snapshots are of files nothing is writing. Checked before the
            // age, because a recent snapshot of the wrong directory is the
            // more misleading of the two.
            let value = if s.hub.is_ok() && !s.volume_attached {
                st.yellow(&format!(
                    "{n}, newest {ago} — but the running computer is not on this volume, so nothing here is being snapshotted"
                ))
            } else if age > chrono::Duration::hours(6) {
                st.yellow(&format!("{n}, newest {ago} — is `botroster up` running?"))
            } else {
                format!("{n}, newest {ago}")
            };
            row(&mut out, "snapshots", value);
        }
    }

    if !s.due_now.is_empty() {
        // Due now means the next `botroster routine tick` will run it. Its own
        // line, because "why has nothing happened" is usually "nothing has
        // ticked".
        row(
            &mut out,
            "due now",
            format!(
                "{}  {}",
                s.due_now.join(", "),
                st.dim("run `botroster routine tick`")
            ),
        );
    }

    if s.routines_enabled > 0 {
        match s.last_seen {
            None => row(
                &mut out,
                "watched",
                st.dim("nobody has looked from a terminal yet — routines will not idle-pause"),
            ),
            Some(t) => {
                let days = (chrono::Utc::now() - t).num_days();
                let value = if days >= s.idle_days {
                    st.red(&format!(
                        "{days} days ago — routines are paused until you resume them"
                    ))
                } else {
                    format!("{days} days ago")
                };
                row(&mut out, "watched", value);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config that will not load is the first thing this screen says.
    ///
    /// The command's one-line help is "Is anything wrong?", and it answered no
    /// about a `config.toml` the hub refuses to start on: `applied` reads the
    /// file with `load(home).unwrap_or_default()`, so an unparseable file
    /// silently became the defaults and every line below was rendered from
    /// settings nobody had chosen. Found by writing the README's own
    /// `[permission]` example to a home, which the parser rejects.
    #[test]
    fn a_config_that_will_not_load_is_reported_before_anything_else() {
        let mut s = base();
        s.config = Err("[permission] rule 1: unknown variant `ask`".into());
        let shown = render(&s, Style::plain());
        let first = shown.lines().next().unwrap_or_default();
        assert!(
            first.contains("config"),
            "a broken config must come before the lines rendered from defaults: {shown}"
        );
        assert!(
            shown.contains("unknown variant"),
            "the reason has to travel with the warning, or the next step is a guess: {shown}"
        );
    }

    /// And a config that loads gets no line at all.
    ///
    /// This screen is a list of what is wrong. A reassuring "config ok" row on
    /// every run is how people stop reading the rows that matter — and without
    /// this, a renderer that printed the warning unconditionally would pass the
    /// test above.
    #[test]
    fn a_config_that_loads_says_nothing() {
        let shown = render(&base(), Style::plain());
        // By label, not by substring: "config" is inside "none configured" on
        // the model line, so a substring test here answers a different
        // question and fails on a perfectly healthy account.
        assert!(
            !shown
                .lines()
                .any(|l| l.split_whitespace().next() == Some("config")),
            "a working config should be silent: {shown}"
        );
    }

    fn base() -> Status {
        Status {
            config: Ok(()),
            hub_url: "ws://127.0.0.1:8443/v1/tools".into(),
            hub: Ok(vec!["botroster-workspace".into()]),
            model: Some("grok-4-5".into()),
            key_env: "XAI_API_KEY".into(),
            key_present: true,
            bots: 3,
            routines_enabled: 2,
            routines_paused: 0,
            due_now: vec![],
            failing: vec![],
            snapshots: None,
            // The healthy case: a computer is running and it is on the volume
            // these snapshots belong to.
            volume_attached: true,
            last_seen: Some(chrono::Utc::now()),
            idle_days: 14,
        }
    }

    /// A computer that is running somewhere else is not covered by these
    /// snapshots, and the screen has to say so.
    ///
    /// `botroster up --workspace <dir>` puts the guest on a plain directory that
    /// nothing snapshots. Reporting "1, newest just now" alone would claim the
    /// computer is protected when it is not.
    #[test]
    fn snapshots_of_a_volume_nothing_is_using_say_so() {
        let mut s = base();
        s.snapshots = Some((1, chrono::Duration::minutes(1)));
        s.volume_attached = false;
        let out = render(&s, Style::plain());
        assert!(
            out.contains("not on this volume"),
            "a recent snapshot of the wrong directory read as protection:
{out}"
        );

        // When the guest is on the volume, the line stays the quiet one.
        s.volume_attached = true;
        let ok = render(&s, Style::plain());
        assert!(
            !ok.contains("not on this volume"),
            "a healthy computer was told its snapshots do not count:
{ok}"
        );
        assert!(ok.contains("1, newest"), "{ok}");
    }

    /// Not attached and no hub is simply nothing running, which the hub line
    /// already reports. Saying it twice, in the words of a different problem,
    /// sends somebody looking for a workspace mismatch that is not there.
    #[test]
    fn a_stopped_computer_is_not_reported_as_a_workspace_mistake() {
        let mut s = base();
        s.hub = Err("connection refused".into());
        s.snapshots = Some((3, chrono::Duration::hours(2)));
        s.volume_attached = false;
        let out = render(&s, Style::plain());
        assert!(
            !out.contains("not on this volume"),
            "a stopped computer was reported as a misplaced workspace:
{out}"
        );
    }

    #[test]
    fn a_healthy_account_reads_plainly() {
        let out = render(&base(), Style::plain());
        assert!(out.contains("botroster-workspace"));
        assert!(out.contains("grok-4-5"));
        assert!(out.contains("3"));
    }

    #[test]
    fn a_routine_failing_every_night_is_on_the_screen() {
        // "2 enabled" is equally true of a routine that has thrown the same
        // error nightly for a week. The run history is in a command nobody
        // opens; the failure has to be on this screen.
        let mut s = base();
        s.failing = vec![(
            "account-health/morning".into(),
            "[-32003] no tool server registered as `botroster-workspace`".into(),
        )];
        let out = render(&s, Style::plain());
        assert!(out.contains("failing"), "{out}");
        assert!(out.contains("account-health/morning"), "{out}");
        assert!(out.contains("no tool server"), "{out}");
    }

    #[test]
    fn a_failure_is_cut_to_one_line() {
        // A tool failure can carry a page of output. A status screen that
        // reprints it is no longer a status screen.
        let mut s = base();
        s.failing = vec![(
            "a/b".into(),
            format!(
                "first line
second line
{}",
                "x".repeat(300)
            ),
        )];
        let out = render(&s, Style::plain());
        assert!(out.contains("first line"), "{out}");
        assert!(!out.contains("second line"), "the whole error was printed");
        assert!(
            out.lines().count() < 12,
            "the screen stopped being scannable"
        );
    }

    #[test]
    fn snapshots_that_stopped_say_so() {
        // A schedule that has quietly stopped looks exactly like one that is
        // working, right up until somebody needs the rollback.
        let mut s = base();
        s.snapshots = Some((48, chrono::Duration::hours(30)));
        let out = render(&s, Style::plain());
        assert!(out.contains("30 hours ago"), "{out}");
        assert!(
            out.contains("botroster up"),
            "it does not say what to check: {out}"
        );
    }

    #[test]
    fn recent_snapshots_are_reported_without_alarm() {
        let mut s = base();
        s.snapshots = Some((12, chrono::Duration::minutes(20)));
        let out = render(&s, Style::plain());
        assert!(out.contains("20 min ago"), "{out}");
        assert!(
            !out.contains("botroster up"),
            "a working schedule raised an alarm: {out}"
        );
    }

    #[test]
    fn a_computer_with_no_snapshots_yet_says_nothing_about_them() {
        let s = base();
        let out = render(&s, Style::plain());
        assert!(!out.contains("snapshots"), "{out}");
    }

    #[test]
    fn an_unreachable_hub_says_so_rather_than_looking_empty() {
        let mut s = base();
        s.hub = Err("connection refused".into());
        let out = render(&s, Style::plain());
        assert!(out.contains("unreachable"), "{out}");
        assert!(out.contains("connection refused"), "{out}");
    }

    #[test]
    fn a_hub_with_no_computer_names_the_missing_piece() {
        // The most confusing state: everything looks fine, and every tool call
        // fails with "no such server".
        let mut s = base();
        s.hub = Ok(vec![]);
        let out = render(&s, Style::plain());
        assert!(out.contains("no computer is attached"), "{out}");
        assert!(out.contains("botroster-guest"), "{out}");
    }

    #[test]
    fn a_missing_key_is_called_out_even_though_a_model_is_configured() {
        // config.toml naming an env var says nothing about that var existing
        // in the shell a routine runs in, a common way for cron to fail.
        let mut s = base();
        s.key_present = false;
        let out = render(&s, Style::plain());
        assert!(out.contains("not set in this shell"), "{out}");
    }

    /// A model that wants no key is a working arrangement, and `status` is the
    /// one command somebody runs to ask whether things are set up. Calling a
    /// local model's absent key a problem would have it contradict an agent
    /// that starts perfectly well, which is worse than saying nothing.
    #[test]
    fn a_model_that_needs_no_key_is_not_reported_as_a_problem() {
        let mut s = base();
        s.key_env = String::new();
        s.key_present = true;
        let out = render(&s, Style::plain());
        assert!(
            !out.contains("not set in this shell"),
            "a local model was reported as missing a key it never wanted: {out}"
        );
        assert!(
            out.contains("no key needed"),
            "the row does not say why the key column is empty: {out}"
        );
    }

    #[test]
    fn something_due_says_what_to_type() {
        let mut s = base();
        s.due_now = vec!["account-health/morning".into()];
        let out = render(&s, Style::plain());
        assert!(out.contains("account-health/morning"));
        assert!(out.contains("routine tick"), "{out}");
    }

    #[test]
    fn a_long_absence_explains_why_nothing_ran() {
        let mut s = base();
        s.last_seen = Some(chrono::Utc::now() - chrono::Duration::days(30));
        let out = render(&s, Style::plain());
        assert!(out.contains("30 days ago"), "{out}");
        assert!(out.contains("paused"), "{out}");
    }

    #[test]
    fn an_account_with_no_routines_says_nothing_about_watching() {
        // The idle rule only matters if something is scheduled; mentioning it
        // otherwise is noise in the one view meant to be scannable.
        let mut s = base();
        s.routines_enabled = 0;
        s.last_seen = None;
        let out = render(&s, Style::plain());
        assert!(!out.contains("watched"), "{out}");
    }
}
