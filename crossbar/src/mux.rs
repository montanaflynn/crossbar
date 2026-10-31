//! The two terminals crossbar drives: Rex (through the `rex` CLI) and tmux
//! (on $CROSSBAR_TMUX_SOCKET's server if set, tmux -L).
//!
//! Panes are always started with their program as the pane's own command,
//! never by typing shell syntax into a shell, so any login shell works.

use std::time::Duration;

use serde_json::Value;

use crate::util::{self, strings};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Rex,
    Tmux,
}

impl Term {
    pub fn parse(s: &str) -> Option<Term> {
        match s {
            "rex" => Some(Term::Rex),
            "tmux" => Some(Term::Tmux),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Term::Rex => "rex",
            Term::Tmux => "tmux",
        }
    }
}

pub fn rex(args: &[String]) -> (i32, String, String) {
    util::run("rex", args, TIMEOUT)
}

pub fn rex_json(args: &[&str]) -> Option<Value> {
    let (code, out, _) = rex(&strings(args));
    if code != 0 || out.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&out).ok()
}

pub fn tmux_socket() -> Option<String> {
    util::env("CROSSBAR_TMUX_SOCKET")
}

/// tmux's argv prefix: tmux [-L SOCKET].
pub fn tmux_argv() -> Vec<String> {
    let mut v = vec!["tmux".to_string()];
    if let Some(s) = tmux_socket() {
        v.push("-L".into());
        v.push(s);
    }
    v
}

pub fn tmux(args: &[String]) -> (i32, String, String) {
    let mut v = tmux_argv();
    let program = v.remove(0);
    v.extend_from_slice(args);
    util::run(&program, &v, TIMEOUT)
}

pub fn tmux_s(args: &[&str]) -> (i32, String, String) {
    tmux(&strings(args))
}

/// The terminal this process runs in: tmux, Rex or neither.
pub fn here_terminal() -> Option<Term> {
    if util::env("TMUX_PANE").is_some() {
        Some(Term::Tmux)
    } else if util::env("REX_SESSION").is_some() {
        Some(Term::Rex)
    } else {
        None
    }
}

/// Rex sessions as (id, label).
pub fn rex_sessions() -> Vec<(String, String)> {
    let Some(v) = rex_json(&["ls", "--json"]) else { return Vec::new() };
    v["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| (s["session_id"].as_str().unwrap_or("").to_string(), s["label"].as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The id of the Rex session labelled (or with id) NAME.
pub fn rex_session_id(name: &str) -> Option<String> {
    rex_sessions().into_iter().find(|(id, label)| label == name || id == name).map(|(id, _)| id)
}

pub fn tmux_has(name: &str) -> bool {
    tmux_s(&["has-session", "-t", &format!("={name}")]).0 == 0
}

/// tmux sessions as (id, name).
pub fn tmux_sessions() -> Vec<(String, String)> {
    let (code, out, _) = tmux_s(&["list-sessions", "-F", "#{session_id}\t#{session_name}"]);
    if code != 0 {
        return Vec::new();
    }
    out.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

/// This pane's session name.
pub fn here_session() -> Option<String> {
    match here_terminal()? {
        Term::Tmux => {
            let pane = util::env("TMUX_PANE")?;
            let (code, out, _) = tmux_s(&["display-message", "-p", "-t", &pane, "#{session_name}"]);
            (code == 0 && !out.trim().is_empty()).then(|| out.trim().to_string())
        }
        Term::Rex => {
            let id = util::env("REX_SESSION")?;
            rex_sessions().into_iter().find(|(sid, _)| sid == &id).map(|(_, l)| l)
        }
    }
}

/// This pane's id: $TMUX_PANE or $REX_BLOCK.
pub fn here_pane() -> Option<String> {
    util::env("TMUX_PANE").or_else(|| util::env("REX_BLOCK"))
}

pub fn session_running(name: &str, term: Term) -> bool {
    match term {
        Term::Tmux => tmux_has(name),
        Term::Rex => rex_session_id(name).is_some(),
    }
}

/// The Rex app to drive: prefer an app attached to our own session.
pub fn app_client() -> Option<String> {
    let v = rex_json(&["client", "ls", "--json"])?;
    let mine = util::env("REX_SESSION").unwrap_or_default();
    let mut apps: Vec<&Value> = v["clients"]
        .as_array()?
        .iter()
        .filter(|c| c["info"]["kind"] == "app" && c["connection_state"].as_str().unwrap_or("connected") == "connected")
        .collect();
    apps.sort_by_key(|c| !c["session_ids"].as_array().is_some_and(|ids| ids.iter().any(|i| i == mine.as_str())));
    apps.first().and_then(|c| c["client_id"].as_str()).map(String::from)
}

/// A pane created with its own command: its id, and its tab's id (Rex).
pub struct Made {
    pub pane: String,
}

fn first_block(v: &Value) -> Option<String> {
    v["initial_windows"][0]["block_ids"][0]
        .as_str()
        .or_else(|| v["block_ids"][0].as_str())
        .or_else(|| v["blocks"][0]["block_id"].as_str())
        .map(String::from)
}

/// Create session NAME in DIR with first tab TAB running ARGV.
pub fn new_session(term: Term, name: &str, dir: &str, tab: &str, argv: &[String]) -> Result<Made, String> {
    match term {
        Term::Rex => {
            let mut args = strings(&["new", name, "-c", dir, "--window", tab, "--json", "--"]);
            args.extend_from_slice(argv);
            let (code, out, err) = rex(&args);
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            match first_block(&v) {
                Some(pane) if code == 0 => Ok(Made { pane }),
                _ => Err(if err.is_empty() { format!("could not create session '{name}'") } else { err }),
            }
        }
        Term::Tmux => {
            let mut args = strings(&["new-session", "-d", "-s", name, "-c", dir, "-n", tab, "-x", "200", "-y", "50", "-P", "-F", "#{pane_id}"]);
            args.extend_from_slice(argv);
            let (code, out, err) = tmux(&args);
            if code == 0 && !out.trim().is_empty() {
                Ok(Made { pane: out.trim().to_string() })
            } else {
                Err(if err.is_empty() { format!("could not create session '{name}'") } else { err })
            }
        }
    }
}

/// Split PANE of session SESSION: a pane labelled LABEL taking PCT% on the
/// right (or LEFT), running ARGV.
pub fn split(term: Term, session: &str, pane: &str, dir: &str, pct: u32, left: bool, label: &str, argv: &[String]) -> Result<Made, String> {
    match term {
        Term::Rex => {
            let side = if left { "--split=left" } else { "--split=right" };
            let mut args = strings(&["split", "-s", session, "-b", pane, "-c", dir, side, "--ratio", &pct.to_string(), "--focus=false", "--label", label, "--json", "--"]);
            args.extend_from_slice(argv);
            let (code, out, err) = rex(&args);
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            match first_block(&v) {
                Some(p) if code == 0 => Ok(Made { pane: p }),
                _ => Err(if err.is_empty() { "could not split the pane".into() } else { err }),
            }
        }
        Term::Tmux => {
            let mut args = strings(&["split-window", "-h", "-d", "-l", &format!("{pct}%"), "-t", pane, "-c", dir, "-P", "-F", "#{pane_id}"]);
            if left {
                args.insert(1, "-b".into());
            }
            args.extend_from_slice(argv);
            let (code, out, err) = tmux(&args);
            if code != 0 || out.trim().is_empty() {
                return Err(if err.is_empty() { "could not split the pane".into() } else { err });
            }
            let p = out.trim().to_string();
            tmux_s(&["set-option", "-p", "-t", &p, "@cbar_label", label]);
            Ok(Made { pane: p })
        }
    }
}

/// A new tab TAB in session SESSION (not switched to) running ARGV.
pub fn new_tab(term: Term, session: &str, tab: &str, dir: &str, argv: &[String]) -> Result<Made, String> {
    match term {
        Term::Rex => {
            let mut args = strings(&["window", "new", "-s", session, tab, "-c", dir, "--focus=false", "--json", "--"]);
            args.extend_from_slice(argv);
            let (code, out, err) = rex(&args);
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            match first_block(&v) {
                Some(p) if code == 0 => Ok(Made { pane: p }),
                _ => Err(if err.is_empty() { "could not open a tab".into() } else { err }),
            }
        }
        Term::Tmux => {
            let mut args = strings(&["new-window", "-d", "-t", &format!("={session}:"), "-n", tab, "-c", dir, "-P", "-F", "#{pane_id}"]);
            args.extend_from_slice(argv);
            let (code, out, err) = tmux(&args);
            if code == 0 && !out.trim().is_empty() {
                Ok(Made { pane: out.trim().to_string() })
            } else {
                Err(if err.is_empty() { "could not open a tab".into() } else { err })
            }
        }
    }
}

/// Replace what runs in PANE (a bare shell) with ARGV. tmux respawns the
/// pane in place; Rex closes it and opens ARGV where it was: to the left of
/// NEIGHBOR (its tab's other pane) at PCT%, or as a new tab TAB.
pub fn respawn(term: Term, session: &str, pane: &str, window: &str, neighbor: Option<&str>, tab: &str, dir: &str, pct: u32, argv: &[String]) -> Result<(), String> {
    match term {
        Term::Tmux => {
            let mut args = strings(&["respawn-pane", "-k", "-t", pane, "-c", dir]);
            args.extend_from_slice(argv);
            let (code, _, err) = tmux(&args);
            if code == 0 {
                Ok(())
            } else {
                Err(err)
            }
        }
        Term::Rex => {
            if let Some(n) = neighbor {
                rex(&strings(&["block", "close", "-s", session, pane]));
                split(term, session, n, dir, pct, true, "shell", argv).map(|_| ())
            } else {
                rex(&strings(&["window", "close", "-s", session, window]));
                new_tab(term, session, tab, dir, argv).map(|_| ())
            }
        }
    }
}

/// Type TEXT into PANE, then Enter as a write of its own after a pause:
/// text and Enter in one write reach Claude Code as a paste, which it
/// doesn't submit.
pub fn send_line(term: Term, session: &str, pane: &str, text: &str) -> Result<(), String> {
    let (code, _, err) = match term {
        Term::Rex => rex(&strings(&["send", "-s", session, "-b", pane, text])),
        Term::Tmux => tmux(&strings(&["send-keys", "-t", pane, "-l", "--", text])),
    };
    if code != 0 {
        return Err(err);
    }
    std::thread::sleep(Duration::from_millis(400));
    let (code, _, err) = match term {
        Term::Rex => rex(&strings(&["send", "-s", session, "-b", pane, "\r"])),
        Term::Tmux => tmux_s(&["send-keys", "-t", pane, "Enter"]),
    };
    if code == 0 {
        Ok(())
    } else {
        Err(err)
    }
}

/// The screen and scrollback of PANE, at most the last N lines (tmux).
pub fn capture(term: Term, session: &str, pane: &str, n: usize) -> String {
    match term {
        Term::Tmux => tmux_s(&["capture-pane", "-p", "-J", "-t", pane, "-S", &format!("-{n}")]).1,
        Term::Rex => rex(&strings(&["capture", "-s", session, "-b", pane])).1,
    }
}

pub fn close_tab(term: Term, session: &str, window: &str) -> bool {
    match term {
        Term::Tmux => tmux_s(&["kill-window", "-t", window]).0 == 0,
        Term::Rex => rex(&strings(&["window", "close", "-s", session, window])).0 == 0,
    }
}

pub fn kill_session(term: Term, name: &str) -> bool {
    match term {
        Term::Tmux => tmux_s(&["kill-session", "-t", &format!("={name}")]).0 == 0,
        Term::Rex => match rex_session_id(name) {
            Some(id) => rex(&strings(&["kill", &id])).0 == 0,
            None => false,
        },
    }
}

pub fn rename_session(term: Term, old: &str, new: &str) -> Result<(), String> {
    let (code, _, err) = match term {
        Term::Tmux => tmux_s(&["rename-session", "-t", &format!("={old}"), new]),
        Term::Rex => match rex_session_id(old) {
            Some(id) => rex(&strings(&["session", "rename", &id, new])),
            None => (1, String::new(), format!("no session '{old}'")),
        },
    };
    if code == 0 {
        Ok(())
    } else {
        Err(err)
    }
}

/// Whether ID is a tmux id ($session, @window, %pane).
pub fn is_tmux_id(id: &str) -> bool {
    matches!(id.chars().next(), Some('$' | '@' | '%'))
}

/// Show session SID (and the tab, and the pane) in a tmux client: this one
/// inside tmux, else the most recently active. Returns an error or None.
pub fn tmux_jump(sid: &str, window: Option<&str>, pane: Option<&str>) -> Option<String> {
    let mut client = Vec::new();
    if util::env("TMUX_PANE").is_none() {
        let (_, out, _) = tmux_s(&["list-clients", "-F", "#{client_activity}\t#{client_name}"]);
        let mut clients: Vec<(String, String)> =
            out.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect();
        clients.sort_by(|a, b| b.cmp(a));
        let Some((_, name)) = clients.first() else { return Some("no tmux client attached (tmux attach)".into()) };
        client = vec!["-c".to_string(), name.clone()];
    }
    let mut args = Vec::new();
    if let Some(w) = window {
        args.extend(strings(&["select-window", "-t", w, ";"]));
    }
    if let Some(p) = pane {
        args.extend(strings(&["select-pane", "-t", p, ";"]));
    }
    args.push("switch-client".into());
    args.extend(client);
    args.push("-t".into());
    args.push(pane.or(window).unwrap_or(sid).to_string());
    let (code, _, err) = tmux(&args);
    (code != 0).then(|| if err.is_empty() { "switch-client failed".into() } else { err })
}

/// Show Rex session SID (and the tab) in the Rex app and focus the pane.
/// Returns an error or None; Some("?…") when the focus didn't stick.
pub fn rex_jump(client: Option<String>, sid: &str, window: Option<&str>, pane: Option<&str>) -> Option<String> {
    let Some(client) = client.or_else(app_client) else { return Some("no Rex app connected".into()) };
    let mut args = strings(&["-C", &client, "do", "session.select", &format!("session_id={sid}")]);
    if let Some(w) = window {
        args.push(format!("window_id={w}"));
    }
    let (code, _, err) = rex(&args);
    if code != 0 {
        return Some(if err.is_empty() { "session.select failed".into() } else { err });
    }
    if let Some(p) = pane {
        let (code, _, err) = rex(&strings(&["focus", "-s", sid, p]));
        if code != 0 {
            return Some(if err.is_empty() { "focus failed".into() } else { err });
        }
        // check the server agrees: the tab is active and the pane focused
        let view = rex_json(&["window", "ls", "-s", sid, "--json"]).unwrap_or(Value::Null);
        let win = view["windows"].as_array().and_then(|ws| ws.iter().find(|w| w["window_id"].as_str() == window));
        let ok = view["active_window_id"].as_str() == window && win.is_some_and(|w| w["focused_block_id"].as_str() == Some(p));
        if !ok {
            return Some("?focus did not stick".into());
        }
    }
    None
}
