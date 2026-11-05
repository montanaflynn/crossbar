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
