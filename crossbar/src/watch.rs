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
            let r = Report { name, state: p.state.clone(), kind: p.kind.clone(), msg: p.msg.clone(), since: p.since_at.clone() };
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

/// Watch forever, printing events. A report without a message waits up to
/// WAIT_MSG for one (Rex sends the summary just after the state); a pane
/// must be missing from two looks in a row to count as closed.
pub fn watch(home: &Home, scope: &str, label: &str) {
    let every = Duration::from_secs(3);
    let wait_msg = Duration::from_secs(8);
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
    let mut last: HashMap<String, Report> = HashMap::new();
    let mut pending: HashMap<String, Instant> = HashMap::new();
    let mut missing: HashMap<String, u32> = HashMap::new();
    let mut first = true;
    loop {
        let now = Instant::now();
        let cur = reports(&look, home, scope, label);
        if first {
            // what stands now: blocked and error always, the rest if new
            let mut now_reports: Vec<&Report> = cur.values().filter(|r| REPORTABLE.contains(&r.state.as_str())).collect();
            now_reports.sort_by(|a, b| a.name.cmp(&b.name));
            for r in now_reports {
                if STANDING.contains(&r.state.as_str()) || !ledger.knows(r) {
                    say(&mut ledger, r, &r.state);
                }
            }
        }
        for (key, r) in &cur {
            missing.remove(key);
            let prev = last.get(key).cloned();
            if r.state.is_empty() && prev.is_none() {
                continue; // not an agent (yet)
            }
            last.insert(key.clone(), r.clone());
            if first || prev.as_ref().is_some_and(|p| (&p.state, &p.msg) == (&r.state, &r.msg)) {
                continue;
            }
            pending.remove(key);
            if REPORTABLE.contains(&r.state.as_str()) {
                if !r.msg.is_empty() || prev.as_ref().is_some_and(|p| p.state == r.state) {
                    say(&mut ledger, r, &r.state);
                } else {
                    pending.insert(key.clone(), now);
                }
            } else if r.state.is_empty() && prev.as_ref().is_some_and(|p| !p.state.is_empty()) {
                say(&mut ledger, r, "stopped");
            }
        }
        for (key, since) in pending.clone() {
            let r = last[&key].clone();
            if !REPORTABLE.contains(&r.state.as_str()) {
                pending.remove(&key);
            } else if !r.msg.is_empty() || now.duration_since(since) >= wait_msg {
                pending.remove(&key);
                say(&mut ledger, &r, &r.state);
            }
        }
        let gone: Vec<String> = last.keys().filter(|k| !cur.contains_key(*k)).cloned().collect();
        for key in gone {
            let n = missing.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n >= 2 {
                let r = last[&key].clone();
                if !first {
                    say(&mut ledger, &r, "closed");
                }
                last.remove(&key);
                missing.remove(&key);
                pending.remove(&key);
            }
        }
        first = false;
        std::thread::sleep(every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_what_it_printed() {
        let path = std::env::temp_dir().join(format!("cbar-watch-{}/x.json", std::process::id()));
        let r = Report { name: "fix-x".into(), state: "done".into(), kind: "".into(), msg: "ok".into(), since: "t1".into() };
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
