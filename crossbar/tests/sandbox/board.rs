//! The switchboards' panes and `cbar up`, in a sandbox (common.rs).
//!
//! The exchange has its supervisor and exchange switchboard, and projects
//! alpha and beta their operators and project switchboards. Then:
//!
//! - every board is replaced the way the operator did it live (close the
//!   board's pane, split `cbar run --pane board` off the supervisor's or
//!   operator's pane): all three run;
//! - the exchange switchboard is sent a mouse report and a focus report that
//!   arrive as a lone ESC and then the rest, as the Rex app's can. 0.5.3 quit
//!   on that Esc and its pane dropped to a shell, silently: it must keep
//!   running, and not jump anywhere;
//! - q quits it: its pane and ~/.crossbar/logs/switchboard.log say why;
//! - `cbar up` starts it again in its pane, and leaves the rest alone;
//! - alpha's board pane is closed: `cbar up` adds one next to its operator;
//! - `cbar up` again: everything is running and nothing changes;
//! - a board killed with SIGKILL: its pane and the log say so;
//! - a board whose pane is closed logs why it exited.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use crate::common::*;

#[test]
fn tmux() {
    run(false);
}

#[test]
#[ignore = "needs Rex and a private Rex server: cargo test --test sandbox -- --ignored"]
fn rex() {
    run(true);
}

fn run(rex: bool) {
    let Some(mut s) = Sandbox::new("board", rex) else { return };
    s.setup();
    cases(&s);
    s.finish();
}

const SHELLS: [&str; 6] = ["sh", "zsh", "bash", "-sh", "-zsh", "-bash"];

type Boards = BTreeMap<String, Vec<(String, String, String)>>;

/// {session: [(sid, pane, tab)]} of the panes labelled switchboard.
fn boards(s: &Sandbox) -> Boards {
    let mut out = Boards::new();
    for p in s.panes() {
        if p.label == "switchboard" {
            out.entry(p.session).or_default().push((p.sid, p.pane, p.tab));
        }
    }
    out
}

fn board(s: &Sandbox, session: &str) -> Option<(String, String)> {
    boards(s).get(session).and_then(|b| b.first()).map(|(sid, pane, _)| (sid.clone(), pane.clone()))
}

fn running(s: &Sandbox, session: &str) -> bool {
    board(s, session).is_some_and(|(sid, pane)| s.program(&sid, &pane) == "switchboard")
}

fn is_shell(program: &str) -> bool {
    SHELLS.contains(&program)
}

fn tabs(s: &Sandbox, session: &str) -> Vec<String> {
    boards(s).get(session).into_iter().flatten().map(|(_, _, t)| t.clone()).collect()
}

fn cases(s: &Sandbox) {
    let e = EXCHANGE.to_string();
    let alpha = format!("{EXCHANGE}/alpha");
    let beta = format!("{EXCHANGE}/beta");
    let log = s.cb.join("logs").join("switchboard.log");
    let logged = || read(&log);
    let sessions = [e.clone(), alpha.clone(), beta.clone()];
    let leads = [(e.clone(), &s.panes_at[EXCHANGE]), (alpha.clone(), &s.panes_at["alpha"]), (beta.clone(), &s.panes_at["beta"])];
    let board_argv: Vec<String> = vec![s.bin.join("cbar").to_string_lossy().into_owned(), "run".into(), "--pane".into(), "board".into()];

    // every board replaced the operator's way
    for (session, (lsid, lead)) in &leads {
        if let Some((sid, pane)) = board(s, session) {
            s.close(&sid, &pane);
        }
        s.split(lsid, lead, &s.home, &board_argv, "switchboard");
    }
    let ok = wait_for(20.0, || sessions.iter().all(|x| running(s, x)));
    let what: Vec<String> = sessions
        .iter()
        .map(|x| format!("{x}: {}", board(s, x).map(|(sid, pane)| s.program(&sid, &pane)).unwrap_or_else(|| "no pane".into())))
        .collect();
    s.check("boards replaced the operator's way all run", ok, what.join(", "));
    sleep(3.0);
    s.check("and keeps running", sessions.iter().all(|x| running(s, x)), "");

    let Some((sid, ex_board)) = board(s, &e) else {
        s.check("the exchange has a board", false, format!("{:?}", boards(s)));
        return;
    };
    // a mouse report and a focus report that arrive as a lone ESC, then the
    // rest. Read as keys, its digits jump to sessions: a jump leaves ✓, ✗ or ?
    // in the footer for 3s
    let mut footers = Vec::new();
    for tail in ["[<35;1;3M", "[I", "[<35;12;2M"] {
        s.write(&sid, &ex_board, "\x1b");
        sleep(0.1);
        s.write(&sid, &ex_board, tail);
        sleep(0.6);
        let screen = s.screen(&sid, &ex_board);
        footers.push(screen.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string());
    }
    sleep(2.0);
    s.check(
        "a split mouse or focus report doesn't quit the exchange switchboard",
        s.program(&sid, &ex_board) == "switchboard",
        last(&s.screen(&sid, &ex_board), 300).trim(),
    );
    s.check("nor jump to a session", !footers.iter().any(|f| f.starts_with(['✓', '✗', '?'])), format!("{footers:?}"));

    // q quits it, and it says why
    let mark = logged().len();
    s.write(&sid, &ex_board, "q");
    let gone = wait_for(10.0, || is_shell(&s.program(&sid, &ex_board)));
    s.check("q quits it to a shell", gone, s.program(&sid, &ex_board));
    let screen = s.screen(&sid, &ex_board);
    s.check("its pane says why", screen.contains("switchboard: exited: q pressed"), last(&screen, 300).trim());
    let new = logged()[mark..].to_string();
    s.check("so does the log", new.contains("exited: q pressed") && new.contains(&format!("switchboard --exchange {EXCHANGE}")), new.trim());
    s.check("which has its start too", logged().contains("): started"), logged().trim().chars().take(300).collect::<String>());

    // cbar up starts it again in its pane, and leaves the rest alone
    let others: Vec<(String, Option<(String, String)>)> = sessions.iter().filter(|x| **x != e).map(|x| (x.clone(), board(s, x))).collect();
    let o = s.cbar(&["up"], Some(EXCHANGE));
    s.check("cbar up says it started the exchange switchboard again", o.out.contains(&format!("{e}: started its switchboard again")), o.to_string());
    s.check("it's running", wait_for(15.0, || running(s, &e)), format!("{:?}", boards(s).get(&e)));
    s.check("one board in the exchange's session", boards(s).get(&e).map(Vec::len) == Some(1), format!("{:?}", boards(s).get(&e)));
    s.check("in the supervisor's tab", tabs(s, &e).iter().all(|t| t == "supervisor"), format!("{:?}", tabs(s, &e)));
    s.check(
        "the projects' boards are left alone",
        others.iter().all(|(x, b)| board(s, x) == *b) && others.iter().all(|(x, _)| o.out.contains(&format!("{x}: running"))),
        o.to_string(),
    );

    // alpha's board pane is closed: cbar up puts one next to its operator
    if let Some((asid, apane)) = board(s, &alpha) {
        s.close(&asid, &apane);
    }
    wait_for(10.0, || !boards(s).contains_key(&alpha));
    let o = s.cbar(&["up"], Some(EXCHANGE));
    s.check("cbar up adds alpha's board", o.out.contains(&format!("{alpha}: started its switchboard")), o.to_string());
    s.check("which runs", wait_for(15.0, || running(s, &alpha)), format!("{:?}", boards(s).get(&alpha)));
    s.check("in alpha's operator tab", tabs(s, &alpha) == ["operator"], format!("{:?}", tabs(s, &alpha)));

    // and again: nothing to do
    sleep(2.0);
    let snap = boards(s);
    let o = s.cbar(&["up"], Some(EXCHANGE));
    s.check(
        "cbar up again: all running",
        sessions.iter().all(|x| o.out.contains(&format!("{x}: running"))) && !o.out.contains("started"),
        o.to_string(),
    );
    let now = boards(s);
    s.check("and no board changed", now == snap, format!("{snap:?} → {now:?}"));

    // a board killed outright: cbar run --pane board says so
    let Some((bsid, bpane)) = board(s, &beta).filter(|_| running(s, &beta)) else {
        s.check("beta's board runs, to kill", false, format!("{:?}", boards(s)));
        return;
    };
    let pid: Option<i32> = if s.rex {
        s.rex_json(&["block", "call", "-s", &bsid, "-b", &bpane, "process"])["foreground"]["pid"].as_i64().map(|p| p as i32)
    } else {
        let root = s.tmux(&["display-message", "-p", "-t", &bpane, "#{pane_pid}"]).out.trim().to_string();
        let kids = Command::new("pgrep").args(["-P", &root]).stdin(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        kids.split_whitespace().next().and_then(|p| p.parse().ok())
    };
    let mark = logged().len();
    if let Some(pid) = pid {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    wait_for(10.0, || is_shell(&s.program(&bsid, &bpane)));
    let screen = s.screen(&bsid, &bpane);
    s.check("a killed board's pane says so", screen.contains("switchboard: killed by SIGKILL"), last(&screen, 300).trim());
    let new = logged()[mark..].to_string();
    s.check("so does the log", new.contains("killed by SIGKILL"), new.trim());
    // in tmux, a killed board leaves its pane's mark on the shell: up still sees it's gone
    let o = s.cbar(&["up"], Some(EXCHANGE));
    s.check("cbar up starts the killed board again", o.out.contains(&format!("{beta}: started its switchboard again")), o.to_string());
    s.check("which runs", wait_for(15.0, || running(s, &beta)), format!("{:?}", boards(s).get(&beta)));

    // a board whose pane is closed logs why it exited
    let mark = logged().len();
    let Some((asid, apane)) = board(s, &alpha) else {
        s.check("alpha has a board, to close", false, format!("{:?}", boards(s)));
        return;
    };
    s.close(&asid, &apane);
    let got = wait_for(10.0, || logged()[mark..].contains("exited:"));
    s.check("a board whose pane closes logs why", got, logged()[mark..].trim());
    sleep(1.0);
    s.check("and no board's exit reads as a crash", !logged().contains("crashed"), logged().trim());
}
