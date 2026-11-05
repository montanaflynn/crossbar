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

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::layout::{Home, OPERATOR, SEP};
use crate::status::{Looker, Pane};
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
}

fn yes() -> bool {
    true
}

/// {pane id: report} for the panes reporting to whoever runs in session
/// SCOPE (id) labelled LABEL. Switchboards and our own pane are left out.
fn reports(look: &Looker, home: &Home, scope: &str, label: &str) -> HashMap<String, Report> {
    let mut found = HashMap::new();
    let mut add = |name: String, p: &Pane| {
        if p.program != "switchboard" && p.label != "switchboard" {
            let r = Report { name, state: p.state.clone(), kind: p.kind.clone(), msg: p.msg.clone(), since: p.since_at.clone(), owner: p.owner };
            found.insert(p.block_id.clone(), r);
        }
    };
    let name_of = |p: &Pane| if p.tab.is_empty() { p.program.clone() } else { p.tab.clone() };
    // an exchange's session
    if !home.exchange_exists(label) {
        for (_, panes) in look.snapshot(Some(scope)) {
            for p in &panes {
                add(name_of(p), p);
            }
        }
        return found;
    }
    let prefix = format!("{label}{SEP}");
    for (g, panes) in look.snapshot(None) {
        for p in &panes {
            if g.session_id == scope {
                add(name_of(p), p);
            } else if p.tab == OPERATOR {
                if let Some(project) = g.label.strip_prefix(&prefix) {
                    add(format!("{project} operator"), p);
                }
            }
        }
    }
    found
}

/// Where a watch of session LABEL keeps what it printed.
fn seen_file(home: &Home, label: &str) -> PathBuf {
    home.root.join("watch").join(format!("{}.json", label.replace(SEP, "+")))
}

struct Ledger {
    path: PathBuf,
    seen: HashMap<String, Seen>,
    /// What the file holds.
    saved: HashMap<String, Seen>,
}

impl Ledger {
    fn load(path: PathBuf) -> Ledger {
        let seen: HashMap<String, Seen> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Ledger { path, saved: seen.clone(), seen }
    }

    fn save(&mut self) {
        self.saved = self.seen.clone();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string_pretty(&self.seen).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn entry(r: &Report, printed: bool) -> Seen {
        Seen { name: r.name.clone(), state: r.state.clone(), kind: r.kind.clone(), msg: r.msg.clone(), since: r.since.clone(), owner: r.owner, printed }
    }

    /// A watch printed R (in pane KEY) as it is now.
    fn printed(&mut self, key: &str, r: &Report) {
        self.seen.insert(key.to_string(), Ledger::entry(r, true));
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
    /// that changed. A report that stopped or closed is dropped.
    fn sync(&mut self, last: &HashMap<String, Report>) {
        let now: HashMap<String, Seen> = last.iter().filter(|(_, r)| !r.state.is_empty()).map(|(k, r)| (k.clone(), Ledger::entry(r, self.knows(r)))).collect();
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
pub fn watch(home: &Home, scope: &str, label: &str, next: bool) {
    let every = Duration::from_secs(3);
    let look = Looker::new(true);
    let mut w = Watch::new(Ledger::load(seen_file(home, label)), next);
    let mut out = std::io::stdout();
    let alive = |pid: i64| util::alive(&pid.to_string());
    let mut last_look = false;
    loop {
        let cur = reports(&look, home, scope, label);
        let events = w.look(&cur, Instant::now(), &alive);
        for (_, r, state) in &events {
            if writeln!(out, "{}", line(&r.name, state, &r.kind, &r.msg)).and_then(|_| out.flush()).is_err() {
                std::process::exit(0); // nobody is listening
            }
        }
        w.record(&events);
        if next && (last_look || !events.is_empty()) {
            if last_look {
                std::process::exit(0);
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
