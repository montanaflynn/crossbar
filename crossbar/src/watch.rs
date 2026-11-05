//! switchboard --watch: a line each time one of the caller's reports turns
//! done, blocked or error (with its message), stops running Claude, or
//! closes. The reports of a project's operator are the agents in its
//! session's tabs; an exchange's supervisor also hears from the operators of
//! the exchange's projects.
//!
//! At startup it prints every report that is blocked or in error now, and a
//! report that is done only if no earlier watch of the same session printed
//! that already. What each watch knew of each report (its state, and whether
//! it printed that) is kept in ~/.crossbar/watch/<session>.json, so a watch
//! restarted after a crash or a sleep misses nothing and repeats no old
//! news: a report it knew that stopped or closed while no watch ran is
//! printed once at startup too.
//!
//! With --next (`cbar watch --next`) it waits for the first event, prints it
//! and whatever else comes within a second, and exits 0; a report that
//! stands blocked or in error is not printed again once a watch printed it.
//! Run one after another, they print every event once.
//!
//! A done line just like the last one printed for its pane (same state,
//! kind and message, from the same Claude) is not printed again, however
//! many turns end on it: the ledger keeps that line through the looks in
//! between and across runs. A stop or a close is still printed, and drops
//! it, so what the pane says next is news.
//!
//! While it runs, a watch holds a lease for its session: a file under
//! ~/.crossbar/watch/<session>.live/ that it keeps locked (flock) until it
//! exits, so a watch that was killed holds none. `watching` says whether a
//! watch runs for a session, and `waiting` what a `--next` started now would
//! print at once. With them `cbar ls` and the switchboards flag a supervisor
//! or operator that runs with no watch, and `cbar watch --check` (the Stop
//! hook crossbar gives the supervisors and operators it starts) tells that
//! Claude at the end of its turn.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::layout::{Home, OPERATOR, SEP, SUPERVISOR};
use crate::status::{Group, Looker, Pane, Snapshot};
use crate::util;

const REPORTABLE: [&str; 3] = ["done", "blocked", "error"];
/// Reported at every startup; the others only when not seen before.
const STANDING: [&str; 2] = ["blocked", "error"];

#[derive(Clone, Debug, PartialEq)]
struct Report {
    name: String,
    state: String,
    kind: String,
    msg: String,
    since: String,
    /// The pid of the process that reported the state, if known.
    owner: Option<i64>,
}

/// What a watch knew about a report, keyed by its pane id. (Files of 0.5.5
/// and earlier are keyed by the report's name and hold only what was
/// printed: those entries have no name.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Seen {
    #[serde(default)]
    name: String,
    state: String,
    #[serde(default)]
    kind: String,
    msg: String,
    since: String,
    #[serde(default)]
    owner: Option<i64>,
    /// Whether a watch printed it as it is.
    #[serde(default = "yes")]
    printed: bool,
    /// The last done line a watch printed for this pane, kept through the
    /// looks in between (its turns' working) until the pane stops or closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    said: Option<Said>,
}

/// A done line as printed: its state, kind and message, and which process
/// reported it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Said {
    state: String,
    #[serde(default)]
    kind: String,
    msg: String,
    #[serde(default)]
    owner: Option<i64>,
}

impl Said {
    fn of(r: &Report) -> Said {
        Said { state: r.state.clone(), kind: r.kind.clone(), msg: r.msg.clone(), owner: r.owner }
    }

    /// Whether printing R would say this again: the same line from the same
    /// Claude (a new process in the pane is a new Claude, so news).
    fn repeats(&self, r: &Report) -> bool {
        let same_owner = match (self.owner, r.owner) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        (&self.state, &self.kind, &self.msg) == (&r.state, &r.kind, &r.msg) && same_owner
    }
}

impl Seen {
    /// The last done line printed for its pane (a ledger of 0.5.6 or
    /// earlier has only the entry itself, if it was a printed done).
    fn said(&self) -> Option<Said> {
        self.said.clone().or_else(|| {
            (self.printed && self.state == "done").then(|| Said { state: self.state.clone(), kind: self.kind.clone(), msg: self.msg.clone(), owner: self.owner })
        })
    }
}

fn yes() -> bool {
    true
}

/// The look a watch of session SCOPE (id) labelled LABEL takes: every
/// session for an exchange's (its projects' operators report to it), else
/// that session's.
fn snapshot(look: &Looker, home: &Home, scope: &str, label: &str) -> Snapshot {
    look.snapshot(if home.exchange_exists(label) { None } else { Some(scope) })
}

/// {pane id: report} for the panes in SNAP reporting to whoever runs in
/// session SCOPE (id) labelled LABEL: that session's panes and, in an
/// exchange's session, its projects' operators. Switchboards and the panes
/// in SKIP (the watcher's own, when the Looker didn't hide it) are left out.
fn reports(snap: &Snapshot, home: &Home, scope: &str, label: &str, skip: &[&str]) -> HashMap<String, Report> {
    let mut found = HashMap::new();
    let mut add = |name: String, p: &Pane| {
        if p.program != "switchboard" && p.label != "switchboard" && !skip.contains(&p.block_id.as_str()) {
            let r = Report { name, state: p.state.clone(), kind: p.kind.clone(), msg: p.msg.clone(), since: p.since_at.clone(), owner: p.owner };
            found.insert(p.block_id.clone(), r);
        }
    };
    let name_of = |p: &Pane| if p.tab.is_empty() { p.program.clone() } else { p.tab.clone() };
    let exchange = home.exchange_exists(label);
    let prefix = format!("{label}{SEP}");
    for (g, panes) in snap {
        for p in panes {
            if p.session_id == scope {
                add(name_of(p), p);
            } else if exchange && p.tab == OPERATOR {
                if let Some(project) = g.label.strip_prefix(&prefix) {
                    add(format!("{project} operator"), p);
                }
            }
        }
    }
    found
}

/// Session LABEL as a file name.
fn file_name(label: &str) -> String {
    label.replace(SEP, "+")
}

/// Where a watch of session LABEL keeps what it printed.
fn seen_file(home: &Home, label: &str) -> PathBuf {
    home.root.join("watch").join(format!("{}.json", file_name(label)))
}

/// Where the watches of session LABEL keep their leases, one file each.
fn lease_dir(home: &Home, label: &str) -> PathBuf {
    home.root.join("watch").join(format!("{}.live", file_name(label)))
}

/// flock(F, HOW) without waiting: whether it got the lock.
fn flock(f: &File, how: libc::c_int) -> bool {
    unsafe { libc::flock(f.as_raw_fd(), how | libc::LOCK_NB) == 0 }
}

/// The lease of a running watch: `<pid>.json` (its pid, when it started and
/// its mode) in its session's lease folder, locked while the watch runs. It
/// is removed when the watch ends; the lock goes with the process however it
/// ends (and a pid used again holds no lock), so a file left by a killed
/// watch counts for nothing.
pub struct Lease {
    path: PathBuf,
    _file: File,
}

/// The lease to remove if the watch is killed (SIGTERM, SIGHUP, SIGINT).
static LEASE_PATH: OnceLock<CString> = OnceLock::new();

extern "C" fn drop_lease(sig: libc::c_int) {
    if let Some(p) = LEASE_PATH.get() {
        unsafe { libc::unlink(p.as_ptr()) };
    }
    unsafe { libc::_exit(128 + sig) };
}

impl Lease {
    /// Take a lease for a watch of session LABEL in mode MODE ("next" or
    /// "plain"). It is written and locked under a temporary name, then
    /// renamed into place, so no one finds it unlocked.
    pub fn take(home: &Home, label: &str, mode: &str) -> std::io::Result<Lease> {
        let dir = lease_dir(home, label);
        std::fs::create_dir_all(&dir)?;
        let pid = std::process::id();
        let tmp = dir.join(format!(".{pid}.tmp"));
        let mut file = File::create(&tmp)?;
        if !flock(&file, libc::LOCK_EX) {
            return Err(std::io::Error::last_os_error());
        }
        let info = serde_json::json!({"pid": pid, "started": util::utc_iso_now(), "mode": mode});
        file.write_all(info.to_string().as_bytes())?;
        let path = dir.join(format!("{pid}.json"));
        std::fs::rename(&tmp, &path)?;
        if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
            if LEASE_PATH.set(c).is_ok() {
                for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                    unsafe { libc::signal(sig, drop_lease as *const () as libc::sighandler_t) };
                }
            }
        }
        Ok(Lease { path, _file: file })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the lease file PATH is held by a running watch. One that isn't is
/// removed (if it is still the file we looked at).
fn held(path: &Path) -> bool {
    let Ok(f) = File::open(path) else { return false };
    if !flock(&f, libc::LOCK_EX) {
        return true;
    }
    // nobody holds it: its watch is gone
    let ours = f.metadata().ok().map(|m| (m.dev(), m.ino()));
    if ours.is_some() && std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino())) == ours {
        let _ = std::fs::remove_file(path);
    }
    false
}

/// Whether a watch (`cbar watch`, plain or --next) runs for session LABEL.
pub fn watching(home: &Home, label: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(lease_dir(home, label)) else { return false };
    let mut found = false;
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        // every file is looked at, so those of killed watches are cleared
        if !name.starts_with('.') && name.ends_with(".json") && held(&e.path()) {
            found = true;
        }
    }
    found
}

/// The lines a `cbar watch --next` started now in session SCOPE (id)
/// labelled LABEL would print at once (in its first looks, a missing message
/// waited for), from SNAP: the events waiting for a watch. It changes
/// nothing, the ledger included. SKIP: as for `reports`.
pub fn waiting(home: &Home, snap: &Snapshot, scope: &str, label: &str, skip: &[&str]) -> Vec<String> {
    let cur = reports(snap, home, scope, label, skip);
    let mut w = Watch::new(Ledger::peek(seen_file(home, label)), true);
    let alive = |pid: i64| util::alive(&pid.to_string());
    pending(&mut w, &cur, &alive)
}

/// The lines W prints over its first two looks at CUR, the second as late
/// as a missing message is waited for (what closed shows then, and what had
/// no message yet).
fn pending(w: &mut Watch, cur: &HashMap<String, Report>, alive: &dyn Fn(i64) -> bool) -> Vec<String> {
    let now = Instant::now();
    let mut events = w.look(cur, now, alive);
    w.record(&events);
    let more = w.look(cur, now + w.tracker.wait_msg, alive);
    events.extend(more);
    events.iter().map(|(_, r, state)| line(&r.name, state, &r.kind, &r.msg)).collect()
}

/// The role whose Claude watches session LABEL: an exchange's supervisor, a
/// registered project's operator; None for any other session.
fn watcher(home: &Home, label: &str) -> Option<&'static str> {
    if home.exchange_exists(label) {
        return Some(SUPERVISOR);
    }
    let (e, p) = label.split_once(SEP)?;
    home.project(e, p).map(|_| OPERATOR)
}

/// The supervisor and operator panes in SNAP, of SESSIONS (id, label), whose
/// Claude runs while no watch does: pane id → the events waiting for it.
/// Agents never count: they don't watch. For an exchange's session, SNAP must
/// hold every session to count its operators' events.
pub fn unwatched(home: &Home, snap: &Snapshot, sessions: &[(String, String)]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for (sid, label) in sessions {
        let Some(role) = watcher(home, label) else { continue };
        let pane = snap.iter().flat_map(|(_, ps)| ps).find(|p| p.session_id == *sid && p.tab == role && p.label != "switchboard" && !p.state.is_empty());
        if let Some(p) = pane {
            if !watching(home, label) {
                out.insert(p.block_id.clone(), waiting(home, snap, sid, label, &[&p.block_id]));
            }
        }
    }
    out
}

/// The sessions of SNAP (id, label), when its groups are sessions (not one
/// session's tabs).
pub fn sessions(snap: &Snapshot) -> Vec<(String, String)> {
    snap.iter().filter(|(g, _)| g.window_id.is_none()).map(|(g, _)| (g.session_id.clone(), g.label.clone())).collect()
}

/// What `cbar watch --check`, the Stop hook, tells the Claude in session G
/// whose turn ends, from the hook's INPUT: None when a watch runs, when
/// Claude Code is already going on for a Stop hook (`stop_hook_active`: at
/// most once a turn), or when its background tasks hold a `cbar watch` (one
/// just started, before its lease); else why it should go on.
pub fn check(home: &Home, g: &Group, input: &Value) -> Option<String> {
    if input["stop_hook_active"].as_bool() == Some(true) || starting(input) || watching(home, &g.label) {
        return None;
    }
    let snap = snapshot(&Looker::new(true), home, &g.session_id, &g.label);
    // one may have started while we looked
    if watching(home, &g.label) {
        return None;
    }
    Some(nudge(&g.label, &waiting(home, &snap, &g.session_id, &g.label, &[])))
}

/// Whether the Stop hook's INPUT lists a running background `cbar watch`:
/// one just started, before it took its lease, or one of a cbar too old to take one.
fn starting(input: &Value) -> bool {
    input["background_tasks"].as_array().is_some_and(|ts| {
        ts.iter().any(|t| {
            let cmd = t["command"].as_str().unwrap_or("");
            t["status"].as_str().is_none_or(|s| s == "running") && (cmd.contains("cbar watch") || cmd.contains("switchboard --watch"))
        })
    })
}

/// What the Stop hook says when no watch runs for SESSION, with the events WAITING.
pub fn nudge(session: &str, waiting: &[String]) -> String {
    let what = match waiting.len() {
        0 => "no events are waiting yet".to_string(),
        1 => format!("1 event is waiting: {}", waiting[0]),
        n => format!("{n} events are waiting: {}", waiting.join("; ")),
    };
    format!("cbar watch isn't running for {session}; {what}. Start `cbar watch --next` as a background Bash command, then act on what it prints.")
}

struct Ledger {
    path: PathBuf,
    seen: HashMap<String, Seen>,
    /// What the file holds.
    saved: HashMap<String, Seen>,
    /// Only looked at: never saved.
    readonly: bool,
}

impl Ledger {
    fn load(path: PathBuf) -> Ledger {
        let seen: HashMap<String, Seen> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Ledger { path, saved: seen.clone(), seen, readonly: false }
    }

    /// The ledger at PATH, to look at only.
    fn peek(path: PathBuf) -> Ledger {
        Ledger { readonly: true, ..Ledger::load(path) }
    }

    fn save(&mut self) {
        self.saved = self.seen.clone();
        if self.readonly {
            return;
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string_pretty(&self.seen).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn entry(r: &Report, printed: bool, said: Option<Said>) -> Seen {
        Seen { name: r.name.clone(), state: r.state.clone(), kind: r.kind.clone(), msg: r.msg.clone(), since: r.since.clone(), owner: r.owner, printed, said }
    }

    /// A watch printed R (in pane KEY) as it is now.
    fn printed(&mut self, key: &str, r: &Report) {
        let said = if r.state == "done" { Some(Said::of(r)) } else { self.said(key) };
        self.seen.insert(key.to_string(), Ledger::entry(r, true, said));
    }

    /// The last done line a watch printed for pane KEY.
    fn said(&self, key: &str) -> Option<Said> {
        self.seen.get(key).and_then(Seen::said)
    }

    /// Whether R in pane KEY is a done line just like the last one printed
    /// there: noise, not news.
    fn repeats(&self, key: &str, r: &Report) -> bool {
        r.state == "done" && self.said(key).is_some_and(|s| s.repeats(r))
    }

    /// Whether a watch already printed R as it is now.
    fn knows(&self, r: &Report) -> bool {
        self.seen.iter().any(|(k, s)| {
            let name = if s.name.is_empty() { k } else { &s.name };
            s.printed && *name == r.name && s.state == r.state && s.msg == r.msg && s.since == r.since
        })
    }

    /// The reports the last watch knew, by pane id: where the next one starts.
    fn known(&self) -> HashMap<String, Report> {
        self.seen
            .iter()
            .filter(|(_, s)| !s.name.is_empty() && !s.state.is_empty())
            .map(|(k, s)| (k.clone(), Report { name: s.name.clone(), state: s.state.clone(), kind: s.kind.clone(), msg: s.msg.clone(), since: s.since.clone(), owner: s.owner }))
            .collect()
    }

    /// Keep what the watch knows now (LAST, the tracker's) and save it if
    /// that changed. A report that stopped or closed is dropped, and with it
    /// the last line printed for its pane.
    fn sync(&mut self, last: &HashMap<String, Report>) {
        let now: HashMap<String, Seen> =
            last.iter().filter(|(_, r)| !r.state.is_empty()).map(|(k, r)| (k.clone(), Ledger::entry(r, self.knows(r), self.said(k)))).collect();
        self.seen = now;
        if self.seen != self.saved {
            self.save();
        }
    }
}

fn line(name: &str, state: &str, kind: &str, msg: &str) -> String {
    let kind = if kind.is_empty() { String::new() } else { format!(" ({kind})") };
    let msg = if msg.is_empty() { String::new() } else { format!(" — {msg}") };
    format!("{name}: {state}{kind}{msg}")
}

/// What a watch remembers between looks, and the events each look makes.
///
/// A report without a message waits up to WAIT_MSG for one (Rex sends the
/// summary just after the state); a pane must be missing from two looks in a
/// row to count as closed. A report whose state is gone counts as stopped
/// only once the process that reported it has exited (or, if nobody knows
/// which process that was, after two looks in a row without a state): while
/// it runs, its Claude is still there, and a look that found no state was a
/// read that failed or a status Rex dropped, not a stop.
struct Tracker {
    last: HashMap<String, Report>,
    pending: HashMap<String, Instant>,
    missing: HashMap<String, u32>,
    blank: HashMap<String, u32>,
    first: bool,
    wait_msg: Duration,
}

impl Tracker {
    fn new(wait_msg: Duration) -> Tracker {
        Tracker {
            last: HashMap::new(),
            pending: HashMap::new(),
            missing: HashMap::new(),
            blank: HashMap::new(),
            first: true,
            wait_msg,
        }
    }

    /// The events of the look CUR at NOW: (pane id, report, the state to
    /// print). ALIVE says whether a pid still runs.
    fn step(&mut self, cur: &HashMap<String, Report>, now: Instant, alive: &dyn Fn(i64) -> bool) -> Vec<(String, Report, String)> {
        let mut events = Vec::new();
        let first = self.first;
        self.first = false;
        for (key, r) in cur {
            self.missing.remove(key);
            let prev = self.last.get(key).cloned();
            if r.state.is_empty() {
                match &prev {
                    None => continue, // not an agent (yet)
                    Some(p) if !p.state.is_empty() => {
                        let ended = match p.owner {
                            Some(pid) => !alive(pid),
                            None => {
                                let n = self.blank.entry(key.clone()).or_insert(0);
                                *n += 1;
                                *n >= 2
                            }
                        };
                        if !ended {
                            continue; // its Claude still runs: keep what we knew
                        }
                        self.blank.remove(key);
                        self.pending.remove(key);
                        self.last.insert(key.clone(), r.clone());
                        events.push((key.clone(), r.clone(), "stopped".into()));
                        continue;
                    }
                    Some(_) => {}
                }
            }
            self.blank.remove(key);
            self.last.insert(key.clone(), r.clone());
            if first || prev.as_ref().is_some_and(|p| (&p.state, &p.msg) == (&r.state, &r.msg)) {
                continue;
            }
            self.pending.remove(key);
            if REPORTABLE.contains(&r.state.as_str()) {
                if !r.msg.is_empty() || prev.as_ref().is_some_and(|p| p.state == r.state) {
                    events.push((key.clone(), r.clone(), r.state.clone()));
                } else {
                    self.pending.insert(key.clone(), now);
                }
            }
        }
        for (key, since) in self.pending.clone() {
            let r = self.last[&key].clone();
            if !REPORTABLE.contains(&r.state.as_str()) {
                self.pending.remove(&key);
            } else if !r.msg.is_empty() || now.duration_since(since) >= self.wait_msg {
                self.pending.remove(&key);
                events.push((key.clone(), r.clone(), r.state.clone()));
            }
        }
        let gone: Vec<String> = self.last.keys().filter(|k| !cur.contains_key(*k)).cloned().collect();
        for key in gone {
            let n = self.missing.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n >= 2 {
                let r = self.last[&key].clone();
                if !first {
                    events.push((key.clone(), r, "closed".into()));
                }
                self.last.remove(&key);
                self.missing.remove(&key);
                self.pending.remove(&key);
                self.blank.remove(&key);
            }
        }
        events
    }
}

/// What a watch prints at startup, from the first look CUR: (pane id,
/// report) in name order. Blocked and error always (with NEXT, only if no
/// watch printed them as they are), done only if no watch printed it. A new
/// report without a message yet goes to QUIET instead, to wait for one.
fn startup(cur: &HashMap<String, Report>, ledger: &Ledger, next: bool) -> (Vec<(String, Report)>, Vec<String>) {
    let mut now: Vec<(&String, &Report)> = cur.iter().filter(|(_, r)| REPORTABLE.contains(&r.state.as_str())).collect();
    now.sort_by(|a, b| (&a.1.name, a.0).cmp(&(&b.1.name, b.0)));
    let (mut say, mut quiet) = (Vec::new(), Vec::new());
    for (key, r) in now {
        if ledger.knows(r) {
            if STANDING.contains(&r.state.as_str()) && !next {
                say.push((key.clone(), r.clone()));
            }
        } else if r.msg.is_empty() {
            quiet.push(key.clone());
        } else {
            say.push((key.clone(), r.clone()));
        }
    }
    (say, quiet)
}

/// One watch: its tracker, started from what the last watch knew (so what
/// stopped or closed since is news), and its ledger.
struct Watch {
    tracker: Tracker,
    ledger: Ledger,
    next: bool,
}

impl Watch {
    fn new(ledger: Ledger, next: bool) -> Watch {
        let mut tracker = Tracker::new(Duration::from_secs(8));
        tracker.last = ledger.known();
        Watch { tracker, ledger, next }
    }

    /// The events of the look CUR at NOW (with what stands, at startup).
    fn look(&mut self, cur: &HashMap<String, Report>, now: Instant, alive: &dyn Fn(i64) -> bool) -> Vec<(String, Report, String)> {
        let mut events = Vec::new();
        let mut quiet = Vec::new();
        if self.tracker.first {
            let (standing, q) = startup(cur, &self.ledger, self.next);
            events.extend(standing.into_iter().map(|(k, r)| {
                let s = r.state.clone();
                (k, r, s)
            }));
            quiet = q;
        }
        events.extend(self.tracker.step(cur, now, alive));
        for key in quiet {
            self.tracker.pending.insert(key, now);
        }
        // a turn that ends on the done line printed last for its pane is no news
        events.retain(|(key, r, state)| *state != r.state || !self.ledger.repeats(key, r));
        events
    }

    /// Keep that EVENTS were printed, and what the watch knows now.
    fn record(&mut self, events: &[(String, Report, String)]) {
        for (key, r, state) in events {
            if *state == r.state {
                self.ledger.printed(key, r);
            }
        }
        self.ledger.sync(&self.tracker.last);
    }
}

/// Watch, printing events (see Tracker): forever, or with NEXT until the
/// first look that has any, and one more a second later.
/// It holds its lease (see Lease) until it returns.
pub fn watch(home: &Home, scope: &str, label: &str, next: bool) {
    let _lease = Lease::take(home, label, if next { "next" } else { "plain" })
        .map_err(|e| util::warn(format!("could not take the watch lease in {}: {e}", util::short(lease_dir(home, label)))))
        .ok();
    let every = Duration::from_secs(3);
    let look = Looker::new(true);
    let mut w = Watch::new(Ledger::load(seen_file(home, label)), next);
    let mut out = std::io::stdout();
    let alive = |pid: i64| util::alive(&pid.to_string());
    let mut last_look = false;
    loop {
        let cur = reports(&snapshot(&look, home, scope, label), home, scope, label, &[]);
        let events = w.look(&cur, Instant::now(), &alive);
        for (_, r, state) in &events {
            if writeln!(out, "{}", line(&r.name, state, &r.kind, &r.msg)).and_then(|_| out.flush()).is_err() {
                return; // nobody is listening
            }
        }
        w.record(&events);
        if next && (last_look || !events.is_empty()) {
            if last_look {
                return;
            }
            // and whatever else happens in the same moment
            last_look = true;
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        std::thread::sleep(every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(panes: &[(&str, &str, &str, Option<i64>)]) -> HashMap<String, Report> {
        panes
            .iter()
            .map(|(key, state, msg, owner)| {
                let r = Report { name: format!("{key} operator"), state: state.to_string(), kind: String::new(), msg: msg.to_string(), since: "t1".into(), owner: *owner };
                (key.to_string(), r)
            })
            .collect()
    }

    fn events(e: Vec<(String, Report, String)>) -> Vec<String> {
        let mut v: Vec<String> = e.into_iter().map(|(_, r, s)| line(&r.name, &s, &r.kind, &r.msg)).collect();
        v.sort();
        v
    }

    /// The live false stop: two idle (done) operators whose state reads as
    /// nothing for one look (their reads timed out) while their Claudes run.
    #[test]
    fn a_blank_look_is_not_a_stop() {
        let mut t = Tracker::new(Duration::from_secs(8));
        let now = Instant::now();
        let alive = |_: i64| true;
        let idle = look(&[("grail", "done", "ok", Some(1)), ("crossbar", "done", "ok", Some(2))]);
        assert!(t.step(&idle, now, &alive).is_empty());
        let blank = look(&[("grail", "", "", None), ("crossbar", "", "", None)]);
        for _ in 0..3 {
            assert_eq!(events(t.step(&blank, now, &alive)), Vec::<String>::new());
        }
        // and back, as it was: nothing new, so nothing printed again
        assert!(t.step(&idle, now, &alive).is_empty());
    }

    #[test]
    fn a_real_stop_is_reported_at_once() {
        let mut t = Tracker::new(Duration::from_secs(8));
        let now = Instant::now();
        t.step(&look(&[("grail", "done", "ok", Some(1)), ("crossbar", "idle", "", Some(2))]), now, &|_| true);
        // grail's Claude exited; crossbar's runs, its status gone for now
        let blank = look(&[("grail", "", "", None), ("crossbar", "", "", None)]);
        assert_eq!(events(t.step(&blank, now, &|pid| pid != 1)), vec!["grail operator: stopped"]);
        assert!(t.step(&blank, now, &|pid| pid != 1).is_empty());
        // crossbar's Claude exits later: reported then
        assert_eq!(events(t.step(&blank, now, &|_| false)), vec!["crossbar operator: stopped"]);
        // a new Claude in grail's pane reports again
        let back = look(&[("grail", "done", "again", Some(3)), ("crossbar", "", "", None)]);
        assert_eq!(events(t.step(&back, now, &|_| true)), vec!["grail operator: done — again"]);
        // closed: missing from two looks in a row
        let gone = look(&[("crossbar", "", "", None)]);
        assert!(t.step(&gone, now, &|_| true).is_empty());
        assert_eq!(events(t.step(&gone, now, &|_| true)), vec!["grail operator: closed — again"]);
    }

    #[test]
    fn without_an_owner_two_blank_looks_are_a_stop() {
        let mut t = Tracker::new(Duration::from_secs(8));
        let now = Instant::now();
        t.step(&look(&[("a", "working", "", None)]), now, &|_| true);
        let blank = look(&[("a", "", "", None)]);
        assert!(t.step(&blank, now, &|_| true).is_empty());
        assert_eq!(events(t.step(&blank, now, &|_| true)), vec!["a operator: stopped"]);
        // one blank look between two states is nothing
        t.step(&look(&[("a", "working", "", None)]), now, &|_| true);
        assert!(t.step(&blank, now, &|_| true).is_empty());
        assert_eq!(events(t.step(&look(&[("a", "done", "x", None)]), now, &|_| true)), vec!["a operator: done — x"]);
    }

    #[test]
    fn remembers_what_it_printed() {
        let path = std::env::temp_dir().join(format!("cbar-watch-{}/x.json", std::process::id()));
        let r = Report { name: "fix-x".into(), state: "done".into(), kind: "".into(), msg: "ok".into(), since: "t1".into(), owner: None };
        let mut l = Ledger::load(path.clone());
        assert!(!l.knows(&r));
        l.printed("%1", &r);
        l.sync(&HashMap::from([("%1".to_string(), r.clone())]));
        let l2 = Ledger::load(path.clone());
        assert!(l2.knows(&r));
        assert!(!l2.knows(&Report { since: "t2".into(), ..r.clone() }));
        assert_eq!(l2.known(), HashMap::from([("%1".to_string(), r.clone())]));
        assert_eq!(line("a", "error", "api", "boom"), "a: error (api) — boom");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A ledger written by 0.5.5: keyed by name, only what was printed.
    #[test]
    fn reads_an_old_ledger() {
        let path = std::env::temp_dir().join(format!("cbar-watch-old-{}/x.json", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"fix-x": {"state": "done", "msg": "ok", "since": "t1"}}"#).unwrap();
        let l = Ledger::load(path.clone());
        let r = Report { name: "fix-x".into(), state: "done".into(), kind: "".into(), msg: "ok".into(), since: "t1".into(), owner: None };
        assert!(l.knows(&r));
        assert!(l.known().is_empty()); // no pane: nothing to start from
        // a watch that starts on it doesn't print fix-x again, and keeps that
        let mut w = Watch::new(l, true);
        let cur = HashMap::from([("%1".to_string(), r.clone())]);
        let e = w.look(&cur, Instant::now(), &|_| true);
        assert!(e.is_empty());
        w.record(&e);
        assert!(Ledger::load(path.clone()).knows(&r));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn report(name: &str, state: &str, msg: &str, since: &str, owner: Option<i64>) -> Report {
        let kind = if state == "blocked" { "permission" } else { "" };
        Report { name: name.into(), state: state.into(), kind: kind.into(), msg: msg.into(), since: since.into(), owner }
    }

    /// One `cbar watch --next` over LOOKS (each a map of pane id to report):
    /// it stops after the look that follows its first events. What it
    /// printed, or None if it was still waiting when the looks ran out.
    fn next_run(path: &std::path::Path, looks: &mut dyn Iterator<Item = HashMap<String, Report>>, alive: &dyn Fn(i64) -> bool) -> Option<Vec<String>> {
        let mut w = Watch::new(Ledger::load(path.to_path_buf()), true);
        let now = Instant::now();
        let mut printed: Vec<(String, Report, String)> = Vec::new();
        let mut last = false;
        for cur in looks {
            let e = w.look(&cur, now, alive);
            w.record(&e);
            printed.extend(e);
            if last {
                return Some(events(printed));
            }
            last = !printed.is_empty();
        }
        (!printed.is_empty()).then(|| events(printed))
    }

    #[test]
    fn next_exits_on_an_event_and_waits_while_idle() {
        let path = std::env::temp_dir().join(format!("cbar-watch-next-{}/x.json", std::process::id()));
        let alive = |_: i64| true;
        let working = HashMap::from([("%1".to_string(), report("fix-x", "working", "", "t1", Some(1)))]);
        // idle: nothing happens, look after look, so it keeps waiting
        assert_eq!(next_run(&path, &mut std::iter::repeat_n(working.clone(), 50), &alive), None);
        // it turns done: that look's event, one look more, and it's over
        let done = HashMap::from([("%1".to_string(), report("fix-x", "done", "fixed", "t2", Some(1)))]);
        let mut looks = vec![working.clone(), working.clone(), done.clone(), done.clone(), done.clone()].into_iter();
        assert_eq!(next_run(&path, &mut looks, &alive), Some(vec!["fix-x: done — fixed".to_string()]));
        assert_eq!(looks.len(), 1);
        // the next run: nothing new, so it waits
        assert_eq!(next_run(&path, &mut std::iter::repeat_n(done.clone(), 50), &alive), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Events between two runs (done, blocked, stopped, closed) are printed
    /// by the next run, none twice.
    #[test]
    fn next_runs_miss_nothing_between_them() {
        let path = std::env::temp_dir().join(format!("cbar-watch-gap-{}/x.json", std::process::id()));
        let map = |rs: Vec<(&str, Report)>| rs.into_iter().map(|(k, r)| (k.to_string(), r)).collect::<HashMap<_, _>>();
        let r = |n: &str, s: &str, m: &str, t: &str, pid: i64| report(n, s, m, t, Some(pid));
        let before = map(vec![
            ("%1", r("a", "working", "", "t1", 1)),
            ("%2", r("b", "working", "", "t1", 2)),
            ("%3", r("c", "idle", "", "t1", 3)),
            ("%4", r("d", "done", "old", "t1", 4)),
            ("%5", r("e", "working", "", "t1", 5)),
        ]);
        // the first run prints d's done (new to it) and exits
        assert_eq!(next_run(&path, &mut std::iter::repeat_n(before, 5), &|_| true), Some(vec!["d: done — old".to_string()]));
        // while no watch runs: a turns done, b blocked, c's Claude exits, d's tab closes; e works on
        let after = |b_since: &str| {
            map(vec![
                ("%1", r("a", "done", "ok", "t2", 1)),
                ("%2", r("b", "blocked", "allow?", b_since, 2)),
                ("%3", r("c", "", "", "", 3)),
                ("%5", r("e", "working", "", "t1", 5)),
            ])
        };
        let alive = |pid: i64| pid != 3;
        let got = next_run(&path, &mut std::iter::repeat_n(after("t2"), 5), &alive);
        assert_eq!(got, Some(vec!["a: done — ok", "b: blocked (permission) — allow?", "c: stopped", "d: closed — old"].into_iter().map(String::from).collect()));
        // the run after that has nothing to say, b's standing block included
        assert_eq!(next_run(&path, &mut std::iter::repeat_n(after("t2"), 50), &alive), None);
        // b blocks again (a new since): that's news
        assert_eq!(next_run(&path, &mut std::iter::repeat_n(after("t3"), 5), &alive), Some(vec!["b: blocked (permission) — allow?".to_string()]));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Plain `cbar watch` still prints what stands at every startup.
    #[test]
    fn a_plain_watch_repeats_what_stands() {
        let path = std::env::temp_dir().join(format!("cbar-watch-plain-{}/x.json", std::process::id()));
        let cur = HashMap::from([("%1".to_string(), report("b", "blocked", "allow?", "t1", None)), ("%2".to_string(), report("a", "done", "ok", "t1", None))]);
        for want in [vec!["a: done — ok", "b: blocked (permission) — allow?"], vec!["b: blocked (permission) — allow?"]] {
            let mut w = Watch::new(Ledger::load(path.clone()), false);
            let e = w.look(&cur, Instant::now(), &|_| true);
            w.record(&e);
            assert_eq!(events(e), want);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Each look of LOOKS through W (recorded as printed), and what it printed.
    fn through(w: &mut Watch, looks: &[HashMap<String, Report>], alive: &dyn Fn(i64) -> bool) -> Vec<String> {
        let mut said = Vec::new();
        for cur in looks {
            let e = w.look(cur, Instant::now(), alive);
            w.record(&e);
            said.extend(events(e));
        }
        said
    }

    fn one(state: &str, msg: &str, since: &str, owner: i64) -> HashMap<String, Report> {
        HashMap::from([("%1".to_string(), report("ops", state, msg, since, Some(owner)))])
    }

    /// An operator ends turn after turn on the same summary: printed once.
    #[test]
    fn the_same_done_across_turns_prints_once() {
        let path = std::env::temp_dir().join(format!("cbar-watch-same-{}/x.json", std::process::id()));
        let mut w = Watch::new(Ledger::load(path.clone()), false);
        let looks = [
            one("working", "", "t0", 1),
            one("done", "waits on push", "t1", 1),
            one("working", "", "t2", 1),
            one("done", "waits on push", "t3", 1),
            one("working", "", "t4", 1),
            one("done", "waits on push", "t5", 1),
        ];
        assert_eq!(through(&mut w, &looks, &|_| true), vec!["ops: done — waits on push"]);
        // a new message is news, and so is the old one after it
        let looks = [one("working", "", "t6", 1), one("done", "pushed", "t7", 1), one("working", "", "t8", 1), one("done", "waits on push", "t9", 1)];
        assert_eq!(through(&mut w, &looks, &|_| true), vec!["ops: done — pushed", "ops: done — waits on push"]);
        // so is a block, and a block again (a new since) as before
        let looks = [one("blocked", "allow?", "t10", 1), one("working", "", "t11", 1), one("blocked", "allow?", "t12", 1)];
        assert_eq!(through(&mut w, &looks, &|_| true), vec!["ops: blocked (permission) — allow?", "ops: blocked (permission) — allow?"]);
        // and the done line printed before the blocks is still the last one
        assert!(through(&mut w, &[one("working", "", "t13", 1), one("done", "waits on push", "t14", 1)], &|_| true).is_empty());
        // a plain watch started later doesn't print it either
        let mut w = Watch::new(Ledger::load(path.clone()), false);
        assert!(through(&mut w, &[one("done", "waits on push", "t15", 1)], &|_| true).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The same summary in two `cbar watch --next` runs: the second waits.
    #[test]
    fn the_same_done_across_next_runs_prints_once() {
        let path = std::env::temp_dir().join(format!("cbar-watch-same-next-{}/x.json", std::process::id()));
        let alive = |_: i64| true;
        let done = |since: &str, msg: &str| one("done", msg, since, 1);
        assert_eq!(next_run(&path, &mut vec![done("t1", "no agents"); 3].into_iter(), &alive), Some(vec!["ops: done — no agents".to_string()]));
        // a turn between the runs, and one while the next runs, both ending the same
        let mut looks = vec![done("t3", "no agents"), one("working", "", "t4", 1), done("t5", "no agents")].into_iter().cycle().take(60);
        assert_eq!(next_run(&path, &mut looks, &alive), None);
        // a new message wakes it
        assert_eq!(next_run(&path, &mut vec![done("t7", "one agent"); 3].into_iter(), &alive), Some(vec!["ops: done — one agent".to_string()]));
        // a new Claude in the pane (between runs) saying what the old one said last is news
        assert_eq!(next_run(&path, &mut vec![one("done", "one agent", "t9", 2); 3].into_iter(), &alive), Some(vec!["ops: done — one agent".to_string()]));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A stop after a repeat that wasn't printed is printed, and what the
    /// pane says when it comes back (or after it closed) is news again.
    #[test]
    fn a_stop_after_a_repeat_prints() {
        let path = std::env::temp_dir().join(format!("cbar-watch-same-stop-{}/x.json", std::process::id()));
        let mut w = Watch::new(Ledger::load(path.clone()), false);
        let looks = [one("working", "", "t0", 1), one("done", "ok", "t1", 1), one("working", "", "t2", 1), one("done", "ok", "t3", 1)];
        assert_eq!(through(&mut w, &looks, &|_| true), vec!["ops: done — ok"]);
        assert_eq!(through(&mut w, &[one("", "", "", 1)], &|_| false), vec!["ops: stopped"]);
        // a Claude started again says what it said: news
        assert_eq!(through(&mut w, &[one("working", "", "t4", 1), one("done", "ok", "t5", 1)], &|_| true), vec!["ops: done — ok"]);
        // closed, and a pane by the same id later: news too
        let gone = HashMap::new();
        assert_eq!(through(&mut w, &[gone.clone(), gone], &|_| true), vec!["ops: closed — ok"]);
        assert_eq!(through(&mut w, &[one("working", "", "t6", 1), one("done", "ok", "t7", 1)], &|_| true), vec!["ops: done — ok"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A ledger written by 0.5.6 (by pane, no last line): read, and a done
    /// it printed counts as the last line printed there.
    #[test]
    fn reads_a_0_5_6_ledger() {
        let path = std::env::temp_dir().join(format!("cbar-watch-056-{}/x.json", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = r#"{"%1": {"name": "ops", "state": "done", "kind": "", "msg": "ok", "since": "t1", "owner": 1, "printed": true},
                      "%2": {"name": "b", "state": "working", "kind": "", "msg": "", "since": "t1", "owner": 2, "printed": false}}"#;
        std::fs::write(&path, old).unwrap();
        let cur = HashMap::from([("%1".to_string(), report("ops", "done", "ok", "t2", Some(1))), ("%2".to_string(), report("b", "done", "fine", "t2", Some(2)))]);
        let mut w = Watch::new(Ledger::load(path.clone()), true);
        assert_eq!(through(&mut w, &[cur], &|_| true), vec!["b: done — fine"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("cbar-watch-guard-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        home.register_exchange("central", crate::mux::Term::Tmux, None).unwrap();
        home.register_project("central", "stats", &root, crate::mux::Term::Tmux).unwrap();
        home
    }

    fn pane(sid: &str, block: &str, tab: &str, state: &str, msg: &str, owner: i64) -> Pane {
        Pane {
            session_id: sid.into(),
            block_id: block.into(),
            window_id: format!("w{block}"),
            tab: tab.into(),
            label: "shell".into(),
            program: "claude".into(),
            cwd: "~/x".into(),
            state: state.into(),
            kind: String::new(),
            msg: msg.into(),
            since: "1m".into(),
            since_at: "t1".into(),
            owner: Some(owner),
        }
    }

    fn group(sid: &str, label: &str) -> Group {
        Group { session_id: sid.into(), label: label.into(), mux: "tmux".into(), window_id: None }
    }

    /// The exchange central (supervisor idle, one agent done) and its project
    /// stats (operator done, an agent done, one blocked, one working).
    fn snap() -> Snapshot {
        let me = std::process::id() as i64; // alive
        vec![
            (group("$0", "central"), vec![pane("$0", "%0", "supervisor", "idle", "", me), pane("$0", "%1", "notes", "done", "wrote it", me)]),
            (
                group("$1", "central/stats"),
                vec![
                    pane("$1", "%2", "operator", "done", "waiting on fix-x", me),
                    pane("$1", "%3", "fix-x", "done", "fixed", me),
                    pane("$1", "%4", "fix-y", "blocked", "allow?", me),
                    pane("$1", "%5", "fix-z", "working", "", me),
                ],
            ),
        ]
    }

    #[test]
    fn a_lease_is_held_while_its_watch_runs() {
        let home = home("lease");
        assert!(!watching(&home, "central/stats"));
        let lease = Lease::take(&home, "central/stats", "next").unwrap();
        assert!(watching(&home, "central/stats"));
        assert!(!watching(&home, "central"), "a lease is its session's only");
        let info: Value = serde_json::from_str(&std::fs::read_to_string(&lease.path).unwrap()).unwrap();
        assert_eq!((info["pid"].as_u64(), info["mode"].as_str()), (Some(std::process::id() as u64), Some("next")));
        assert!(lease.path.starts_with(home.root.join("watch/central+stats.live")));
        drop(lease);
        assert!(!watching(&home, "central/stats"));
        assert_eq!(std::fs::read_dir(lease_dir(&home, "central/stats")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    /// A lease left by a watch that was killed (its file, unlocked) is no
    /// watch, and is cleared; one still held stays.
    #[test]
    fn a_killed_watchs_lease_counts_for_nothing() {
        let home = home("stale");
        let dir = lease_dir(&home, "central");
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("99999.json");
        std::fs::write(&stale, r#"{"pid": 99999, "started": "t0", "mode": "next"}"#).unwrap();
        assert!(!watching(&home, "central"));
        assert!(!stale.exists());
        std::fs::write(&stale, "{}").unwrap();
        let live = Lease::take(&home, "central", "plain").unwrap();
        assert!(watching(&home, "central"));
        assert!(!stale.exists() && live.path.exists());
        drop(live);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    /// What waits for an operator's watch is what a --next started now would
    /// print; looking changes nothing, and once a watch printed it, nothing waits.
    #[test]
    fn waiting_is_what_a_next_would_print() {
        let home = home("waiting");
        let snap = snap();
        let got = waiting(&home, &snap, "$1", "central/stats", &["%2"]);
        assert_eq!(got, vec!["fix-x: done — fixed", "fix-y: blocked — allow?"]);
        assert!(!seen_file(&home, "central/stats").exists(), "looking saves no ledger");
        // a --next run prints them
        let mut w = Watch::new(Ledger::load(seen_file(&home, "central/stats")), true);
        let cur = reports(&snap, &home, "$1", "central/stats", &["%2"]);
        assert_eq!(pending(&mut w, &cur, &|_| true).len(), 2);
        assert!(waiting(&home, &snap, "$1", "central/stats", &["%2"]).is_empty());
        // the supervisor's: its own agent and its projects' operators, not their agents
        assert_eq!(waiting(&home, &snap, "$0", "central", &["%0"]), vec!["notes: done — wrote it", "stats operator: done — waiting on fix-x"]);
        let _ = std::fs::remove_dir_all(&home.root);
    }

    /// The flag: supervisors and operators running with no watch, never agents.
    #[test]
    fn flags_a_supervisor_or_operator_with_no_watch() {
        let home = home("unwatched");
        let snap = snap();
        let all = sessions(&snap);
        let got = unwatched(&home, &snap, &all);
        let mut keys: Vec<&String> = got.keys().collect();
        keys.sort();
        assert_eq!(keys, ["%0", "%2"]);
        assert_eq!(got["%2"].len(), 2);
        assert_eq!(got["%0"].len(), 2);
        // the operator starts a watch: only the supervisor is left
        let lease = Lease::take(&home, "central/stats", "next").unwrap();
        assert_eq!(unwatched(&home, &snap, &all).into_keys().collect::<Vec<_>>(), vec!["%0".to_string()]);
        drop(lease);
        // a Claude that isn't running isn't flagged, nor is a session crossbar didn't make
        let mut quiet = snap.clone();
        quiet[1].1[0].state.clear();
        quiet.push((group("$9", "scratch"), vec![pane("$9", "%9", "operator", "idle", "", 1)]));
        assert!(!unwatched(&home, &quiet, &sessions(&quiet)).contains_key("%2"));
        assert!(!unwatched(&home, &quiet, &sessions(&quiet)).contains_key("%9"));
        let _ = std::fs::remove_dir_all(&home.root);
    }

    /// The Stop hook lets the turn end when Claude Code is already going on
    /// for a Stop hook, and when a `cbar watch` it just started has no lease yet.
    #[test]
    fn the_stop_hook_blocks_once_a_turn() {
        let home = home("check");
        let g = group("$1", "central/stats");
        let again = serde_json::json!({"stop_hook_active": true, "background_tasks": []});
        assert_eq!(check(&home, &g, &again), None);
        let started = serde_json::json!({"stop_hook_active": false, "background_tasks": [
            {"id": "b1", "type": "shell", "status": "running", "description": "watch", "command": "cbar watch --next"}]});
        assert_eq!(check(&home, &g, &started), None);
        let lease = Lease::take(&home, "central/stats", "next").unwrap();
        assert_eq!(check(&home, &g, &serde_json::json!({"stop_hook_active": false})), None);
        drop(lease);
        let other = serde_json::json!({"background_tasks": [{"type": "shell", "status": "running", "command": "cargo test"}]});
        assert!(!starting(&other));
        assert_eq!(
            nudge("central/stats", &["fix-x: done — fixed".into(), "fix-y: blocked — allow?".into()]),
            "cbar watch isn't running for central/stats; 2 events are waiting: fix-x: done — fixed; fix-y: blocked — allow?. \
             Start `cbar watch --next` as a background Bash command, then act on what it prints."
        );
        assert!(nudge("central", &[]).starts_with("cbar watch isn't running for central; no events are waiting yet. Start"));
        let _ = std::fs::remove_dir_all(&home.root);
    }

    /// A new report without a message waits for one at startup too.
    #[test]
    fn startup_waits_for_a_message() {
        let path = std::env::temp_dir().join(format!("cbar-watch-quiet-{}/x.json", std::process::id()));
        let mut w = Watch::new(Ledger::load(path.clone()), true);
        let now = Instant::now();
        let bare = HashMap::from([("%1".to_string(), report("a", "done", "", "t1", None))]);
        assert!(w.look(&bare, now, &|_| true).is_empty());
        let said = HashMap::from([("%1".to_string(), report("a", "done", "ok", "t1", None))]);
        assert_eq!(events(w.look(&said, now, &|_| true)), vec!["a: done — ok"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
