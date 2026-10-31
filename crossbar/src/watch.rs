//! switchboard --watch: a line each time one of the caller's reports turns
//! done, blocked or error (with its message), stops running Claude, or
//! closes. The reports of a project's operator are the agents in its
//! session's tabs; an exchange's supervisor also hears from the operators of
//! the exchange's projects.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::layout::{Home, OPERATOR, SEP};
use crate::status::{Looker, Pane};

const REPORTABLE: [&str; 3] = ["done", "blocked", "error"];

type Report = (String, String, String, String); // name, state, kind, msg

/// {pane id: report} for the panes reporting to whoever runs in session
/// SCOPE (id) labelled LABEL. Switchboards and our own pane are left out.
fn reports(look: &Looker, home: &Home, scope: &str, label: &str) -> HashMap<String, Report> {
    let mut found = HashMap::new();
    let mut add = |name: String, p: &Pane| {
        if p.program != "switchboard" && p.label != "switchboard" {
            found.insert(p.block_id.clone(), (name, p.state.clone(), p.kind.clone(), p.msg.clone()));
        }
    };
    // an exchange's session (or, in a 0.4 layout, `central`)
    let exchange = home.exchange_exists(label) || (label == "central" && home.needs_migrate());
    if !exchange {
        for (_, panes) in look.snapshot(Some(scope)) {
            for p in &panes {
                add(if p.tab.is_empty() { p.program.clone() } else { p.tab.clone() }, p);
            }
        }
        return found;
    }
    let prefix = format!("{label}{SEP}");
    for (g, panes) in look.snapshot(None) {
        for p in &panes {
            if g.session_id == scope {
                add(if p.tab.is_empty() { p.program.clone() } else { p.tab.clone() }, p);
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

/// Watch forever, printing events. Nothing at startup. A report without a
/// message waits up to WAIT_MSG for one (Rex sends the summary just after
/// the state); a pane must be missing from two looks in a row to count as
/// closed.
pub fn watch(home: &Home, scope: &str, label: &str) {
    let every = Duration::from_secs(3);
    let wait_msg = Duration::from_secs(8);
    let look = Looker::new(true);
    let mut out = std::io::stdout();
    let mut say = |name: &str, state: &str, kind: &str, msg: &str| {
        let kind = if kind.is_empty() { String::new() } else { format!(" ({kind})") };
        let msg = if msg.is_empty() { String::new() } else { format!(" — {msg}") };
        if writeln!(out, "{name}: {state}{kind}{msg}").and_then(|_| out.flush()).is_err() {
            std::process::exit(0); // nobody is listening
        }
    };
    let mut last: HashMap<String, Report> = HashMap::new();
    let mut pending: HashMap<String, Instant> = HashMap::new();
    let mut missing: HashMap<String, u32> = HashMap::new();
    let mut first = true;
    loop {
        let now = Instant::now();
        let cur = reports(&look, home, scope, label);
        for (key, r) in &cur {
            missing.remove(key);
            let prev = last.get(key).cloned();
            if r.1.is_empty() && prev.is_none() {
                continue; // not an agent (yet)
            }
            last.insert(key.clone(), r.clone());
            if first || prev.as_ref().is_some_and(|p| (&p.1, &p.3) == (&r.1, &r.3)) {
                continue;
            }
            pending.remove(key);
            if REPORTABLE.contains(&r.1.as_str()) {
                if !r.3.is_empty() || prev.as_ref().is_some_and(|p| p.1 == r.1) {
                    say(&r.0, &r.1, &r.2, &r.3);
                } else {
                    pending.insert(key.clone(), now);
                }
            } else if r.1.is_empty() && prev.as_ref().is_some_and(|p| !p.1.is_empty()) {
                say(&r.0, "stopped", "", "");
            }
        }
        for (key, since) in pending.clone() {
            let r = &last[&key];
            if !REPORTABLE.contains(&r.1.as_str()) {
                pending.remove(&key);
            } else if !r.3.is_empty() || now.duration_since(since) >= wait_msg {
                pending.remove(&key);
                say(&r.0, &r.1, &r.2, &r.3);
            }
        }
        let gone: Vec<String> = last.keys().filter(|k| !cur.contains_key(*k)).cloned().collect();
        for key in gone {
            let n = missing.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n >= 2 {
                if !first {
                    let name = last[&key].0.clone();
                    say(&name, "closed", "", "");
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
