//! Every pane of every Rex and tmux session, with its program status.
//!
//! Rex reports Claude's status (OSC 7501) per block. In tmux, a Claude
//! started through claude-relay reports it to its pane's options @cbar_state,
//! @cbar_kind, @cbar_msg and @cbar_since, the relay's pid in @cbar_relay (a
//! dead relay's status is stale). Any other Claude's comes from `claude agents`,
//! matched to the pane whose process it descends from: idle, working or
//! blocked, with no message. Switchboards mark their own pane with @cbar_role
//! (main or local) and, for a main board, @cbar_exchange; cbar labels panes
//! it makes with @cbar_label.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::mux::{self, rex_json};
use crate::util::{self, short};

pub const ORDER: [&str; 5] = ["blocked", "error", "done", "working", "idle"];

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Pane {
    pub session_id: String,
    pub block_id: String,
    pub window_id: String,
    pub tab: String,
    pub label: String,
    pub program: String,
    pub cwd: String,
    pub state: String,
    pub kind: String,
    pub msg: String,
    pub since: String,
}

/// A card: a session, or (scoped to one session) a tab.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Group {
    pub session_id: String,
    pub label: String,
    pub mux: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
}

pub type Snapshot = Vec<(Group, Vec<Pane>)>;

/// What to leave out (our own pane) and what to remember between looks.
pub struct Looker {
    pub hide: HashSet<String>,
    me: u32,
    since: Mutex<HashMap<String, (String, String)>>, // tmux pane → (state, first seen)
}

impl Default for Looker {
    fn default() -> Self {
        Self::new(true)
    }
}

pub fn rank(state: &str) -> usize {
    ORDER.iter().position(|s| *s == state).unwrap_or(ORDER.len())
}

/// Run F over ITEMS on up to 16 threads, keeping the order.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..items.len().min(16) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

const TMUX_FIELDS: [&str; 16] = [
    "session_id",
    "session_name",
    "window_id",
    "window_name",
    "pane_id",
    "pane_pid",
    "pane_current_command",
    "pane_current_path",
    "@cbar_label",
    "@cbar_role",
    "@cbar_state",
    "@cbar_kind",
    "@cbar_msg",
    "@cbar_since",
    "@cbar_relay",
    "@cbar_exchange",
];

pub type Row = HashMap<&'static str, String>;

pub fn tmux_rows() -> Vec<Row> {
    let fmt = TMUX_FIELDS.iter().map(|f| format!("#{{{f}}}")).collect::<Vec<_>>().join("\t");
    let (code, out, _) = mux::tmux_s(&["list-panes", "-a", "-F", &fmt]);
    if code != 0 {
        return Vec::new();
    }
    out.lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|r| r.len() == TMUX_FIELDS.len())
        .map(|r| TMUX_FIELDS.iter().copied().zip(r.into_iter().map(String::from)).collect())
        .collect()
}

/// {pane pid: Claude session from `claude agents`} for panes running Claude.
fn claude_by_pane(pane_pids: &HashSet<i64>) -> HashMap<i64, Value> {
    if pane_pids.is_empty() {
        return HashMap::new();
    }
    let (_, out, _) = util::run("claude", &util::strings(&["agents", "--json"]), Duration::from_secs(5));
    let Ok(Value::Array(sessions)) = serde_json::from_str::<Value>(if out.trim().is_empty() { "[]" } else { &out }) else {
        return HashMap::new();
    };
    let (_, ps, _) = util::run("ps", &util::strings(&["-A", "-o", "pid=,ppid="]), Duration::from_secs(5));
    let mut parent = HashMap::new();
    for line in ps.lines() {
        let mut it = line.split_whitespace();
        if let (Some(a), Some(b)) = (it.next(), it.next()) {
            if let (Ok(a), Ok(b)) = (a.parse::<i64>(), b.parse::<i64>()) {
                parent.insert(a, b);
            }
        }
    }
    let mut found = HashMap::new();
    for s in sessions {
        let mut pid = s["pid"].as_i64();
        let mut hops = 0;
        while let Some(p) = pid {
            if pane_pids.contains(&p) || hops >= 16 {
                break;
            }
            pid = parent.get(&p).copied();
            hops += 1;
        }
        if let Some(p) = pid.filter(|p| pane_pids.contains(p)) {
            found.entry(p).or_insert(s);
        }
    }
    found
}

/// The argv of process PID, from ps (Rex reports argv0 only).
pub fn argv_of(pid: i64) -> Vec<String> {
    let (_, out, _) = util::run("ps", &util::strings(&["-o", "args=", "-p", &pid.to_string()]), Duration::from_secs(5));
    out.split_whitespace().map(String::from).collect()
}

/// What a switchboard's ARGV makes it: None if it isn't one, Some(None) for
/// a local board, Some(Some(EXCHANGE)) for a main board ("*" for --all, ""
/// for the default exchange).
pub fn board_kind(argv: &[String]) -> Option<Option<String>> {
    let i = argv.iter().position(|a| util::basename(a) == "switchboard")?;
    let rest = &argv[i + 1..];
    if rest.iter().any(|a| a == "--here" || a == "--session" || a.starts_with("--session=")) {
        return Some(None);
    }
    if rest.iter().any(|a| a == "--all") {
        return Some(Some("*".into()));
    }
    let ex = rest.iter().position(|a| a == "--exchange").and_then(|k| rest.get(k + 1)).cloned();
    Some(Some(ex.unwrap_or_default()))
}

impl Looker {
    /// HIDE_SELF leaves our own pane out (as the board does; --json shows it).
    pub fn new(hide_self: bool) -> Looker {
        let hide = if hide_self {
            [util::env("REX_BLOCK"), util::env("TMUX_PANE")].into_iter().flatten().collect()
        } else {
            HashSet::new()
        };
        Looker { hide, me: std::process::id(), since: Mutex::new(HashMap::new()) }
    }

    fn rex_pane(&self, sid: &str, block: &Value, tabs: &HashMap<String, String>) -> Option<Pane> {
        let bid = block["block_id"].as_str()?.to_string();
        if self.hide.contains(&bid) {
            return None;
        }
        let proc = rex_json(&["block", "call", "-s", sid, "-b", &bid, "process"]).unwrap_or(Value::Null);
        let fg = &proc["foreground"];
        if fg["pid"].as_u64() == Some(self.me as u64) {
            return None;
        }
        let status = rex_json(&["block", "call", "-s", sid, "-b", &bid, "program_status"]).unwrap_or(Value::Null);
        let records = status["records"].as_array().cloned().unwrap_or_default();
        let root = records
            .iter()
            .find(|r| r["id"].as_str().is_none_or(|s| s.is_empty()))
            .or(records.first())
            .cloned()
            .unwrap_or(Value::Null);
        // a status left by a program that has exited is stale
        let root = match root["owner"]["pid"].as_i64() {
            Some(pid) if !util::alive(&pid.to_string()) => Value::Null,
            _ => root,
        };
        let program = [&fg["script"], &fg["argv0"], &fg["name"]]
            .iter()
            .filter_map(|v| v.as_str())
            .find(|s| !s.is_empty())
            .map(|s| util::basename(s).trim_start_matches('-').to_string())
            .unwrap_or_else(|| "?".into());
        let wid = block["window_id"].as_str().unwrap_or("").to_string();
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        Some(Pane {
            session_id: sid.to_string(),
            block_id: bid,
            tab: tabs.get(&wid).cloned().unwrap_or_default(),
            window_id: wid,
            label: s(&block["label"]),
            program,
            cwd: short(fg["cwd"].as_str().unwrap_or("")),
            state: s(&root["state"]),
            kind: s(&root["kind"]),
            msg: util::text(root["msg"].as_str().unwrap_or("")),
            since: util::age(root["updated_at"].as_str().unwrap_or("")),
        })
    }

    /// One Rex session's windows (id, label) and placed blocks.
    fn rex_layout(sid: &str) -> (Vec<(String, String)>, Vec<Value>) {
        let windows = rex_json(&["window", "ls", "-s", sid, "--json"])
            .and_then(|v| v["windows"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|w| (w["window_id"].as_str().unwrap_or("").to_string(), w["label"].as_str().unwrap_or("").to_string()))
            .collect();
        let blocks = rex_json(&["block", "ls", "-s", sid, "--json"])
            .and_then(|v| v["blocks"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b["placement"].as_str().unwrap_or("placed") == "placed")
            .collect();
        (windows, blocks)
    }

    pub fn rex_snapshot(&self) -> Snapshot {
        let sessions = mux::rex_sessions();
        let layouts = parallel(&sessions, |(sid, _)| Self::rex_layout(sid));
        let mut jobs = Vec::new();
        for ((sid, _), (windows, blocks)) in sessions.iter().zip(&layouts) {
            let tabs: HashMap<String, String> = windows.iter().cloned().collect();
            for b in blocks {
                jobs.push((sid.clone(), b.clone(), tabs.clone()));
            }
        }
        let panes = parallel(&jobs, |(sid, b, tabs)| self.rex_pane(sid, b, tabs));
        let mut out: Snapshot = sessions
            .iter()
            .map(|(sid, label)| (Group { session_id: sid.clone(), label: label.clone(), mux: "rex".into(), window_id: None }, Vec::new()))
            .collect();
        for ((sid, _, _), p) in jobs.iter().zip(panes) {
            if let (Some(p), Some(g)) = (p, out.iter_mut().find(|(g, _)| &g.session_id == sid)) {
                g.1.push(p);
            }
        }
        out
    }

    /// One Rex session, grouped by tab, in tab order.
    pub fn rex_tabs(&self, sid: &str, label: &str) -> Snapshot {
        let (windows, blocks) = Self::rex_layout(sid);
        let tabs: HashMap<String, String> = windows.iter().cloned().collect();
        let panes = parallel(&blocks, |b| self.rex_pane(sid, b, &tabs));
        let _ = label;
        windows
            .iter()
            .map(|(wid, wl)| {
                let g = Group { session_id: sid.to_string(), label: wl.clone(), mux: "rex".into(), window_id: Some(wid.clone()) };
                (g, panes.iter().flatten().filter(|p| &p.window_id == wid).cloned().collect())
            })
            .collect()
    }

    fn tmux_pane(&self, row: &Row, claude: &HashMap<i64, Value>) -> Option<Pane> {
        if self.hide.contains(&row["pane_id"]) {
            return None;
        }
        let s = row["pane_pid"].parse::<i64>().ok().and_then(|p| claude.get(&p));
        let (mut state, mut kind, mut msg, mut since) =
            (row["@cbar_state"].clone(), row["@cbar_kind"].clone(), util::text(&row["@cbar_msg"]), row["@cbar_since"].clone());
        if !row["@cbar_relay"].is_empty() && !util::alive(&row["@cbar_relay"]) {
            (state, kind, msg, since) = Default::default();
        }
        let relayed = !state.is_empty();
        if state.is_empty() {
            if let Some(s) = s {
                let st = s["state"].as_str().unwrap_or("");
                state = if ORDER.contains(&st) {
                    st.to_string()
                } else {
                    match s["status"].as_str().unwrap_or("") {
                        "busy" => "working",
                        "waiting" => "blocked",
                        "idle" => "idle",
                        _ => "",
                    }
                    .to_string()
                };
                let mut seen = self.since.lock().unwrap();
                let first = !seen.contains_key(&row["pane_id"]);
                let entry = seen.entry(row["pane_id"].clone()).or_insert((state.clone(), String::new()));
                if entry.0 != state {
                    *entry = (state.clone(), if first { String::new() } else { util::utc_iso_now() });
                }
                since = entry.1.clone();
            }
        }
        let role = &row["@cbar_role"];
        Some(Pane {
            session_id: row["session_id"].clone(),
            block_id: row["pane_id"].clone(),
            window_id: row["window_id"].clone(),
            tab: row["window_name"].clone(),
            label: if !row["@cbar_label"].is_empty() {
                row["@cbar_label"].clone()
            } else if !role.is_empty() {
                "switchboard".into()
            } else {
                "shell".into()
            },
            program: if s.is_some() || relayed {
                "claude".into()
            } else if !role.is_empty() {
                "switchboard".into()
            } else {
                row["pane_current_command"].clone()
            },
            cwd: short(&row["pane_current_path"]),
            state,
            kind,
            msg,
            since: util::age(&since),
        })
    }

    /// Every tmux session, or with SCOPE (a session id) its tabs.
    pub fn tmux_snapshot(&self, scope: Option<&str>) -> Snapshot {
        let rows = tmux_rows();
        let pids: HashSet<i64> = rows.iter().filter_map(|r| r["pane_pid"].parse().ok()).collect();
        let claude = claude_by_pane(&pids);
        let mut out: Snapshot = Vec::new();
        for r in &rows {
            let (key, g) = match scope {
                Some(sc) if r["session_id"] != sc => continue,
                Some(_) => (
                    r["window_id"].clone(),
                    Group { session_id: r["session_id"].clone(), label: r["window_name"].clone(), mux: "tmux".into(), window_id: Some(r["window_id"].clone()) },
                ),
                None => (
                    r["session_id"].clone(),
                    Group { session_id: r["session_id"].clone(), label: r["session_name"].clone(), mux: "tmux".into(), window_id: None },
                ),
            };
            let idx = match out.iter().position(|(g, _)| g.window_id.as_deref().unwrap_or(&g.session_id) == key) {
                Some(i) => i,
                None => {
                    out.push((g, Vec::new()));
                    out.len() - 1
                }
            };
            if let Some(p) = self.tmux_pane(r, &claude) {
                out[idx].1.push(p);
            }
        }
        out
    }

    /// Every session (Rex, then tmux), or with SCOPE (a session id) its tabs.
    pub fn snapshot(&self, scope: Option<&str>) -> Snapshot {
        match scope {
            Some(s) if mux::is_tmux_id(s) => self.tmux_snapshot(Some(s)),
            Some(s) => self.rex_tabs(s, ""),
            None => {
                let mut all = self.rex_snapshot();
                all.extend(self.tmux_snapshot(None));
                all
            }
        }
    }
}

/// The session NAME (a label or id), Rex or tmux, as a group; a name in both
/// is taken from the terminal this process runs in.
pub fn resolve(name: &str) -> Option<Group> {
    let rex = || mux::rex_sessions().into_iter().map(|(i, l)| Group { session_id: i, label: l, mux: "rex".into(), window_id: None });
    let tmux = || mux::tmux_sessions().into_iter().map(|(i, l)| Group { session_id: i, label: l, mux: "tmux".into(), window_id: None });
    let pool: Vec<Group> = if util::env("TMUX_PANE").is_some() { tmux().chain(rex()).collect() } else { rex().chain(tmux()).collect() };
    pool.into_iter().find(|g| g.session_id == name || g.label == name)
}

/// A pane flattened with its group, as `cbar` uses them.
#[derive(Clone, Debug)]
pub struct Flat {
    pub mux: String,
    pub group: String,
    pub pane: Pane,
}

pub fn flatten(snap: Snapshot) -> Vec<Flat> {
    snap.into_iter()
        .flat_map(|(g, ps)| ps.into_iter().map(move |p| Flat { mux: g.mux.clone(), group: g.label.clone(), pane: p }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_boards_apart() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(board_kind(&v("/x/switchboard")), Some(Some(String::new())));
        assert_eq!(board_kind(&v("python3 /x/switchboard")), Some(Some(String::new())));
        assert_eq!(board_kind(&v("/x/switchboard --here")), Some(None));
        assert_eq!(board_kind(&v("switchboard --exchange work")), Some(Some("work".into())));
        assert_eq!(board_kind(&v("switchboard --all")), Some(Some("*".into())));
        assert_eq!(board_kind(&v("/bin/zsh")), None);
    }

    #[test]
    fn keeps_order_in_parallel() {
        let v: Vec<u32> = (0..50).collect();
        assert_eq!(parallel(&v, |x| x * 2), (0..50).map(|x| x * 2).collect::<Vec<_>>());
    }
}
