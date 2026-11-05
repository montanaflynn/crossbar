//! `cbar watch`, in a sandbox (common.rs).
//!
//! First `cbar watch --next` in alpha's operator pane, run after run: the
//! first exits at once on what stands; one with nothing new waits, silently,
//! until an agent turns done, and through a turn that ends on the line printed
//! last for its pane; what happens while none runs (a done, a block, a Claude
//! that quits, a closed tab) is printed by the next run, and nothing by the
//! one after.
//!
//! Then the supervisor's watch hears the operators of alpha and beta, idle.
//! In Rex, its `rex block call`s then fail for a few looks, as the live ones
//! did when the Rex server was slow (they timed out at cbar's 5s): 0.5.2
//! printed "<project> operator: stopped" for Claudes that were still there.
//! Nothing may be printed for that. Then beta's Claude really quits (/exit):
//! "beta operator: stopped" must come within a look or two, and the pane,
//! once closed, "closed".

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

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
    let Some(mut s) = Sandbox::new("watch", rex) else { return };
    s.setup();
    next_cases(&s);
    guard_cases(&s);
    cases(&s);
    s.finish();
}

/// A folder whose `rex` fails every `rex block call` while the file it
/// returns exists (a second late, as a slow server's call does), and is the
/// sandbox's rex otherwise.
fn flaky_rex(s: &Sandbox) -> (PathBuf, PathBuf) {
    let d = s.root.join("flaky");
    fs::create_dir_all(&d).unwrap();
    let flag = s.root.join("rex-fails");
    let script = format!(
        "#!/bin/sh\nif [ -e \"{}\" ] && [ \"$1 $2\" = \"block call\" ]; then sleep 1; echo \"rex: timed out\" >&2; exit 1; fi\nexec \"{}\" \"$@\"\n",
        flag.display(),
        s.stub.join("rex").display()
    );
    fs::write(d.join("rex"), script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(d.join("rex"), fs::Permissions::from_mode(0o755)).unwrap();
    (d, flag)
}

fn agent_pane(s: &Sandbox, tab: &str) -> (String, String) {
    let p = s.pane_in(&s.session(Some("alpha")), tab).unwrap_or_else(|| panic!("no pane for {tab}"));
    (p.sid, p.pane)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// `cbar watch --next` in alpha's operator pane, run after run, over alpha's
/// agents (t-done done, t-ask blocked, t-boom error, t-busy working, t-chat idle).
fn next_cases(s: &Sandbox) {
    let mut runs = 0;
    let mut logs = Vec::new();
    let mut start = || {
        runs += 1;
        let log = s.root.join(format!("next-{runs}.out"));
        logs.push(log.clone());
        s.spawn(&["cbar", "watch", "--next"], Some("alpha"), &[], &log)
    };

    // 1: what stands at startup is news to the first watch: it prints it and exits at once
    let mut p = start();
    let t0 = Instant::now();
    s.check("--next with news at startup exits 0", p.exits_ok(20.0), p.text().trim());
    let took = t0.elapsed().as_secs_f64();
    s.check("promptly (within 8s)", took <= 8.0, format!("{took:.1}s"));
    let want = strings(&["t-ask: blocked (permission) — allow npm test?", "t-boom: error — tests failed", "t-done: done — finished the thing"]);
    s.check("it printed what stands", p.lines() == want, format!("{:?}", p.lines()));
    p.stop();

    // 2: nothing new: it waits, silently, until t-chat is sent a line and turns done
    let mut p = start();
    sleep(12.0);
    s.check("idle, --next keeps waiting", p.running(), format!("exit {:?}", p.code()));
    s.check("and prints nothing (what stands was printed)", p.lines().is_empty(), format!("{:?}", p.lines()));
    let chat = agent_pane(s, "t-chat");
    s.send_line(&chat.0, &chat.1, "ping");
    s.check("an event exits it", p.exits_ok(20.0), p.text().trim());
    s.check("it printed the event", p.lines() == strings(&["t-chat: done — did: ping"]), format!("{:?}", p.lines()));
    p.stop();

    // 2b: t-chat's next turn ends on the same line: no news, so it waits; a new line is news
    let mut p = start();
    s.send_line(&chat.0, &chat.1, "ping");
    sleep(12.0); // working, and done again with the same message
    s.check("a turn ending as the last one did doesn't exit --next", p.running(), format!("exit {:?}: {}", p.code(), p.text().trim()));
    s.check("and isn't printed again", p.lines().is_empty(), format!("{:?}", p.lines()));
    s.send_line(&chat.0, &chat.1, "pong");
    s.check("a new message exits it", p.exits_ok(20.0), p.text().trim());
    s.check("it printed the new line only", p.lines() == strings(&["t-chat: done — did: pong"]), format!("{:?}", p.lines()));
    p.stop();

    // 3: while no watch runs: t-busy turns done, t-chat blocked, t-boom's Claude quits, t-done closes
    let busy = agent_pane(s, "t-busy");
    s.send_line(&busy.0, &busy.1, "/state done | busy no more");
    s.send_line(&chat.0, &chat.1, "/state blocked permission | allow rm?");
    let boom = agent_pane(s, "t-boom");
    s.send_line(&boom.0, &boom.1, "/exit");
    sleep(1.0);
    s.send_line(&boom.0, &boom.1, ""); // Enter: keep its worktree
    let gone = s.cbar(&["rm", "t-done"], Some("alpha"));
    s.check("t-done closed", gone.code == 0, gone.to_string());
    // (Rex keeps a quit Claude's last status on its pane: its process is what's gone)
    let boom_mark = format!("{}/claude -n t-boom", s.stub.display());
    let boom_runs = || !pgrep(&boom_mark).is_empty();
    let alpha = s.session(Some("alpha"));
    let states = || -> BTreeMap<String, String> { s.panes().into_iter().filter(|x| x.session == alpha).map(|x| (x.tab, x.state)).collect() };
    let settled = wait_for(30.0, || {
        let st = states();
        st.get("t-busy").map(String::as_str) == Some("done")
            && st.get("t-chat").map(String::as_str) == Some("blocked")
            && !st.contains_key("t-done")
            && !boom_runs()
    });
    s.check("the changes took", settled, format!("{:?}, t-boom's Claude {}", states(), if boom_runs() { "runs" } else { "quit" }));
    let mut p = start();
    s.check("the next --next exits on them", p.exits_ok(30.0), p.text().trim());
    let want = strings(&["t-boom: stopped", "t-busy: done — busy no more", "t-chat: blocked (permission) — allow rm?", "t-done: closed — finished the thing"]);
    s.check("it printed each event between the runs", p.lines() == want, format!("{:?}", p.lines()));
    p.stop();

    // 4: and the run after that has nothing to say: none twice
    let mut p = start();
    sleep(12.0);
    s.check("the run after waits", p.running(), format!("exit {:?}", p.code()));
    s.check("and repeats nothing", p.lines().is_empty(), format!("{:?}", p.lines()));
    p.stop();
    let errs: String = logs.iter().map(|l| read(l.with_extension("err"))).collect();
    s.check("--next wrote no errors", errs.trim().is_empty(), errs.trim());
}

/// The watch guard, after next_cases (whose runs printed all that stood):
/// with no watch running, `cbar ls` and both switchboards flag alpha's
/// operator, with the events waiting for it, and its Stop hook (the stub runs
/// it as Claude Code does) tells it so at the end of its turn, once. While a
/// `cbar watch --next` runs, none of that; once the watch is killed, it's back.
fn guard_cases(s: &Sandbox) {
    let busy = agent_pane(s, "t-busy");
    s.send_line(&busy.0, &busy.1, "again");
    let alpha = s.session(Some("alpha"));
    let done = wait_for(20.0, || s.panes().iter().any(|p| p.session == alpha && p.tab == "t-busy" && p.state == "done" && s.screen(&busy.0, &busy.1).contains("> again")));
    s.check("guard: t-busy is done again", done, "");
    sleep(1.0);
    let waiting = "t-busy: done — did: again";

    let ls = s.cbar(&["ls"], None);
    let row = |name: &str| ls.out.lines().find(|l| l.trim_start_matches(['├', '└', '─', ' ']).starts_with(name)).unwrap_or("").to_string();
    s.check("guard: ls flags alpha's operator, with what waits", row("alpha").contains("operator idle · no watch (1 waiting)"), row("alpha"));
    s.check("guard: and the supervisor (nothing waits)", row(EXCHANGE).ends_with("supervisor idle · no watch"), row(EXCHANGE));
    s.check("guard: never an agent", !ls.out.lines().any(|l| l.contains("t-") && l.contains("no watch")), &ls.out);
    let board = s.run(&["switchboard", "--session", &alpha, "--once"], None, 60.0);
    s.check("guard: the project switchboard flags it", board.out.contains("no watch (1 waiting)") && board.out.contains("1 no watch"), &board.out);
    let board = s.run(&["switchboard", "--exchange", EXCHANGE, "--once"], None, 60.0);
    s.check("guard: the exchange switchboard flags all three", board.out.contains("3 no watch") && board.out.contains("no watch (1 waiting)"), &board.out);

    let check = s.cbar(&["watch", "--check"], Some("alpha"));
    let v: serde_json::Value = serde_json::from_str(check.out.trim()).unwrap_or_default();
    let fed = &v["hookSpecificOutput"];
    let reason = fed["additionalContext"].as_str().unwrap_or("");
    s.check("guard: the Stop hook keeps Claude going", check.code == 0 && fed["hookEventName"] == "Stop" && !reason.is_empty(), check.to_string());
    let want = format!("cbar watch isn't running for {alpha}; 1 event is waiting: {waiting}. Start `cbar watch --next` as a background Bash command");
    s.check("guard: saying what waits", reason.starts_with(&want), reason);

    // the operator's turn ends: its Stop hook (crossbar's, from --settings) tells it, once
    let (osid, opane) = s.panes_at["alpha"].clone();
    s.send_line(&osid, &opane, "hello");
    // (the pane wraps the long line)
    let told = wait_for(30.0, || s.screen(&osid, &opane).replace('\n', "").contains(&format!("stop hook: {want}")));
    s.check("guard: the operator is told when its turn ends", told, last(&s.screen(&osid, &opane), 600));
    sleep(3.0);
    s.check("guard: once a turn (stop_hook_active)", !s.screen(&osid, &opane).contains("blocked again"), last(&s.screen(&osid, &opane), 600));

    // it starts a watch: the first run prints what waits and exits; the next one waits, and holds the lease
    let mut p = s.spawn(&["cbar", "watch", "--next"], Some("alpha"), &[], &s.root.join("guard-1.out"));
    s.check("guard: a --next prints what waited", p.exits_ok(20.0) && p.lines() == strings(&[waiting]), p.text().trim());
    let mut p = s.spawn(&["cbar", "watch", "--next"], Some("alpha"), &[], &s.root.join("guard-2.out"));
    let quiet = wait_for(20.0, || s.cbar(&["watch", "--check"], Some("alpha")).out.trim().is_empty());
    s.check("guard: while a watch runs, the Stop hook says nothing", quiet && p.running(), s.cbar(&["watch", "--check"], Some("alpha")).to_string());
    let ls = s.cbar(&["ls"], None);
    let alpha_row = ls.out.lines().find(|l| l.contains("alpha")).unwrap_or("").to_string();
    s.check("guard: nor does ls", alpha_row.contains("operator done") && !alpha_row.contains("no watch"), &alpha_row);
    let before = s.screen(&osid, &opane).matches("stop hook:").count();
    s.send_line(&osid, &opane, "hello again");
    let turned = wait_for(30.0, || s.screen(&osid, &opane).contains("did: hello again") || s.screen(&osid, &opane).contains("> hello again"));
    sleep(5.0);
    let after = s.screen(&osid, &opane).matches("stop hook:").count();
    s.check("guard: and its turn ends without a word", turned && after == before, last(&s.screen(&osid, &opane), 600));

    // the watch is killed (no exit of its own, its lease left behind): no watch again
    unsafe { libc::kill(-(p.child.id() as i32), libc::SIGKILL) };
    let _ = p.child.wait();
    let check = s.cbar(&["watch", "--check"], Some("alpha"));
    let v: serde_json::Value = serde_json::from_str(check.out.trim()).unwrap_or_default();
    let reason = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or("");
    s.check("guard: a killed watch is no watch", reason.starts_with(&format!("cbar watch isn't running for {alpha}; no events are waiting yet.")), check.to_string());
    let errs = read(s.root.join("guard-1.err")) + &read(s.root.join("guard-2.err"));
    s.check("guard: the watches wrote no errors", errs.trim().is_empty(), errs.trim());
}

fn cases(s: &Sandbox) {
    let mut env = Vec::new();
    let mut flag = None;
    if s.rex {
        let (d, f) = flaky_rex(s);
        env.push(("PATH", format!("{}:{}", d.display(), s.env["PATH"])));
        flag = Some(f);
    }
    let mut p = s.spawn(&["cbar", "watch"], Some(EXCHANGE), &env, &s.root.join("watch.out"));
    sleep(8.0); // its startup report, and a look or two
    let start = p.text().len();
    if let Some(f) = &flag {
        fs::write(f, "").unwrap();
        sleep(10.0); // a few looks whose reads all fail
        let _ = fs::remove_file(f);
    }
    sleep(8.0);
    let blip = p.text()[start..].to_string();
    let name = if flag.is_some() { "idle operators whose reads fail aren't stopped" } else { "idle operators aren't stopped" };
    s.check(name, !blip.contains("stopped"), blip.trim());
    s.check("nor reported again once read", blip.trim().is_empty(), blip.trim());

    // beta's Claude quits: a real stop, reported within a look or two
    let (sid, pane) = s.panes_at["beta"].clone();
    let mark = p.text().len();
    s.send_line(&sid, &pane, "/exit");
    let t0 = Instant::now();
    let got = wait_for(20.0, || p.text()[mark..].contains("beta operator: stopped"));
    s.check("a Claude that quits is stopped", got, p.text()[mark..].trim());
    if got {
        let took = t0.elapsed().as_secs_f64();
        s.check("promptly (within 8s)", took <= 8.0, format!("{took:.1}s"));
    }
    s.check("alpha's operator isn't", !p.text()[mark..].contains("alpha operator"), p.text()[mark..].trim());

    // alpha's pane closes: closed
    let (asid, apane) = s.panes_at["alpha"].clone();
    let mark = p.text().len();
    s.close(&asid, &apane);
    let got = wait_for(20.0, || p.text()[mark..].contains("alpha operator: closed"));
    s.check("a closed pane is closed", got, p.text()[mark..].trim());

    p.stop();
    let err = p.err();
    s.check("watch wrote no errors", err.trim().is_empty(), err.trim());
    if std::env::var("CBAR_TEST_VERBOSE").is_ok() {
        say(&format!("watch printed:\n{}", p.text()));
    }
}
