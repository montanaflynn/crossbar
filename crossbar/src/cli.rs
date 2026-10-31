//! cbar: crossbar, a team of Claude Code agents in Rex or tmux. Each exchange
//! has a supervisor (in the session named after it) and projects; each
//! project has a session `<exchange>/<project>` with its operator in the
//! first tab, and agents in other tabs.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::launch::{self, pane_argv};
use crate::layout::{self, Home, OPERATOR, SUPERVISOR};
use crate::mux::{self, Term};
use crate::paths::{self, Node, Want};
use crate::restart;
use crate::status::{self, Flat, Looker};
use crate::util::{self, die, short, strings};

const REPO: &str = "https://github.com/montanaflynn/crossbar";

pub fn usage() -> String {
    "usage: cbar COMMAND

A PATH is exchange/project/agent, relative to where you are: inside a project,
`fix-x` is its agent; inside an exchange (or outside crossbar, in the default
exchange), `stats` is its project. `/central/stats` is absolute, `..` goes up.

  new PROJECT [DIR] [--tmux|--rex]   register a project and start its session with an operator
                                     and a local switchboard (DIR: this folder; terminal: the
                                     one you're in, else Rex if it's running)
  new PROJECT/AGENT [BRIEF]          start Claude in a new tab of that project on its brief,
      [--no-worktree]                <project>/briefs/AGENT.md (BRIEF is copied there); in its
                                     own git worktree when the project is a git repo
  new --agent AGENT [BRIEF]          the same in the exchange's own session (the supervisor's)
  new --exchange NAME [DIR]          create an exchange and start its supervisor and main board
      [--tmux|--rex]                 in DIR (`path` in its exchange.toml; default: its own
                                     folder under ~/.crossbar/exchanges/), where its agents start
  up [EXCHANGE] [--tmux|--rex]       start the exchange's supervisor and main switchboard and
                                     every project's session and operator that isn't running;
                                     leaves whatever runs alone
  ls [PATH]                          exchanges > projects > agents, with their status
  open [-n] PATH                     switch to that session (an agent: its tab)
  send PATH TEXT                     type TEXT and Enter into its Claude, only if idle or done
                                     (a project: its operator; an exchange: its supervisor)
  status PATH                        its Claude's state
  tail PATH [N]                      the last N lines of its screen (default 60)
  wait PATH [SECONDS]                wait until it stops working (default 600)
  restart PATH                       quit its Claude (only if idle or done; your own: once this
                                     turn ends) and start it again in its pane with today's role
                                     prompt, resuming its conversation
  rm [-y] [-f] PATH                  an agent: close its tab, and remove its worktree if clean and
                                     merged (refuses while it works, is blocked or has uncommitted
                                     changes, unless -f); a project or an exchange: kill its
                                     session and unregister it, briefs and reports too (asks first)
  watch                              a line each time one of your reports turns done, blocked or
                                     error, stops or closes (operators: their agents; supervisors:
                                     their agents and their projects' operators)
  board [EXCHANGE|PROJECT] [--all]   the switchboard (--once prints once, --json prints JSON)
  update                             install the latest crossbar from GitHub
  version                            the installed version
  run supervisor|operator|board|agent  what cbar starts in the panes it makes
  help                               this text

Old forms still work, with a note: cbar operator, cbar supervisor, cbar agent …, cbar board --here."
        .into()
}

fn agent_usage() -> String {
    "usage: cbar agent COMMAND    (deprecated: each has a path-based form)

  new TASK [--no-worktree]     cbar new TASK
  ls                           cbar ls .
  status TASK                  cbar status TASK
  tail TASK [N]                cbar tail TASK [N]
  send TASK TEXT               cbar send TASK TEXT
  wait TASK [SECONDS]          cbar wait TASK [SECONDS]
  close [-f] TASK              cbar rm [-f] TASK"
        .into()
}

fn deprecated(old: &str, new: &str) {
    eprintln!("cbar: `{old}` is deprecated; use `{new}`");
}

pub fn main(args: Vec<String>) {
    let home = Home::from_env();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r.to_vec()),
        None => ("help", Vec::new()),
    };
    match cmd {
        "new" => cmd_new(&home, rest),
        "up" => cmd_up(&home, rest),
        "ls" => cmd_ls(&home, rest),
        "open" => cmd_open(&home, rest),
        "send" => cmd_send(&home, rest),
        "status" => cmd_status(&home, rest),
        "tail" => cmd_tail(&home, rest),
        "wait" => cmd_wait(&home, rest),
        "restart" => cmd_restart(&home, rest),
        "rm" => cmd_rm(&home, rest),
        "watch" => cmd_watch(),
        "board" => cmd_board(&home, rest),
        "run" => cmd_run(&home, rest),
        "update" => cmd_update(&home),
        "version" | "--version" => println!("{}", version(&home)),
        "supervisor" => {
            deprecated("cbar supervisor", "cbar run supervisor");
            cmd_run(&home, [vec!["supervisor".into()], rest].concat())
        }
        "operator" => {
            deprecated("cbar operator", "cbar run operator");
            cmd_run(&home, [vec!["operator".into()], rest].concat())
        }
        "agent" => cmd_agent(&home, rest),
        "help" | "-h" | "--help" => println!("{}", usage()),
        _ => {
            eprintln!("cbar: unknown command '{cmd}'");
            eprintln!("{}", usage());
            std::process::exit(1)
        }
    }
}

/// Flags and positionals of ARGS; FLAGS are the flags it takes.
struct Args {
    flags: Vec<String>,
    pos: Vec<String>,
}

fn parse(args: &[String], flags: &[&str], usage: &str) -> Args {
    let mut out = Args { flags: Vec::new(), pos: Vec::new() };
    let mut rest = false;
    for a in args {
        if rest || !a.starts_with('-') || a == "-" {
            out.pos.push(a.clone());
        } else if a == "--" {
            rest = true;
        } else if flags.contains(&a.as_str()) {
            out.flags.push(a.clone());
        } else {
            die(format!("unknown option '{a}' (usage: {usage})"));
        }
    }
    out
}

impl Args {
    fn has(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }

    fn term(&self) -> Option<Term> {
        if self.has("--tmux") {
            Some(Term::Tmux)
        } else if self.has("--rex") {
            Some(Term::Rex)
        } else {
            None
        }
    }
}

/// The terminal for a new session: the one you're in, else Rex if it's
/// running, else tmux.
fn default_terminal() -> Term {
    mux::here_terminal().unwrap_or_else(|| if mux::rex(&strings(&["ls", "--json"])).0 == 0 { Term::Rex } else { Term::Tmux })
}

fn resolve(home: &Home, path: &str, want: Want) -> Node {
    let here = paths::here(home);
    paths::resolve(home, &here, path, want, &paths::tab_exists).unwrap_or_else(|e| die(e))
}

/// The terminal a node's session is recorded in, if any.
fn recorded_term(home: &Home, node: &Node) -> Option<Term> {
    let e = node.exchange()?;
    match node.project() {
        Some(p) => home.project(e, p).map(|p| p.terminal),
        None => home.exchange(e).and_then(|x| x.terminal),
    }
}

/// A running session NAME: its terminal and id, looking in PREF first.
fn find_session(name: &str, pref: Option<Term>) -> Option<(Term, String)> {
    let first = pref.or_else(mux::here_terminal).unwrap_or(Term::Rex);
    let other = if first == Term::Rex { Term::Tmux } else { Term::Rex };
    for t in [first, other] {
        let id = match t {
            Term::Rex => mux::rex_session_id(name),
            Term::Tmux => mux::tmux_sessions().into_iter().find(|(_, n)| n == name).map(|(i, _)| i),
        };
        if let Some(id) = id {
            return Some((t, id));
        }
    }
    None
}

/// A pane-level target: the session and tab of a node's Claude.
struct Target {
    term: Term,
    session: String,
    sid: String,
    tab: String,
    path: String,
}

fn target(home: &Home, node: &Node) -> Target {
    let session = node.session().unwrap_or_else(|| die("that's the root; name an exchange, project or agent"));
    let (term, sid) = find_session(&session, recorded_term(home, node))
        .unwrap_or_else(|| die(format!("session '{session}' isn't running (cbar up)")));
    Target { term, session, sid, tab: node.tab().unwrap().to_string(), path: node.path() }
}

impl Target {
    fn panes(&self) -> Vec<Flat> {
        status::flatten(Looker::new(false).snapshot(Some(&self.sid)))
    }

    /// The pane holding this tab's program (not its switchboard).
    fn find(&self) -> Option<Flat> {
        self.panes().into_iter().find(|f| f.pane.tab == self.tab && f.pane.label != "switchboard")
    }

    fn get(&self) -> Flat {
        self.find().unwrap_or_else(|| die(format!("no tab '{}' in session '{}' (cbar ls)", self.tab, self.session)))
    }
}

// ---- new -------------------------------------------------------------------

fn cmd_new(home: &Home, args: Vec<String>) {
    let usage = "cbar new PATH [DIR|BRIEF] [--tmux|--rex] [--no-worktree] [--agent] [--exchange]";
    let a = parse(&args, &["--tmux", "--rex", "--no-worktree", "--agent", "--exchange"], usage);
    let Some(path) = a.pos.first() else { die(format!("usage: {usage}")) };
    home.require_layout();
    if a.has("--exchange") {
        return new_exchange(home, path.trim_matches('/'), a.pos.get(1).map(String::as_str), a.term());
    }
    // a first project on a fresh install: the default exchange comes with it
    if home.exchanges().is_empty() && !path.starts_with('/') {
        let e = home.default_exchange();
        home.register_exchange(&e, a.term().unwrap_or_else(default_terminal), None)
            .unwrap_or_else(|err| die(format!("could not register exchange '{e}': {err}")));
        if !home.config().exists() {
            let _ = home.set_default_exchange(&e);
        }
        println!("created exchange '{e}' (cbar up {e} starts its supervisor)");
    }
    let node = resolve(home, path, Want::New { agent: a.has("--agent") });
    let extra = a.pos.get(1).map(String::as_str);
    match &node {
        Node::Root => die(format!("usage: {usage}")),
        Node::Exchange(e) => new_exchange(home, e, extra, a.term()),
        Node::Project(e, p) => new_project(home, e, p, extra, a.term()),
        Node::Agent { exchange, project, name } => new_agent(home, exchange, project.as_deref(), name, extra, !a.has("--no-worktree"), !path.contains('/') && project.is_some()),
    }
}

fn new_exchange(home: &Home, e: &str, dir: Option<&str>, term: Option<Term>) {
    if !layout::valid_name(e) {
        die("an exchange name is letters, digits, '_' and '-'");
    }
    let dir = dir.map(|d| {
        let d = layout::expand_home(d);
        d.canonicalize().ok().filter(|c| c.is_dir()).unwrap_or_else(|| die(format!("no folder {}", d.display())))
    });
    let term = term.or_else(|| home.exchange(e).and_then(|x| x.terminal)).unwrap_or_else(default_terminal);
    let fresh = !home.exchange_exists(e);
    home.register_exchange(e, term, dir.as_deref()).unwrap_or_else(|err| die(format!("could not register '{e}': {err}")));
    if !home.config().exists() {
        let _ = home.set_default_exchange(e);
    }
    if fresh {
        println!("created exchange '{e}' in {}", short(home.exchange_dir(e)));
    }
    if mux::session_running(e, term) {
        println!("session '{e}' already exists; registered it");
        return;
    }
    let dir = exchange_dir(home, e).unwrap_or_else(|err| die(err));
    if let Err(err) = start_exchange(e, &dir, term) {
        die(err);
    }
}

/// The folder exchange E's supervisor and agents start in (`path` in its
/// exchange.toml, else its own folder), or why there's none.
fn exchange_dir(home: &Home, e: &str) -> Result<String, String> {
    let dir = home.exchange_work_dir(e);
    if !dir.is_dir() {
        return Err(format!("exchange '{e}': no folder {} (its path in {})", short(&dir), short(home.exchange_toml(e))));
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Session E in DIR: tab supervisor running `cbar run supervisor`, and the
/// main switchboard at 30% to its right.
fn start_exchange(e: &str, dir: &str, term: Term) -> Result<(), String> {
    let root = format!("/{e}");
    let made = mux::new_session(term, e, dir, SUPERVISOR, &pane_argv(&["supervisor", &root]))?;
    if let Err(err) = mux::split(term, e, &made.pane, dir, 30, false, "switchboard", &pane_argv(&["board", &root])) {
        util::warn(format!("could not add the switchboard pane: {err}"));
    }
    println!("created session '{e}' in {} with a supervisor and the main switchboard{}", short(dir), if term == Term::Tmux { " (tmux)" } else { "" });
    Ok(())
}

fn new_project(home: &Home, e: &str, p: &str, dir: Option<&str>, term: Option<Term>) {
    if !layout::valid_name(p) {
        die("a project name is letters, digits, '_' and '-'");
    }
    if p == SUPERVISOR {
        die(format!("'{SUPERVISOR}' is the exchange's own tab, not a project name"));
    }
    if !home.exchange_exists(e) {
        die(format!("no exchange '{e}' (cbar new --exchange {e})"));
    }
    let dir = match dir {
        Some(d) => PathBuf::from(d),
        None => std::env::current_dir().unwrap_or_else(|_| die("no current folder")),
    };
    let dir = dir.canonicalize().ok().filter(|d| d.is_dir()).unwrap_or_else(|| die(format!("no folder {}", dir.display())));
    let term = term.unwrap_or_else(default_terminal);
    home.register_project(e, p, &dir, term).unwrap_or_else(|err| die(format!("could not register '{p}': {err}")));
    let session = layout::session_of(e, Some(p));
    if mux::session_running(&session, term) {
        println!("session '{session}' already exists; registered it ({})", short(&dir));
        return;
    }
    // as cbar up does: no second session next to one still under its 0.4 name
    if e == home.default_exchange() && mux::session_running(p, term) {
        println!("registered it ({}); its session runs as '{p}' (its 0.4 name): rename that session to '{session}'", short(&dir));
        return;
    }
    if let Err(err) = start_project(&session, &dir.to_string_lossy(), term) {
        die(err);
    }
}

/// Session E/P: tab operator running `cbar run operator`, and its local
/// switchboard at 30% to its right.
fn start_project(session: &str, dir: &str, term: Term) -> Result<(), String> {
    let root = format!("/{session}");
    let made = mux::new_session(term, session, dir, OPERATOR, &pane_argv(&["operator", &root]))?;
    if let Err(err) = mux::split(term, session, &made.pane, dir, 30, false, "switchboard", &pane_argv(&["board", &root])) {
        util::warn(format!("could not add the switchboard pane: {err}"));
    }
    if term == Term::Tmux {
        println!("created session '{session}' in {} with an operator and a switchboard (tmux)", short(dir));
        let how = if util::env("TMUX").is_some() { "switch to it" } else { "attach" };
        println!("{how}: cbar open /{session}");
    } else {
        println!("created session '{session}' in {} with an operator and a switchboard", short(dir));
    }
    Ok(())
}

/// The folder an exchange's or project's agents start in.
fn work_dir(home: &Home, e: &str, p: Option<&str>) -> Option<String> {
    match p {
        Some(p) => home.project(e, p).map(|x| x.path),
        None => Some(home.exchange_work_dir(e).to_string_lossy().into_owned()),
    }
}

fn is_git_repo(dir: &str) -> bool {
    util::run("git", &strings(&["-C", dir, "rev-parse", "--is-inside-work-tree"]), Duration::from_secs(5)).0 == 0
}

fn new_agent(home: &Home, e: &str, p: Option<&str>, name: &str, brief_from: Option<&str>, mut worktree: bool, hint: bool) {
    if !layout::valid_task(name) {
        die("a task name is letters, digits, '.', '_' and '-'");
    }
    if name == OPERATOR || name == SUPERVISOR {
        die(format!("'{name}' is a reserved name"));
    }
    let node = Node::Agent { exchange: e.into(), project: p.map(String::from), name: name.into() };
    let t = target(home, &node);
    let brief = home.dir_of(e, p).join("briefs").join(format!("{name}.md"));
    if let Some(src) = brief_from {
        let src = Path::new(src);
        let text = fs::read(src).unwrap_or_else(|err| die(format!("{}: {err}", src.display())));
        let same = src.canonicalize().ok() == brief.canonicalize().ok();
        if !same {
            if brief.exists() && fs::read(&brief).ok().as_deref() != Some(&text[..]) {
                die(format!("there's already a different brief at {}", short(&brief)));
            }
            fs::create_dir_all(brief.parent().unwrap()).ok();
            fs::write(&brief, &text).unwrap_or_else(|err| die(format!("{}: {err}", short(&brief))));
        }
    }
    if !brief.is_file() {
        let hint = if hint { format!(" (a project, not an agent? cbar new ../{name})") } else { String::new() };
        die(format!("write the brief first: {}{hint}", short(&brief)));
    }
    if t.find().is_some() {
        die(format!("session '{}' already has a tab '{name}'", t.session));
    }
    let dir = match p {
        Some(_) => work_dir(home, e, p).filter(|d| Path::new(d).is_dir()).unwrap_or_else(|| die("its project folder is gone")),
        None => exchange_dir(home, e).unwrap_or_else(|err| die(err)),
    };
    let mut note = String::new();
    if worktree && !is_git_repo(&dir) {
        worktree = false;
        note = format!(" (no worktree: {} isn't a git repo)", short(&dir));
    }
    let path = format!("/{}", node.path());
    let mut sub = vec!["agent", path.as_str()];
    if !worktree {
        sub.push("--no-worktree");
    }
    match mux::new_tab(t.term, &t.session, name, &dir, &pane_argv(&sub)) {
        Ok(m) => println!("started {name} in a new tab ({}){note}", m.pane),
        Err(err) => die(err),
    }
}

// ---- run (what panes run) ----------------------------------------------------

/// Take `FLAG VALUE` (or `FLAG=VALUE`) out of ARGS, before any `--`.
fn take_value(args: &mut Vec<String>, flag: &str, usage: &str) -> Option<String> {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let i = args[..end].iter().position(|a| a == flag || a.starts_with(&format!("{flag}=")))?;
    let a = args.remove(i);
    if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
        return Some(v.to_string());
    }
    if i >= args.len() || args[i] == "--" {
        die(format!("{flag} takes a value (usage: {usage})"));
    }
    Some(args.remove(i))
}

fn cmd_run(home: &Home, mut args: Vec<String>) {
    let usage = "cbar run [--pane] supervisor|operator|board|agent [PATH] [--no-worktree] [--resume SESSION]";
    let resume = take_value(&mut args, "--resume", usage);
    let resume: Vec<String> = resume.map(|id| vec!["--resume".into(), id]).unwrap_or_default();
    let a = parse(&args, &["--pane", "--no-worktree"], usage);
    let pane = a.has("--pane");
    if pane {
        // a pane started for this: if it fails, keep the message on screen
        util::on_die(launch::login_shell);
    }
    let Some(role) = a.pos.first().map(String::as_str) else { die(format!("usage: {usage}")) };
    let node = match a.pos.get(1) {
        Some(p) => {
            home.require_layout();
            resolve(home, p, if role == "agent" { Want::New { agent: true } } else { Want::Existing })
        }
        None => paths::here(home),
    };
    match role {
        "supervisor" => {
            home.require_layout();
            let e = node.exchange().unwrap_or_else(|| die("no exchange here")).to_string();
            let dir = home.exchange_dir(&e);
            for d in ["briefs", "reports", "projects"] {
                let _ = fs::create_dir_all(dir.join(d));
            }
            let prompt = format!(
                "{}\n\nYour exchange is `{e}` (session `{e}`): briefs go in `{}/briefs/`, reports in `{}/reports/`; its projects are registered in `{}/projects/`.",
                home.role_prompt("supervisor").trim_end(),
                short(&dir),
                short(&dir),
                short(&dir)
            );
            claude(&[format!("{e} supervisor"), prompt], &resume, pane)
        }
        "operator" => {
            home.require_layout();
            let (e, p) = match &node {
                Node::Project(e, p) => (e.clone(), p.clone()),
                Node::Agent { exchange, project: Some(p), .. } => (exchange.clone(), p.clone()),
                _ => {
                    // not in a project's session: the folder's name, as before
                    let cwd = std::env::current_dir().unwrap_or_default();
                    (home.default_exchange(), util::basename(&cwd.to_string_lossy()).to_string())
                }
            };
            let dir = home.project_dir(&e, &p);
            let _ = fs::create_dir_all(dir.join("briefs"));
            let _ = fs::create_dir_all(dir.join("reports"));
            let prompt = format!(
                "{}\n\nYour project is `{p}` in the exchange `{e}` (session `{e}/{p}`): briefs go in `{}/briefs/`, reports in `{}/reports/`.",
                home.role_prompt("operator").trim_end(),
                short(&dir),
                short(&dir)
            );
            claude(&[format!("{p} operator"), prompt], &resume, pane)
        }
        "agent" => {
            let Node::Agent { exchange, project, name } = &node else { die("cbar run agent PATH: PATH is an agent") };
            let brief = home.dir_of(exchange, project.as_deref()).join("briefs").join(format!("{name}.md"));
            let mut extra = Vec::new();
            if !a.has("--no-worktree") {
                extra = vec!["-w".to_string(), name.clone()];
            }
            let (mut argv, envs) = launch::claude_cmd();
            argv.extend(["-n".to_string(), name.clone()]);
            argv.extend(extra);
            if resume.is_empty() {
                argv.push(format!("Read {} and do the task it describes.", brief.display()));
            } else {
                argv.extend(resume);
            }
            launch::run(&argv, &envs, pane)
        }
        "board" => {
            let mut argv = vec![switchboard_path()];
            match &node {
                Node::Project(..) | Node::Agent { project: Some(_), .. } => argv.push("--here".into()),
                Node::Exchange(e) | Node::Agent { exchange: e, project: None, .. } => argv.extend(["--exchange".to_string(), e.clone()]),
                Node::Root => {}
            }
            launch::run(&argv, &[], pane)
        }
        _ => die(format!("usage: {usage}")),
    }
}

/// Run Claude named NAME with PROMPT appended to its system prompt.
fn claude(name_prompt: &[String; 2], more: &[String], pane: bool) -> ! {
    let (mut argv, envs) = launch::claude_cmd();
    argv.extend(["-n".to_string(), name_prompt[0].clone(), "--append-system-prompt".into(), name_prompt[1].clone()]);
    argv.extend_from_slice(more);
    launch::run(&argv, &envs, pane)
}

fn switchboard_path() -> String {
    launch::sibling("switchboard").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "switchboard".into())
}

// ---- up --------------------------------------------------------------------

const SHELLS: [&str; 9] = ["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish"];

fn cmd_up(home: &Home, args: Vec<String>) {
    let a = parse(&args, &["--tmux", "--rex"], "cbar up [EXCHANGE] [--tmux|--rex]");
    home.require_layout();
    let e = match a.pos.first() {
        Some(e) => e.trim_matches('/').to_string(),
        None => paths::here(home).exchange().map(String::from).unwrap_or_else(|| home.default_exchange()),
    };
    if !layout::valid_name(&e) {
        die(format!("no exchange '{e}'"));
    }
    let term = a.term().or_else(|| home.exchange(&e).and_then(|x| x.terminal)).unwrap_or_else(default_terminal);
    let fresh = !home.exchange_exists(&e);
    home.register_exchange(&e, term, None).unwrap_or_else(|err| die(format!("could not register '{e}': {err}")));
    if fresh {
        println!("created exchange '{e}' in {}", short(home.exchange_dir(&e)));
        if !home.config().exists() {
            let _ = home.set_default_exchange(&e);
        }
    }
    let all = status::flatten(Looker::new(false).snapshot(None));
    let legacy = e == home.default_exchange();

    match exchange_dir(home, &e) {
        Ok(dir) => up_one(&all, &e, &dir, term, SUPERVISOR, &format!("/{e}")),
        Err(err) => util::warn(format!("{err}; skipped its supervisor")),
    }
    for p in home.projects(&e) {
        if !Path::new(&p.path).is_dir() {
            util::warn(format!("{}: no folder {}; skipped it", p.name, short(&p.path)));
            continue;
        }
        let session = p.session();
        if legacy && !mux::session_running(&session, p.terminal) && all.iter().any(|f| f.group == p.name && f.mux == p.terminal.as_str()) {
            println!("{session}: running as '{}' (its 0.4 name): rename that session to '{session}'", p.name);
            continue;
        }
        up_one(&all, &session, &p.path, p.terminal, OPERATOR, &format!("/{session}"));
    }
}

/// Start session NAME if it's missing; if it runs, start ROLE in its tab
/// ROLE when that tab is missing or only has a shell. ALL holds the panes.
fn up_one(all: &[Flat], name: &str, dir: &str, term: Term, role: &str, path: &str) {
    if !mux::session_running(name, term) {
        let made = if role == SUPERVISOR { start_exchange(name, dir, term) } else { start_project(name, dir, term) };
        if let Err(err) = made {
            util::warn(format!("{name}: {err}"));
        }
        return;
    }
    let mine: Vec<&Flat> = all.iter().filter(|f| f.group == name && f.mux == term.as_str()).collect();
    let pane = mine.iter().find(|f| f.pane.tab == role && f.pane.label != "switchboard");
    let argv = pane_argv(&[role, path]);
    match pane {
        None => match mux::new_tab(term, name, role, dir, &argv) {
            Ok(m) => {
                // its switchboard went with the tab: put one back next to it
                if !mine.iter().any(|f| f.pane.tab == role) {
                    let session = if term == Term::Rex { mux::rex_session_id(name).unwrap_or_default() } else { name.to_string() };
                    if let Err(err) = mux::split(term, &session, &m.pane, dir, 30, false, "switchboard", &pane_argv(&["board", path])) {
                        util::warn(format!("{name}: could not add the switchboard pane: {err}"));
                    }
                }
                println!("{name}: started its {role} in a new tab");
            }
            Err(err) => util::warn(format!("{name}: could not open a {role} tab: {err}")),
        },
        Some(f) if f.pane.state.is_empty() && SHELLS.contains(&f.pane.program.as_str()) => {
            let neighbor = mine.iter().find(|g| g.pane.window_id == f.pane.window_id && g.pane.block_id != f.pane.block_id);
            let session = mine.first().map(|g| g.pane.session_id.clone()).unwrap_or_default();
            let target_session = if term == Term::Rex { session.as_str() } else { name };
            match mux::respawn(term, target_session, &f.pane.block_id, &f.pane.window_id, neighbor.map(|g| g.pane.block_id.as_str()), role, dir, 70, &argv) {
                Ok(()) => println!("{name}: started its {role}"),
                Err(err) => util::warn(format!("{name}: could not start its {role}: {err}")),
            }
        }
        Some(_) => println!("{name}: running"),
    }
}

// ---- ls --------------------------------------------------------------------

struct Tree<'a> {
    all: &'a [Flat],
    rows: Vec<Vec<String>>,
}

impl Tree<'_> {
    fn session_panes(&self, name: &str, term: Term) -> Vec<&Flat> {
        self.all.iter().filter(|f| f.group == name && f.mux == term.as_str()).collect()
    }

    /// The SESSION and CLAUDE columns for session NAME's tab TAB; LEGACY is
    /// its 0.4 name, looked at when NAME doesn't run.
    fn state(&self, name: &str, term: Term, tab: &str, legacy: Option<&str>) -> (String, String) {
        let mut panes = self.session_panes(name, term);
        let mut session = "running".to_string();
        if panes.is_empty() {
            if let Some(old) = legacy {
                panes = self.session_panes(old, term);
                session = format!("running as '{old}' (0.4 name)");
            }
        }
        if panes.is_empty() {
            return ("not running".into(), "-".into());
        }
        let in_tab: Vec<_> = panes.iter().filter(|f| f.pane.tab == tab).collect();
        let st = if in_tab.is_empty() {
            format!("no {tab} tab")
        } else {
            let s = in_tab.iter().find(|f| f.pane.label != "switchboard").map(|f| f.pane.state.clone()).unwrap_or_default();
            format!("{tab} {}", if s.is_empty() { "not running".into() } else { s })
        };
        (session, st)
    }

    /// Agent rows (tabs other than TAB with a Claude in them) of a session.
    fn agents(&self, name: &str, term: Term, skip: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for f in self.session_panes(name, term) {
            let p = &f.pane;
            if p.tab == skip || p.state.is_empty() || p.label == "switchboard" || out.iter().any(|(t, _)| t == &p.tab) {
                continue;
            }
            let st = if p.since.is_empty() { p.state.clone() } else { format!("{} {}", p.state, p.since) };
            out.push((p.tab.clone(), st));
        }
        out
    }

    fn row(&mut self, prefix: &str, name: &str, term: &str, path: &str, session: &str, claude: &str) {
        self.rows.push(strings(&[&format!("{prefix}{name}"), term, path, session, claude]));
    }

    fn project(&mut self, p: &layout::Project, lead: &str, branch: &str, legacy: bool) {
        let session = p.session();
        let (s, c) = self.state(&session, p.terminal, OPERATOR, legacy.then_some(p.name.as_str()));
        self.row(&format!("{lead}{branch}"), &p.name, p.terminal.as_str(), &short(&p.path), &s, &c);
        let under = format!("{lead}{}", match branch {
            "├─ " => "│  ",
            "└─ " => "   ",
            _ => "",
        });
        let mut agents = self.agents(&session, p.terminal, OPERATOR);
        if agents.is_empty() && legacy {
            agents = self.agents(&p.name, p.terminal, OPERATOR);
        }
        let n = agents.len();
        for (i, (tab, st)) in agents.into_iter().enumerate() {
            let b = if i + 1 < n { "├─ " } else { "└─ " };
            self.row(&format!("{under}{b}"), &tab, "", "", "", &st);
        }
    }

    fn exchange(&mut self, home: &Home, x: &layout::Exchange, only_project: Option<&str>) {
        let legacy = x.name == home.default_exchange();
        let has = |g: &str, t: Term| self.all.iter().any(|f| f.group == g && f.mux == t.as_str());
        let term = x.terminal.unwrap_or_else(|| if has(&x.name, Term::Tmux) { Term::Tmux } else { Term::Rex });
        let projects: Vec<_> = home.projects(&x.name).into_iter().filter(|p| only_project.is_none_or(|o| o == p.name)).collect();
        if let Some(only) = only_project {
            if let Some(p) = projects.first() {
                self.project(p, "", "", legacy);
            } else {
                die(format!("no project '{}/{only}'", x.name));
            }
            return;
        }
        let (s, c) = self.state(&x.name, term, SUPERVISOR, None);
        self.row("", &x.name, term.as_str(), &short(home.exchange_work_dir(&x.name)), &s, &c);
        let agents = self.agents(&x.name, term, SUPERVISOR);
        let n = projects.len() + agents.len();
        let mut i = 0;
        for p in &projects {
            i += 1;
            self.project(p, "", if i < n { "├─ " } else { "└─ " }, legacy);
        }
        for (tab, st) in agents {
            i += 1;
            self.row(if i < n { "├─ " } else { "└─ " }, &tab, "", "", "", &st);
        }
    }
}

fn cmd_ls(home: &Home, args: Vec<String>) {
    let a = parse(&args, &[], "cbar ls [PATH]");
    home.require_layout();
    let node = a.pos.first().map(|p| resolve(home, p, Want::Existing));
    let exchanges = home.exchanges();
    if exchanges.is_empty() {
        println!("no exchanges yet (cbar new NAME [DIR] makes a project in '{}')", home.default_exchange());
        return;
    }
    let all = status::flatten(Looker::new(false).snapshot(None));
    let mut tree = Tree { all: &all, rows: vec![strings(&["NAME", "TERMINAL", "PATH", "SESSION", "CLAUDE"])] };
    match &node {
        None | Some(Node::Root) => {
            for x in &exchanges {
                tree.exchange(home, x, None);
            }
        }
        Some(Node::Exchange(e)) => tree.exchange(home, &home.exchange(e).unwrap(), None),
        Some(Node::Project(e, p)) => tree.exchange(home, &home.exchange(e).unwrap(), Some(p)),
        Some(n @ Node::Agent { .. }) => {
            let t = target(home, n);
            let f = t.get();
            let st = if f.pane.since.is_empty() { f.pane.state.clone() } else { format!("{} {}", f.pane.state, f.pane.since) };
            tree.row("", &t.path, t.term.as_str(), &f.pane.cwd, "running", if st.is_empty() { "-" } else { &st });
        }
    }
    print!("{}", util::columns(&tree.rows));
    if matches!(node, None | Some(Node::Root)) && exchanges.iter().all(|x| home.projects(&x.name).is_empty()) {
        println!("no projects registered (cbar new NAME [DIR])");
    }
}

// ---- open, send, status, tail, wait ------------------------------------------

fn cmd_open(home: &Home, args: Vec<String>) {
    let a = parse(&args, &["-n"], "cbar open [-n] PATH");
    let Some(path) = a.pos.first() else { die("usage: cbar open [-n] PATH") };
    home.require_layout();
    let node = resolve(home, path, Want::Existing);
    let t = target(home, &node);
    let (window, pane) = match node {
        Node::Agent { .. } => {
            let f = t.get();
            (Some(f.pane.window_id), Some(f.pane.block_id))
        }
        _ => (None, None),
    };
    let cmd: Vec<String> = match t.term {
        Term::Tmux => {
            let mut c = mux::tmux_argv();
            if let Some(w) = &window {
                c.extend(strings(&["select-window", "-t", w, ";"]));
            }
            let target = pane.clone().unwrap_or_else(|| format!("={}", t.session));
            if util::env("TMUX").is_some() {
                c.extend(strings(&["switch-client", "-t", &target]));
            } else {
                c.extend(strings(&["attach-session", "-t", &target]));
            }
            c
        }
        Term::Rex => {
            let client = mux::app_client().unwrap_or_else(|| die("no Rex app connected"));
            let mut c = strings(&["rex", "-C", &client, "do", "session.select", &format!("session_id={}", t.sid)]);
            if let Some(w) = &window {
                c.push(format!("window_id={w}"));
            }
            c
        }
    };
    if a.has("-n") {
        println!("{}", launch::show(&cmd));
        return;
    }
    match t.term {
        Term::Tmux => launch::run(&cmd, &[], false),
        Term::Rex => {
            let err = mux::rex_jump(None, &t.sid, window.as_deref(), pane.as_deref());
            if let Some(e) = err.filter(|e| !e.starts_with('?')) {
                die(e);
            }
        }
    }
}

fn pane_target(home: &Home, path: &str) -> (Target, Flat) {
    home.require_layout();
    let node = resolve(home, path, Want::Existing);
    let t = target(home, &node);
    let f = t.get();
    (t, f)
}

fn cmd_send(home: &Home, args: Vec<String>) {
    let (path, text) = match args.split_first() {
        Some((p, rest)) if !rest.is_empty() => (p.clone(), rest.join(" ")),
        _ => die("usage: cbar send PATH TEXT"),
    };
    let (t, f) = pane_target(home, &path);
    send_to(&t, &f, &text);
}

/// Only type into a Claude that is idle or done: a keystroke into one that
/// is working or blocked can answer a permission prompt.
fn send_to(t: &Target, f: &Flat, text: &str) {
    let st = &f.pane.state;
    if st != "idle" && st != "done" {
        let what = if st.is_empty() { "not running Claude" } else { st };
        die(format!("{} is {what}; only type into an idle or done agent", t.tab));
    }
    let session = if t.term == Term::Rex { &t.sid } else { &t.session };
    if let Err(err) = mux::send_line(t.term, session, &f.pane.block_id, text) {
        die(err);
    }
}

fn cmd_status(home: &Home, args: Vec<String>) {
    let Some(path) = args.first() else { die("usage: cbar status PATH") };
    let (_, f) = pane_target(home, path);
    println!("{}", if f.pane.state.is_empty() { "none" } else { &f.pane.state });
}

fn cmd_tail(home: &Home, args: Vec<String>) {
    let Some(path) = args.first() else { die("usage: cbar tail PATH [N]") };
    let n = args.get(1).map(|s| s.parse().unwrap_or_else(|_| die("usage: cbar tail PATH [N]"))).unwrap_or(60);
    let (t, f) = pane_target(home, path);
    tail(&t, &f, n);
}

fn tail(t: &Target, f: &Flat, n: usize) {
    let session = if t.term == Term::Rex { &t.sid } else { &t.session };
    let screen = mux::capture(t.term, session, &f.pane.block_id, n);
    let lines: Vec<&str> = screen.lines().collect();
    let last = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
    let lines = &lines[..last];
    for l in &lines[lines.len().saturating_sub(n)..] {
        println!("{l}");
    }
}

fn cmd_wait(home: &Home, args: Vec<String>) {
    let Some(path) = args.first() else { die("usage: cbar wait PATH [SECONDS]") };
    let limit: u64 = args.get(1).map(|s| s.parse().unwrap_or_else(|_| die("usage: cbar wait PATH [SECONDS]"))).unwrap_or(600);
    let (t, f) = pane_target(home, path);
    wait(&t, f, limit);
}

fn wait(t: &Target, f: Flat, limit: u64) {
    let start = Instant::now();
    let mut st = f.pane.state;
    let refresh = || t.get().pane.state;
    // an agent just started has no state until Claude is up (give it a
    // minute), and can look idle for a moment before it takes up its prompt
    // (look twice)
    let mut twice = false;
    loop {
        while st == "working" || (st.is_empty() && start.elapsed().as_secs() < 60) {
            if start.elapsed().as_secs() >= limit {
                println!("{}: still {} after {limit}s", t.tab, if st.is_empty() { "starting" } else { &st });
                std::process::exit(2);
            }
            std::thread::sleep(Duration::from_secs(3));
            st = refresh();
        }
        if twice || st.is_empty() {
            break;
        }
        twice = true;
        std::thread::sleep(Duration::from_secs(5));
        st = refresh();
    }
    println!("{}: {}", t.tab, if st.is_empty() { "not running Claude" } else { &st });
}

// ---- restart -----------------------------------------------------------------

/// What `cbar run` starts in NODE's pane, and the path it's given.
fn restart_role(node: &Node) -> (&'static str, String) {
    match node {
        Node::Exchange(e) => ("supervisor", format!("/{e}")),
        Node::Agent { exchange, project: None, name } if name == SUPERVISOR => ("supervisor", format!("/{exchange}")),
        Node::Project(e, p) => ("operator", format!("/{e}/{p}")),
        Node::Agent { exchange, project: Some(p), name } if name == OPERATOR => ("operator", format!("/{exchange}/{p}")),
        _ => ("agent", format!("/{}", node.path())),
    }
}

/// cbar restart PATH: quit its Claude and start it again in the same pane,
/// with what `cbar run` gives it today, resuming its conversation. Your own
/// pane is restarted by a detached cbar once your turn ends (--wait).
fn cmd_restart(home: &Home, args: Vec<String>) {
    let usage = "cbar restart PATH";
    let a = parse(&args, &["--wait"], usage);
    let Some(path) = a.pos.first() else { die(format!("usage: {usage}")) };
    let waiting = a.has("--wait");
    home.require_layout();
    let node = resolve(home, path, Want::Existing);
    let t = target(home, &node);
    let f = t.get();
    let session = if t.term == Term::Rex { t.sid.clone() } else { t.session.clone() };
    let own = mux::here_terminal() == Some(t.term) && mux::here_pane().as_deref() == Some(f.pane.block_id.as_str());
    let block = f.pane.block_id.clone();
    let found = || {
        let root = mux::pane_pid(t.term, &session, &block).unwrap_or_else(|| die(format!("could not find the process in {}'s pane", t.path)));
        restart::find(root).unwrap_or_else(|| {
            die(format!("found no Claude session file for {}'s Claude in {}; left it running", t.path, short(restart::sessions_dir())))
        })
    };
    if own && !waiting {
        // this Claude runs us: it can't quit until this turn ends, and when it
        // does it may take its children with it, so a cbar of its own does it
        let cs = found();
        let logs = home.root.join("logs");
        let _ = fs::create_dir_all(&logs);
        let log = logs.join(format!("restart-{}.log", t.path.replace('/', "-")));
        let out = fs::File::create(&log).unwrap_or_else(|e| die(format!("{}: {e}", short(&log))));
        let err = out.try_clone().unwrap_or_else(|e| die(format!("{}: {e}", short(&log))));
        let mut cmd = Command::new(launch::self_exe());
        cmd.args(["restart", "--wait", &format!("/{}", t.path)]).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        if let Err(e) = cmd.spawn() {
            die(format!("could not start the restart: {e}"));
        }
        println!("restarting {} (session {}) once this turn ends; log: {}", t.path, cs.id, short(&log));
        return;
    }
    let f = if waiting { wait_ready(&t, &f, Duration::from_secs(1800)) } else { f };
    let st = &f.pane.state;
    if st != "idle" && st != "done" {
        let what = if st.is_empty() { "not running Claude" } else { st };
        die(format!("{} is {what}; only restart an idle or done Claude", t.tab));
    }
    let cs = found();
    let (role, rpath) = restart_role(&node);
    println!("restarting {}: quitting its Claude (pid {}, session {})", t.path, cs.pid, cs.id);
    if let Err(e) = mux::send_line(t.term, &session, &f.pane.block_id, "/exit") {
        util::warn(format!("could not type /exit: {e}"));
    }
    // Claude may ask on /exit: a -w Claude whether to keep its worktree (keep
    // it), one with background work (a cbar watch Monitor) whether to stop it
    // (exit and stop it); each is answered once, with Enter on that choice
    let until = Instant::now() + Duration::from_secs(20);
    let (mut kept, mut stopped) = (false, false);
    while util::alive(&cs.pid.to_string()) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
        if kept && stopped {
            continue;
        }
        let screen = mux::capture(t.term, &session, &f.pane.block_id, 60);
        let answer = if !kept && restart::keep_worktree_prompt(&screen) {
            kept = true;
            "it asks whether to keep its worktree: keeping it"
        } else if !stopped && restart::stop_tasks_prompt(&screen) {
            stopped = true;
            "it asks whether to stop its background work: exiting and stopping it"
        } else {
            continue;
        };
        println!("{answer}");
        if let Err(e) = mux::send_enter(t.term, &session, &f.pane.block_id) {
            util::warn(format!("could not answer it: {e}"));
        }
    }
    if !restart::wait_exit(cs.pid, Duration::from_millis(500)) {
        println!("it didn't quit on /exit; sending it SIGTERM");
        unsafe { libc::kill(cs.pid as libc::pid_t, libc::SIGTERM) };
        if !restart::wait_exit(cs.pid, Duration::from_secs(5)) {
            unsafe { libc::kill(cs.pid as libc::pid_t, libc::SIGKILL) };
            if !restart::wait_exit(cs.pid, Duration::from_secs(5)) {
                die(format!("its Claude (pid {}) won't quit; left the pane as it is", cs.pid));
            }
        }
    }
    let dir = Some(cs.cwd.clone())
        .filter(|d| Path::new(d).is_dir())
        .or_else(|| work_dir(home, node.exchange().unwrap_or_default(), node.project()))
        .unwrap_or_else(|| util::home().to_string_lossy().into_owned());
    let mut sub = vec![role, rpath.as_str()];
    if role == "agent" {
        // already in its worktree, if it has one: that's where its conversation is
        sub.push("--no-worktree");
    }
    sub.extend(["--resume", cs.id.as_str()]);
    let pane = mux::replace(t.term, &session, &f.pane.block_id, &dir, &f.pane.label, &pane_argv(&sub)).unwrap_or_else(|e| die(e));
    // tell it why its system prompt changed, once it's up; if it asks to
    // trust its folder, never answer (its default is "No, exit"): say so
    let until = Instant::now() + Duration::from_secs(90);
    loop {
        // first: typing into that question would answer it
        if restart::trust_prompt(&mux::capture(t.term, &session, &pane, 60)) {
            die(format!("{}'s Claude is waiting at the folder-trust prompt for {}; answer it in its pane", t.path, short(&dir)));
        }
        let st = t.panes().into_iter().find(|g| g.pane.block_id == pane).map(|g| g.pane.state).unwrap_or_default();
        if st == "idle" || st == "done" {
            break;
        }
        if Instant::now() >= until {
            println!("restarted {} in {}, but its Claude isn't idle yet ({}); didn't tell it why", t.path, short(&dir), if st.is_empty() { "starting" } else { &st });
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let why = if role == "agent" {
        format!("crossbar restarted you on {} (cbar restart, same conversation); carry on with your task.", env!("CARGO_PKG_VERSION"))
    } else {
        format!("crossbar restarted you on {} with the current {role} prompt (cbar restart, same conversation); re-arm cbar watch and carry on.", env!("CARGO_PKG_VERSION"))
    };
    if let Err(e) = mux::send_line(t.term, &session, &pane, &why) {
        util::warn(format!("could not tell it why: {e}"));
    }
    println!("restarted {} in {}, resuming session {}", t.path, short(&dir), cs.id);
}

/// Wait (up to LIMIT) until the pane's Claude is idle or done twice in a row.
fn wait_ready(t: &Target, f: &Flat, limit: Duration) -> Flat {
    let until = Instant::now() + limit;
    let mut seen = 0;
    loop {
        let g = t.panes().into_iter().find(|g| g.pane.block_id == f.pane.block_id).unwrap_or_else(|| die(format!("{}'s pane is gone", t.path)));
        if g.pane.state == "idle" || g.pane.state == "done" {
            seen += 1;
            if seen >= 2 {
                return g;
            }
        } else {
            seen = 0;
        }
        if Instant::now() >= until {
            die(format!("{} is still {} after {}s; didn't restart it", t.path, g.pane.state, limit.as_secs()));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ---- rm --------------------------------------------------------------------

fn confirm(what: &[String], name: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        die(format!("pass -y to remove '{name}' without asking"));
    }
    println!("cbar rm {name} will:");
    for w in what {
        println!("  {w}");
    }
    print!("go ahead? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    if line.trim().eq_ignore_ascii_case("y") {
        true
    } else {
        println!("left it alone");
        false
    }
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map(|rd| rd.count()).unwrap_or(0)
}

fn cmd_rm(home: &Home, args: Vec<String>) {
    let a = parse(&args, &["-y", "--yes", "-f", "--force"], "cbar rm [-y] [-f] PATH");
    let Some(path) = a.pos.first() else { die("usage: cbar rm [-y] [-f] PATH") };
    home.require_layout();
    let yes = a.has("-y") || a.has("--yes");
    let here = paths::here(home);
    let node = match paths::resolve(home, &here, path, Want::Existing, &paths::tab_exists) {
        Ok(n) => n,
        // not registered: a project's session can still go
        Err(err) => match paths::resolve(home, &here, path, Want::New { agent: false }, &paths::tab_exists) {
            Ok(n @ Node::Project(..)) if find_session(&n.session().unwrap(), None).is_some() => n,
            _ => die(err),
        },
    };
    match &node {
        Node::Root => die("usage: cbar rm [-y] [-f] PATH"),
        Node::Agent { name, .. } if name == OPERATOR || name == SUPERVISOR => {
            die(format!("'{name}' runs its session; rm the {} instead", node.parent().unwrap().kind()))
        }
        Node::Agent { exchange, project, .. } => close_agent(home, &node, exchange, project.as_deref(), a.has("-f") || a.has("--force")),
        Node::Project(e, p) => rm_home(home, &node, &home.project_dir(e, p), yes),
        Node::Exchange(e) => {
            let ps = home.projects(e);
            if !ps.is_empty() {
                let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
                die(format!("exchange '{e}' still has projects ({}); rm them first", names.join(", ")));
            }
            rm_home(home, &node, &home.exchange_dir(e), yes);
        }
    }
}

/// Kill NODE's session and delete DIR (its registration, briefs and reports).
fn rm_home(home: &Home, node: &Node, dir: &Path, yes: bool) {
    let session = node.session().unwrap();
    let running = find_session(&session, recorded_term(home, node));
    let mut what = Vec::new();
    if let Some((term, sid)) = &running {
        let panes = status::flatten(Looker::new(false).snapshot(Some(sid)));
        let agents: Vec<String> = panes.iter().filter(|f| !f.pane.state.is_empty()).map(|f| format!("{} ({})", f.pane.tab, f.pane.state)).collect();
        let with = if agents.is_empty() { String::new() } else { format!(", with {}", agents.join(", ")) };
        what.push(format!("kill the {} session '{session}'{with}", term.as_str()));
    }
    if dir.is_dir() {
        what.push(format!("delete {} ({} briefs, {} reports)", short(dir), count(&dir.join("briefs")), count(&dir.join("reports"))));
    }
    if !yes && !confirm(&what, &node.path()) {
        std::process::exit(1);
    }
    // unregister first: removing the session from inside it ends this process
    if dir.is_dir() {
        if let Err(err) = fs::remove_dir_all(dir) {
            die(format!("could not delete {}: {err}", short(dir)));
        }
    }
    println!("removed {}", node.path());
    if let Some((term, _)) = running {
        mux::kill_session(term, &session);
    }
}

/// Close an agent's tab, then remove its worktree if it is clean and merged.
/// Refuses an agent that is working or blocked, or has uncommitted changes,
/// unless FORCE; FORCE never removes a worktree with work in it.
fn close_agent(home: &Home, node: &Node, e: &str, p: Option<&str>, force: bool) {
    let t = target(home, node);
    let panes = t.panes();
    let f = panes.iter().find(|f| f.pane.tab == t.tab && f.pane.label != "switchboard").cloned().unwrap_or_else(|| t.get());
    let wid = f.pane.window_id.clone();
    if let Some(me) = mux::here_pane() {
        if panes.iter().any(|g| g.pane.window_id == wid && g.pane.block_id == me) {
            die(format!("'{}' is this tab", t.tab));
        }
    }
    let repo = work_dir(home, e, p).unwrap_or_else(|| std::env::current_dir().unwrap_or_default().to_string_lossy().into_owned());
    let wt = Path::new(&repo).join(".claude/worktrees").join(&t.tab);
    let wt = wt.is_dir().then_some(wt);
    let git = |dir: &Path, args: &[&str]| {
        let mut v = strings(&["-C", &dir.to_string_lossy()]);
        v.extend(strings(args));
        util::run("git", &v, Duration::from_secs(30))
    };
    let dirty = wt.as_ref().is_some_and(|w| !git(w, &["status", "--porcelain"]).1.trim().is_empty());
    let st = &f.pane.state;
    if !force {
        if st == "working" || st == "blocked" {
            die(format!("{} is {st}; wait for it, or close it anyway with -f", t.tab));
        }
        if dirty {
            die(format!("{} has uncommitted changes in {}; commit them, or close it anyway with -f", t.tab, short(wt.as_ref().unwrap())));
        }
    }
    let session = if t.term == Term::Rex { &t.sid } else { &t.session };
    if !mux::close_tab(t.term, session, &wid) {
        die("could not close the tab");
    }
    println!("closed {}", t.tab);
    let Some(wt) = wt else { return };
    let wts = short(&wt);
    if dirty {
        println!("kept {wts}: uncommitted changes");
        return;
    }
    let branch = git(&wt, &["symbolic-ref", "-q", "--short", "HEAD"]).1.trim().to_string();
    if !merged(Path::new(&repo), &wt, &branch) {
        println!("kept {wts}: {} is not merged", if branch.is_empty() { "its commit" } else { &branch });
        return;
    }
    // Claude locks its worktree while it runs: wait for it to exit, then the lock is stale
    let list = git(Path::new(&repo), &["worktree", "list", "--porcelain"]).1;
    let mut on = false;
    let mut lock = None;
    for line in list.lines() {
        if let Some(w) = line.strip_prefix("worktree ") {
            on = Path::new(w) == wt || Path::new(w).canonicalize().ok() == wt.canonicalize().ok();
        } else if on && line.starts_with("locked") {
            lock = Some(line.to_string());
        }
    }
    if let Some(lock) = lock {
        let pid = lock.split("(pid ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).unwrap_or("").to_string();
        let mut tries = 0;
        while !pid.is_empty() && util::alive(&pid) && tries < 25 {
            std::thread::sleep(Duration::from_millis(200));
            tries += 1;
        }
        if !pid.is_empty() && util::alive(&pid) {
            println!("kept {wts}: still in use by pid {pid}");
            return;
        }
        git(Path::new(&repo), &["worktree", "unlock", &wt.to_string_lossy()]);
    }
    if git(Path::new(&repo), &["worktree", "remove", &wt.to_string_lossy()]).0 != 0 {
        println!("kept {wts}: git would not remove it");
        return;
    }
    if !branch.is_empty() && git(Path::new(&repo), &["branch", "-q", "-d", &branch]).0 != 0 {
        git(Path::new(&repo), &["branch", "-q", "-D", &branch]);
    }
    println!("removed {wts}{}", if branch.is_empty() { String::new() } else { format!(" and branch {branch}") });
}

/// Whether the worktree's work is in REPO's default branch (its commit is),
/// or its branch's PR has merged (a squash merge shows only there).
fn merged(repo: &Path, wt: &Path, branch: &str) -> bool {
    let git = |dir: &Path, args: &[&str]| {
        let mut v = strings(&["-C", &dir.to_string_lossy()]);
        v.extend(strings(args));
        util::run("git", &v, Duration::from_secs(30))
    };
    let (code, head, _) = git(wt, &["rev-parse", "HEAD"]);
    if code != 0 {
        return false;
    }
    let head = head.trim();
    let base = git(repo, &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"]).1.trim().to_string();
    let mut bases = vec![base.clone(), base.trim_start_matches("origin/").to_string(), "main".into(), "master".into()];
    bases.retain(|b| !b.is_empty());
    for b in &bases {
        if git(repo, &["rev-parse", "-q", "--verify", b]).0 == 0 && git(repo, &["merge-base", "--is-ancestor", head, b]).0 == 0 {
            return true;
        }
    }
    if branch.is_empty() || !util::on_path("gh") {
        return false;
    }
    let out = Command::new("gh")
        .args(["pr", "view", branch, "--json", "state", "-q", ".state"])
        .current_dir(repo)
        .stderr(std::process::Stdio::null())
        .output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim() == "MERGED").unwrap_or(false)
}

// ---- watch, board ------------------------------------------------------------

fn cmd_watch() {
    if mux::here_terminal().is_none() {
        die("run this inside a project's or an exchange's tmux or Rex pane");
    }
    launch::run(&[switchboard_path(), "--watch".into(), "--here".into()], &[], false)
}

fn cmd_board(home: &Home, args: Vec<String>) {
    let mut argv = vec![switchboard_path()];
    for a in &args {
        if a == "--here" {
            deprecated("cbar board --here", "cbar run board");
        }
        if a.starts_with('-') {
            argv.push(a.clone());
            continue;
        }
        home.require_layout();
        match resolve(home, a, Want::Existing) {
            Node::Exchange(e) => argv.extend(["--exchange".to_string(), e]),
            n @ (Node::Project(..) | Node::Agent { .. }) => argv.extend(["--session".to_string(), n.session().unwrap()]),
            Node::Root => argv.push("--all".into()),
        }
    }
    launch::run(&argv, &[], false)
}

// ---- agent (deprecated) ------------------------------------------------------

fn cmd_agent(home: &Home, args: Vec<String>) {
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r.to_vec()),
        None => ("help", Vec::new()),
    };
    let new_form = match sub {
        "new" => "cbar new TASK",
        "ls" => "cbar ls .",
        "status" => "cbar status TASK",
        "tail" => "cbar tail TASK [N]",
        "send" => "cbar send TASK TEXT",
        "wait" => "cbar wait TASK [SECONDS]",
        "close" => "cbar rm [-f] TASK",
        "" | "help" | "-h" | "--help" => return println!("{}", agent_usage()),
        _ => {
            eprintln!("cbar: unknown agent command '{sub}'");
            eprintln!("{}", agent_usage());
            std::process::exit(1)
        }
    };
    deprecated(&format!("cbar agent {sub}"), new_form);
    if mux::here_terminal().is_none() {
        die("run this inside a project's tmux or Rex pane");
    }
    home.require_layout();
    let here = paths::here(home);
    let in_tab = |task: &str| match &here {
        Node::Project(e, p) => Node::Agent { exchange: e.clone(), project: Some(p.clone()), name: task.into() },
        n => Node::Agent { exchange: n.exchange().map(String::from).unwrap_or_else(|| home.default_exchange()), project: None, name: task.into() },
    };
    match sub {
        "new" => {
            let a = parse(&rest, &["--no-worktree"], "cbar agent new TASK [--no-worktree]");
            let Some(task) = a.pos.first() else { die("usage: cbar agent new TASK [--no-worktree]") };
            let Node::Agent { exchange, project, name } = in_tab(task) else { unreachable!() };
            new_agent(home, &exchange, project.as_deref(), &name, None, !a.has("--no-worktree"), false);
        }
        "ls" => {
            let t = target(home, &match &here {
                Node::Project(..) => here.clone(),
                n => Node::Exchange(n.exchange().map(String::from).unwrap_or_else(|| home.default_exchange())),
            });
            println!("{:<20} {:<8} {:<5} MESSAGE", "TAB", "STATE", "SINCE");
            for f in t.panes().iter().filter(|f| !f.pane.state.is_empty()) {
                println!("{:<20} {:<8} {:<5} {}", f.pane.tab, f.pane.state, f.pane.since, f.pane.msg);
            }
        }
        "close" => {
            let a = parse(&rest, &["-f", "--force"], "cbar agent close [-f] TASK");
            let Some(task) = a.pos.first() else { die("usage: cbar agent close [-f] TASK") };
            let node = in_tab(task);
            close_agent(home, &node, node.exchange().unwrap(), node.project(), a.has("-f") || a.has("--force"));
        }
        _ => {
            let Some(task) = rest.first() else { die(format!("usage: {new_form}")) };
            let node = in_tab(task);
            let t = target(home, &node);
            let f = t.find().unwrap_or_else(|| die(format!("no tab '{task}' in this session (cbar agent ls)")));
            match sub {
                "status" => println!("{}", if f.pane.state.is_empty() { "none" } else { &f.pane.state }),
                "tail" => tail(&t, &f, rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(60)),
                "send" => {
                    if rest.len() < 2 {
                        die("usage: cbar agent send TASK TEXT");
                    }
                    send_to(&t, &f, &rest[1..].join(" "))
                }
                _ => wait(&t, f, rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(600)),
            }
        }
    }
}

// ---- version, update -----------------------------------------------------------

pub fn version(home: &Home) -> String {
    match fs::read_to_string(home.root.join("version")) {
        Ok(v) => v.trim().to_string(),
        Err(_) => format!("{} (not installed by install)", env!("CARGO_PKG_VERSION")),
    }
}

/// cbar update: fetch the latest main and run its installer.
fn cmd_update(home: &Home) {
    let old = version(home);
    let repo = util::env("CROSSBAR_REPO").unwrap_or_else(|| REPO.into());
    let tmp = std::env::temp_dir().join(format!("crossbar-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap_or_else(|e| die(format!("no temp dir: {e}")));
    let tarball = tmp.join("main.tar.gz");
    let url = format!("{}/archive/refs/heads/main.tar.gz", repo.trim_end_matches(".git").trim_end_matches('/'));
    let ok = Command::new("curl").args(["-fsSL", "-o"]).arg(&tarball).arg(&url).status().is_ok_and(|s| s.success())
        && Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&tmp).status().is_ok_and(|s| s.success());
    if !ok {
        let _ = fs::remove_dir_all(&tmp);
        die(format!("could not fetch {url}"));
    }
    let src = fs::read_dir(&tmp)
        .ok()
        .and_then(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.join("install").is_file()))
        .unwrap_or_else(|| die("the download has no installer"));
    let status = Command::new("/bin/sh")
        .arg(src.join("install"))
        .env("CROSSBAR_HOME", &home.root)
        .stdout(std::process::Stdio::null())
        .status();
    let _ = fs::remove_dir_all(&tmp);
    if !status.is_ok_and(|s| s.success()) {
        die("install failed");
    }
    let new = version(home);
    if old == new {
        println!("already up to date: {new}");
    } else {
        println!("updated: {old} → {new}");
    }
}
