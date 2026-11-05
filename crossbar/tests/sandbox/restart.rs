//! `cbar restart` in a sandbox (common.rs).
//!
//! The stub claude records each start's argv and writes
//! ~/.claude/sessions/PID.json under the scratch HOME, as Claude Code does.
//! The cases: an idle operator, a done agent, the supervisor; a blocked and a
//! working agent refused; a Claude whose session file is missing refused with
//! nothing killed; a Claude that ignores /exit (SIGTERM); one with background
//! work, which Claude asks to stop on /exit; an operator restarting its own
//! pane from a tool it runs (its process group goes when it quits); and a
//! supervisor whose session is in ~, which resumes there and stops at
//! Claude's folder-trust prompt, unanswered. Also an exchange's `path`: where
//! its supervisor and agents start. Each restart must resume the old session
//! id, pass the current role prompt, keep the pane's place and the board next
//! to it, and type why.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

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
    let Some(mut s) = Sandbox::new("restart", rex) else { return };
    s.env.insert("CBAR_STUB_SESSIONS".into(), "1".into());
    s.env.insert("CBAR_STUB_LOG".into(), s.root.join("argv").to_string_lossy().into_owned());
    s.setup();
    let w = World { log: s.root.join("argv"), sessions: s.home.join(".claude").join("sessions"), s: &s };
    cases(&w);
    s.finish();
}

struct World<'a> {
    s: &'a Sandbox,
    log: PathBuf,
    sessions: PathBuf,
}

/// How a restart should go.
#[derive(Default)]
struct Want<'a> {
    prompt_mark: Option<&'a str>,
    /// Whether its new argv should have -w.
    worktree_flag: Option<bool>,
    sigterm: bool,
    /// Asked on /exit whether to keep its worktree (and kept it).
    keeps: bool,
    /// Asked on /exit whether to stop its background work (and stopped it).
    stops: bool,
}

fn real(p: &str) -> PathBuf {
    Path::new(p).canonicalize().unwrap_or_else(|_| PathBuf::from(p))
}

fn arg_after<'v>(argv: &'v [String], flag: &str) -> Option<&'v str> {
    argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).map(String::as_str)
}

impl World<'_> {
    /// [(pid, argv)] of each start of the stub named NAME, oldest first.
    fn starts(&self, name: &str) -> Vec<(i32, Vec<String>)> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.log).into_iter().flatten().flatten() {
            let f = e.file_name().to_string_lossy().into_owned();
            let Some((n, pid)) = f.rsplit_once('.') else { continue };
            let Ok(pid) = pid.parse::<i32>() else { continue };
            if n != name {
                continue;
            }
            let mtime = e.metadata().and_then(|m| m.modified()).ok();
            let argv: Vec<String> = read(e.path()).split('\0').map(String::from).collect();
            let argv = argv[..argv.len().saturating_sub(1)].to_vec();
            out.push((mtime, pid, argv));
        }
        out.sort();
        out.into_iter().map(|(_, pid, argv)| (pid, argv)).collect()
    }

    fn session(&self, pid: i32) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.sessions.join(format!("{pid}.json"))).ok()?).ok()
    }

    fn session_str(&self, pid: i32, key: &str) -> String {
        self.session(pid).and_then(|v| v[key].as_str().map(String::from)).unwrap_or_default()
    }

    /// What a restart must keep of SESSION's panes, sorted. In Rex, each
    /// pane's tab, label, place in its tab and whether it has the focus; in
    /// tmux, its tab, label and pane id (respawned in place).
    fn layout(&self, session: &str) -> Vec<String> {
        let s = self.s;
        let mut out = Vec::new();
        if !s.rex {
            out = s.panes().into_iter().filter(|p| p.session == session).map(|p| format!("{} | {} | {}", p.tab, p.label, p.pane)).collect();
        } else if let Some(sid) = s.session_id(session) {
            let blocks = s.rex_json(&["block", "ls", "-s", &sid, "--json"]);
            let labels: std::collections::BTreeMap<String, String> = blocks["blocks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| (b["block_id"].as_str().unwrap_or("").into(), b["label"].as_str().unwrap_or("").into()))
                .collect();
            fn leaves(node: &Value, tab: &str, focused: &str, labels: &std::collections::BTreeMap<String, String>, out: &mut Vec<String>) {
                if !node.is_object() {
                    return;
                }
                if let Some(b) = node["block_id"].as_str() {
                    let rect: Vec<String> = node["rect"].as_array().into_iter().flatten().map(|x| format!("{:.3}", x.as_f64().unwrap_or(0.0))).collect();
                    out.push(format!("{tab} | {} | {} | {}", labels.get(b).map(String::as_str).unwrap_or("?"), rect.join(","), b == focused));
                }
                for k in ["split", "before", "after"] {
                    leaves(&node[k], tab, focused, labels, out);
                }
            }
            let windows = s.rex_json(&["window", "ls", "-s", &sid, "--json"]);
            for w in windows["windows"].as_array().into_iter().flatten() {
                for layer in w["layers"].as_array().into_iter().flatten() {
                    leaves(&layer["layout"], w["label"].as_str().unwrap_or(""), w["focused_block_id"].as_str().unwrap_or(""), &labels, &mut out);
                }
            }
        }
        out.sort();
        out
    }

    fn cbar(&self, args: &[&str]) -> Out {
        self.s.cbar(args, None)
    }

    fn tail(&self, path: &str) -> String {
        self.cbar(&["tail", path, "30"]).out
    }

    fn exists(&self, name: &str, pid: i32, ext: &str) -> bool {
        self.log.join(format!("{name}.{pid}.{ext}")).exists()
    }

    /// Restart PATH (or RUN does) and check what came back in its place.
    fn restarted(&self, label: &str, path: &str, session: &str, name: &str, want: Want, run: Option<&dyn Fn() -> Out>) {
        let s = self.s;
        self.cbar(&["wait", path, "60"]); // a restarted stub works on the why-line for a moment
        let before = self.starts(name);
        if !s.check(format!("{label}: has a start to restart"), !before.is_empty(), "") {
            return;
        }
        let old_pid = before.last().unwrap().0;
        let (old_sid, old_cwd) = (self.session_str(old_pid, "sessionId"), self.session_str(old_pid, "cwd"));
        let layout = self.layout(session);
        let o = match run {
            Some(f) => f(),
            None => self.cbar(&["restart", path]),
        };
        s.check(format!("{label}: cbar restart exits 0"), o.code == 0, o.to_string());
        let after = wait_some(90.0, || {
            let a = self.starts(name);
            (a.len() > before.len()).then_some(a)
        });
        let Some(after) = after else {
            s.check(format!("{label}: a new Claude started"), false, o.to_string());
            return;
        };
        s.check(format!("{label}: a new Claude started"), true, "");
        let (new_pid, argv) = after.last().unwrap().clone();
        let out = o.out.trim();
        s.check(format!("{label}: the old Claude quit"), !alive(old_pid), old_pid.to_string());
        if run.is_none() {
            let name = if want.sigterm { "SIGTERM" } else { "it quit on /exit" };
            s.check(format!("{label}: {name}"), out.contains("SIGTERM") == want.sigterm, out);
        }
        let kept = self.exists(name, old_pid, "exit");
        s.check(
            format!("{label}: {}", if want.keeps { "kept its worktree when asked" } else { "was not asked about a worktree" }),
            kept == want.keeps && (out.contains("keeping it") == want.keeps || run.is_some()),
            out,
        );
        let stopped = self.exists(name, old_pid, "bg");
        s.check(
            format!("{label}: {}", if want.stops { "stopped its background work when asked" } else { "was not asked about background work" }),
            stopped == want.stops && (out.contains("stopping it") == want.stops || run.is_some()),
            out,
        );
        let line: String = argv.join(" ").chars().take(300).collect();
        s.check(format!("{label}: resumes session {old_sid}"), arg_after(&argv, "--resume") == Some(old_sid.as_str()), &line);
        let new = wait_some(10.0, || self.session(new_pid)).unwrap_or(Value::Null);
        let (new_sid, new_cwd) = (new["sessionId"].as_str().unwrap_or(""), new["cwd"].as_str().unwrap_or(""));
        s.check(format!("{label}: its session is the same"), new_sid == old_sid, format!("{new_sid} vs {old_sid}"));
        s.check(format!("{label}: in the same folder"), new_cwd == old_cwd, format!("{new_cwd} vs {old_cwd}"));
        if let Some(mark) = want.prompt_mark {
            let p = arg_after(&argv, "--append-system-prompt");
            s.check(format!("{label}: with the current role prompt"), p.is_some_and(|p| p.starts_with(mark)), p.unwrap_or("none").chars().take(80).collect::<String>());
        }
        // a supervisor or operator gets its watch guard (the Stop hook) again; an agent never has one
        let guard = arg_after(&argv, "--settings");
        let role = name.ends_with("operator") || name.ends_with("supervisor");
        s.check(
            format!("{label}: {}", if role { "with its watch guard" } else { "no watch guard" }),
            if role { guard.is_some_and(|g| g.contains("\"Stop\"") && g.contains(" watch --check")) } else { guard.is_none() },
            guard.unwrap_or("none"),
        );
        if let Some(flag) = want.worktree_flag {
            s.check(format!("{label}: no -w (already in its worktree)"), argv.iter().any(|a| a == "-w") == flag, argv.join(" "));
        }
        s.check(format!("{label}: no brief prompt again"), !argv.iter().any(|a| a.starts_with("Read ")), argv.join(" "));
        let told = wait_for(60.0, || self.tail(path).contains("crossbar restarted you on"));
        let tail = self.tail(path);
        s.check(format!("{label}: told why"), told, last(&tail, 400));
        let now = self.layout(session);
        s.check(format!("{label}: layout kept"), now == layout, format!("{now:?} vs {layout:?}"));
    }

    fn refused(&self, label: &str, path: &str, name: &str, want: &str) {
        let s = self.s;
        let before = self.starts(name);
        let pid = before.last().map(|(p, _)| *p);
        let session = path.contains('/').then(|| path.split('/').take(2).collect::<Vec<_>>().join("/"));
        let layout = session.as_deref().map(|x| self.layout(x));
        let o = self.cbar(&["restart", path]);
        s.check(format!("{label}: refused"), o.code == 1 && o.err.contains(want), o.to_string());
        sleep(2.0);
        s.check(format!("{label}: nothing restarted"), self.starts(name).len() == before.len(), "");
        s.check(format!("{label}: its Claude still runs"), pid.is_some_and(alive), format!("{pid:?}"));
        if let (Some(session), Some(layout)) = (session, layout) {
            s.check(format!("{label}: layout kept"), self.layout(&session) == layout, "");
        }
    }
}

fn cases(w: &World) {
    let s = w.s;
    let alpha = format!("{EXCHANGE}/alpha");
    let beta = format!("{EXCHANGE}/beta");
    let sup = format!("{EXCHANGE} supervisor");
    // today's role prompts, as an update would leave them
    for role in ["operator", "supervisor"] {
        fs::write(s.cb.join(format!("{role}.md")), format!("RESTART-TEST {role} prompt\n")).unwrap();
    }
    let op_prompt = Some("RESTART-TEST operator prompt");

    w.restarted("idle operator", &alpha, &alpha, "alpha operator", Want { prompt_mark: op_prompt, ..Default::default() }, None);
    w.restarted("idle supervisor", EXCHANGE, EXCHANGE, &sup, Want { prompt_mark: Some("RESTART-TEST supervisor prompt"), ..Default::default() }, None);
    // started with -w: Claude asks on /exit whether to keep the worktree
    let done = format!("{alpha}/t-done");
    w.restarted("done agent", &done, &alpha, "t-done", Want { worktree_flag: Some(false), keeps: true, ..Default::default() }, None);
    // restarted already (no -w now): /exit just quits
    w.restarted("done agent again", &done, &alpha, "t-done", Want { worktree_flag: Some(false), ..Default::default() }, None);
    // once restarted, a Claude can be restarted again
    w.restarted("operator again", &alpha, &alpha, "alpha operator", Want { prompt_mark: op_prompt, ..Default::default() }, None);

    w.refused("blocked agent", &format!("{alpha}/t-ask"), "t-ask", "is blocked");
    w.refused("working agent", &format!("{alpha}/t-busy"), "t-busy", "is working");

    // no session file: a clear error, nothing killed
    let chat = w.starts("t-chat").last().map(|(p, _)| *p).expect("t-chat started");
    let file = w.sessions.join(format!("{chat}.json"));
    let hidden = w.sessions.join(format!("{chat}.json.hidden"));
    fs::rename(&file, &hidden).unwrap();
    w.refused("missing session file", &format!("{alpha}/t-chat"), "t-chat", "found no Claude session file");
    fs::rename(&hidden, &file).unwrap();

    // background work (a cbar watch Monitor): Claude asks on /exit whether to
    // stop it; t-chat was started with -w, so then it asks about its worktree
    let chat = s.pane_in(&alpha, "t-chat").expect("t-chat's pane");
    s.send_line(&chat.sid, &chat.pane, "/bg");
    sleep(1.0);
    w.restarted(
        "agent with background work",
        &format!("{alpha}/t-chat"),
        &alpha,
        "t-chat",
        Want { worktree_flag: Some(false), keeps: true, stops: true, ..Default::default() },
        None,
    );

    // a Claude that doesn't quit on /exit gets SIGTERM
    let nogit = s.pane_in(&beta, "t-nogit").expect("t-nogit's pane");
    s.send_line(&nogit.sid, &nogit.pane, "/deaf");
    sleep(1.0);
    w.restarted("deaf agent (SIGTERM)", &format!("{beta}/t-nogit"), &beta, "t-nogit", Want { worktree_flag: Some(false), sigterm: true, ..Default::default() }, None);

    // an operator restarts its own pane from a tool it runs
    let op = s.pane_in(&beta, "operator").expect("beta's operator pane");
    let own_out = s.root.join("own.out");
    let own = || {
        s.send_line(&op.sid, &op.pane, &format!("/sh {} restart operator > {} 2>&1", s.bin.join("cbar").display(), own_out.display()));
        let out = wait_some(30.0, || Some(read(&own_out)).filter(|t| !t.is_empty())).unwrap_or_default();
        Out { code: if out.contains("restarting") { 0 } else { 1 }, out, err: String::new() }
    };
    w.restarted("own pane", &beta, &beta, "beta operator", Want { prompt_mark: op_prompt, ..Default::default() }, Some(&own));
    let log = s.cb.join("logs").join("restart-central-beta-operator.log");
    wait_for(20.0, || read(&log).contains("restarted central/beta/operator"));
    let text = read(&log);
    let tail = last(&text, 300);
    s.check("own pane: the detached restart logged it", text.contains("restarted central/beta/operator"), tail);
    s.check("own pane: it quit on /exit", !text.contains("SIGTERM"), tail);

    exchange_path(w);
    trust(w);
}

/// The row of `cbar ls` for the exchange.
fn ls_row(w: &World) -> String {
    w.cbar(&["ls"]).out.lines().find(|l| l.starts_with(&format!("{EXCHANGE} "))).unwrap_or("").to_string()
}

/// An exchange's supervisor and agents start in its `path`, else in its own
/// folder; never in ~ (Claude Code asks to trust ~ each time).
fn exchange_path(w: &World) {
    let s = w.s;
    let exdir = s.exchange_dir();
    let toml = exdir.join("exchange.toml");
    let sup = format!("{EXCHANGE} supervisor");
    let now = w.session_str(w.starts(&sup).last().unwrap().0, "cwd");
    s.check("exchange path: no path: the supervisor starts in the exchange's folder", real(&now) == real(&exdir.to_string_lossy()), &now);
    let row = ls_row(w);
    s.check("exchange path: ls shows the exchange's folder", row.contains(&format!(" {} ", s.short(&exdir))), &row);
    // the operator adds a path by hand, with ~
    let work = s.home.join("central");
    fs::create_dir_all(&work).unwrap();
    let text = read(&toml);
    fs::write(&toml, format!("{text}path = \"~/central\"\n")).unwrap();
    let o = w.cbar(&["up"]);
    s.check("exchange path: cbar up keeps it", o.code == 0 && read(&toml).contains("path = \"~/central\""), format!("{}{o}", read(&toml)));
    let row = ls_row(w);
    s.check("exchange path: ls shows it", row.contains(" ~/central "), &row);
    fs::write(exdir.join("briefs").join("t-ex.md"), brief("t-chat")).unwrap();
    let o = w.cbar(&["new", "--agent", "t-ex"]);
    let started = wait_some(30.0, || Some(w.starts("t-ex")).filter(|v| !v.is_empty()));
    let cwd = started
        .and_then(|st| wait_some(10.0, || w.session(st.last().unwrap().0)))
        .and_then(|v| v["cwd"].as_str().map(String::from))
        .unwrap_or_default();
    s.check("exchange path: an exchange agent starts in it", o.code == 0 && real(&cwd) == real(&work.to_string_lossy()), format!("{o} cwd={cwd}"));
    // a missing folder: a clear error
    let text = read(&toml);
    fs::write(&toml, text.replace("path = \"~/central\"", "path = \"~/nope\"")).unwrap();
    fs::write(exdir.join("briefs").join("t-ex2.md"), brief("t-chat")).unwrap();
    let o = w.cbar(&["new", "--agent", "t-ex2"]);
    s.check("exchange path: a missing folder is an error", o.code == 1 && o.err.contains("no folder ~/nope"), o.to_string());
    fs::write(&toml, text).unwrap();
}

/// A supervisor whose session is keyed to ~ (started before 0.5.2) resumes
/// in ~, where Claude Code asks to trust the folder: cbar reports that, exits
/// 1, and never answers it or types the why-line into it.
fn trust(w: &World) {
    let s = w.s;
    let sup = format!("{EXCHANGE} supervisor");
    let pid = w.starts(&sup).last().unwrap().0;
    let f = w.sessions.join(format!("{pid}.json"));
    let mut d: Value = serde_json::from_str(&read(&f)).expect("the supervisor's session file");
    d["cwd"] = Value::String(s.home.to_string_lossy().into_owned());
    fs::write(&f, d.to_string()).unwrap();
    let untrusted = s.home.join(".claude").join("stub-untrusted");
    fs::write(&untrusted, format!("{}\n", s.home.display())).unwrap();
    let before = w.starts(&sup).len();
    let o = w.cbar(&["restart", EXCHANGE]);
    let want = format!("{EXCHANGE}'s Claude is waiting at the folder-trust prompt for ~; answer it in its pane");
    s.check("trust prompt: restart says so and exits 1", o.code == 1 && o.err.contains(&want), o.to_string());
    let after = w.starts(&sup);
    s.check("trust prompt: a new Claude started", after.len() == before + 1, "");
    if let Some((new, _)) = after.last().filter(|_| after.len() > before) {
        let cwd = w.session_str(*new, "cwd");
        s.check("trust prompt: it resumed in ~, where its conversation is", real(&cwd) == real(&s.home.to_string_lossy()), &cwd);
        sleep(3.0);
        s.check("trust prompt: nobody answered it", !w.exists(&sup, *new, "trust") && alive(*new), "");
    }
    s.check("trust prompt: the why-line wasn't typed", !w.tail(EXCHANGE).contains("crossbar restarted you"), "");
    let _ = fs::remove_file(&untrusted);
}
