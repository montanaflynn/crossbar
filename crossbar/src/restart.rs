//! Finding the Claude Code session that runs in a pane, for `cbar restart`.
//!
//! Claude Code writes `~/.claude/sessions/<pid>.json` (under
//! $CLAUDE_CONFIG_DIR if set) for each interactive Claude, with its `pid`,
//! `sessionId` and `cwd`. A pane's Claude is the live one whose process
//! descends from the pane's own process (cbar run, maybe claude-relay, then
//! claude); the one closest to it if a Claude runs another.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::util;

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub pid: i64,
    pub id: String,
    pub cwd: String,
}

/// Where Claude Code keeps its session files.
pub fn sessions_dir() -> PathBuf {
    let base = util::env("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| util::home().join(".claude"));
    base.join("sessions")
}

/// A session file's pid, session id and cwd.
pub fn parse(text: &str) -> Option<Session> {
    let v: Value = serde_json::from_str(text).ok()?;
    let pid = v["pid"].as_i64()?;
    let id = v["sessionId"].as_str().filter(|s| !s.is_empty())?.to_string();
    Some(Session { pid, id, cwd: v["cwd"].as_str().unwrap_or("").to_string() })
}

/// The session files of Claudes that still run.
pub fn live_sessions() -> Vec<Session> {
    let Ok(rd) = fs::read_dir(sessions_dir()) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| parse(&fs::read_to_string(p).ok()?))
        .filter(|s| util::alive(&s.pid.to_string()))
        .collect()
}

/// {pid: parent pid} of every process.
pub fn parents() -> HashMap<i64, i64> {
    let (_, ps, _) = util::run("ps", &util::strings(&["-A", "-o", "pid=,ppid="]), Duration::from_secs(5));
    let mut out = HashMap::new();
    for line in ps.lines() {
        let mut it = line.split_whitespace();
        if let (Some(Ok(a)), Some(Ok(b))) = (it.next().map(str::parse), it.next().map(str::parse)) {
            out.insert(a, b);
        }
    }
    out
}

/// How many steps up from PID its ancestor ROOT is (0: PID is ROOT).
pub fn depth_under(pid: i64, root: i64, parent: &HashMap<i64, i64>) -> Option<usize> {
    let mut p = pid;
    for hops in 0..32 {
        if p == root {
            return Some(hops);
        }
        p = *parent.get(&p)?;
        if p <= 1 {
            return None;
        }
    }
    None
}

/// The session of the Claude closest under ROOT, among SESSIONS.
pub fn under(root: i64, sessions: Vec<Session>, parent: &HashMap<i64, i64>) -> Option<Session> {
    sessions.into_iter().filter_map(|s| depth_under(s.pid, root, parent).map(|d| (d, s))).min_by_key(|(d, _)| *d).map(|(_, s)| s)
}

/// The session of the Claude running under ROOT (a pane's process).
pub fn find(root: i64) -> Option<Session> {
    under(root, live_sessions(), &parents())
}

/// Whether SCREEN shows Claude Code's question on /exit in a worktree
/// session (`claude -w`) with "Keep worktree" chosen, so Enter keeps it:
///
///   Exiting worktree session
///   ❯ 1. Keep worktree    Stays at …/.claude/worktrees/NAME
///     2. Remove worktree  Clean up the worktree directory.
pub fn keep_worktree_prompt(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(at) = lines.iter().rposition(|l| l.contains("Exiting worktree session")) else { return false };
    lines[at..].iter().any(|l| l.trim_start().strip_prefix('❯').is_some_and(|rest| rest.trim_start().trim_start_matches("1.").trim_start().starts_with("Keep worktree")))
}

/// Wait up to LIMIT for PID to exit.
pub fn wait_exit(pid: i64, limit: Duration) -> bool {
    let until = std::time::Instant::now() + limit;
    while util::alive(&pid.to_string()) {
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_session_file() {
        let s = parse(r#"{"pid":69568,"sessionId":"ccf09a1c-353f","cwd":"/w","status":"busy"}"#).unwrap();
        assert_eq!(s, Session { pid: 69568, id: "ccf09a1c-353f".into(), cwd: "/w".into() });
        assert_eq!(parse(r#"{"pid":1}"#), None);
        assert_eq!(parse("nope"), None);
    }

    #[test]
    fn sees_the_keep_worktree_question() {
        // as Claude Code 2.1.316 draws it after /exit in a `claude -w` session
        let asked = "❯ /exit\n──────── probe ─\n  Exiting worktree session\n  This session was named \"probe\". Keep the worktree to resume it later, or remove it to clean up.\n  ❯ 1. Keep worktree    Stays at /tmp/x/.claude/worktrees/probe\n    2. Remove worktree  Clean up the worktree directory.\n  Enter to confirm · Esc to cancel\n";
        assert!(keep_worktree_prompt(asked));
        let remove = asked.replace("  ❯ 1. Keep", "    1. Keep").replace("    2. Remove", "  ❯ 2. Remove");
        assert!(!keep_worktree_prompt(&remove));
        assert!(!keep_worktree_prompt("❯ /exit\n  ❯ 1. Keep worktree\n")); // not after the question
        assert!(!keep_worktree_prompt("❯ \n  ⏵⏵ auto mode on\n"));
    }

    #[test]
    fn picks_the_claude_closest_under_the_pane() {
        // pane 10 → relay 11 → claude 12 → bash 13 → claude 14; another pane 20 → claude 21
        let parent: HashMap<i64, i64> = [(10, 1), (11, 10), (12, 11), (13, 12), (14, 13), (20, 1), (21, 20)].into();
        let s = |pid, id: &str| Session { pid, id: id.into(), cwd: String::new() };
        let all = vec![s(14, "inner"), s(21, "other"), s(12, "mine")];
        assert_eq!(under(10, all.clone(), &parent).unwrap().id, "mine");
        assert_eq!(under(20, all.clone(), &parent).unwrap().id, "other");
        assert_eq!(under(30, all, &parent), None);
        assert_eq!(depth_under(10, 10, &parent), Some(0));
    }
}
