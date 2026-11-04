//! switchboard --watch: a line each time one of the caller's reports turns
//! done, blocked or error (with its message), stops running Claude, or
//! closes. The reports of a project's operator are the agents in its
//! session's tabs; an exchange's supervisor also hears from the operators of
//! the exchange's projects.
//!
//! At startup it prints every report that is blocked or in error now, and a
//! report that is done only if no earlier watch of the same session printed
//! that already: what each watch printed is kept in
//! ~/.crossbar/watch/<session>.json, so a watch restarted after a crash or a
//! sleep misses nothing and repeats no old news.

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

/// What a watch last printed about a report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Seen {
    state: String,
    msg: String,
    since: String,
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
                } else if label == "central" && !g.label.contains(SEP) {
                    add(format!("{} operator", g.label), p);
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
}

impl Ledger {
    fn load(path: PathBuf) -> Ledger {
        let seen = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Ledger { path, seen }
    }

    fn save(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string_pretty(&self.seen).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn printed(&mut self, r: &Report) {
        self.seen.insert(r.name.clone(), Seen { state: r.state.clone(), msg: r.msg.clone(), since: r.since.clone() });
        self.save();
    }

    fn forget(&mut self, name: &str) {
        if self.seen.remove(name).is_some() {
            self.save();
        }
    }

    /// Whether a watch already printed R as it is now.
    fn knows(&self, r: &Report) -> bool {
        self.seen.get(&r.name).is_some_and(|s| s.state == r.state && s.msg == r.msg && s.since == r.since)
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

    /// The events of the look CUR at NOW: (report, the state to print).
    /// ALIVE says whether a pid still runs.
    fn step(&mut self, cur: &HashMap<String, Report>, now: Instant, alive: &dyn Fn(i64) -> bool) -> Vec<(Report, String)> {
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
                        events.push((r.clone(), "stopped".into()));
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
                    events.push((r.clone(), r.state.clone()));
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
                events.push((r.clone(), r.state.clone()));
            }
        }
        let gone: Vec<String> = self.last.keys().filter(|k| !cur.contains_key(*k)).cloned().collect();
        for key in gone {
            let n = self.missing.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n >= 2 {
                let r = self.last[&key].clone();
                if !first {
                    events.push((r, "closed".into()));
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

/// Watch forever, printing events (see Tracker).
pub fn watch(home: &Home, scope: &str, label: &str) {
    let every = Duration::from_secs(3);
    let look = Looker::new(true);
    let mut ledger = Ledger::load(seen_file(home, label));
    let mut out = std::io::stdout();
    let mut say = |ledger: &mut Ledger, r: &Report, state: &str| {
        if writeln!(out, "{}", line(&r.name, state, &r.kind, &r.msg)).and_then(|_| out.flush()).is_err() {
            std::process::exit(0); // nobody is listening
        }
        match state {
            "closed" | "stopped" => ledger.forget(&r.name),
            _ => ledger.printed(r),
        }
    };
    let mut tracker = Tracker::new(Duration::from_secs(8));
    let alive = |pid: i64| util::alive(&pid.to_string());
    loop {
        let now = Instant::now();
        let cur = reports(&look, home, scope, label);
        if tracker.first {
            // what stands now: blocked and error always, the rest if new
            let mut now_reports: Vec<&Report> = cur.values().filter(|r| REPORTABLE.contains(&r.state.as_str())).collect();
            now_reports.sort_by(|a, b| a.name.cmp(&b.name));
            for r in now_reports {
                if STANDING.contains(&r.state.as_str()) || !ledger.knows(r) {
                    say(&mut ledger, r, &r.state);
                }
            }
        }
        for (r, state) in tracker.step(&cur, now, &alive) {
            say(&mut ledger, &r, &state);
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

    fn events(e: Vec<(Report, String)>) -> Vec<String> {
        let mut v: Vec<String> = e.into_iter().map(|(r, s)| line(&r.name, &s, &r.kind, &r.msg)).collect();
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
        l.printed(&r);
        let l2 = Ledger::load(path.clone());
        assert!(l2.knows(&r));
        assert!(!l2.knows(&Report { since: "t2".into(), ..r.clone() }));
        assert_eq!(line("a", "error", "api", "boom"), "a: error (api) — boom");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
