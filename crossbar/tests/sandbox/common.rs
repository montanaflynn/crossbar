//! The sandbox the tmux and Rex tests run in, never the live server or
//! exchange: a scratch HOME (with project folders) and CROSSBAR_HOME under
//! /tmp, a private tmux server (`tmux -L`, its socket in the sandbox) or a
//! private Rex server (`rex -S SOCK --autostart=false`), and the stub `claude`
//! (stub/claude) first on PATH. A tmux sandbox puts a `rex` on PATH that always
//! fails, so nothing in it sees the live Rex server.
//!
//! On it: the exchange central with its supervisor and exchange switchboard,
//! projects alpha (a git repo) and beta (a plain folder) with their operators
//! and project switchboards, and the agents in AGENTS, each reporting the state
//! its brief asks the stub for.
//!
//! Each check prints a line (`ok` or `FAIL`) and a test goes on past a failed
//! one; `finish` fails the test with the list. Dropping the sandbox stops its
//! server, kills whatever of it still runs and removes it.
//!
//!   CBAR_TEST_VERBOSE=1        print each check's detail, not only a failed one's
//!   CBAR_TEST_KEEP=1           keep the sandbox (its path is printed)
//!   CBAR_TEST_REQUIRE_TMUX=1   fail, rather than skip, without tmux (CI sets it)

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

pub const EXCHANGE: &str = "central";
/// Project: its folder under HOME.
pub const PROJECTS: [(&str, &str); 2] = [("alpha", "repo-a"), ("beta", "repo-b")];
/// Project: the agents started from BRIEFS, and the state each reports.
pub const AGENTS: [(&str, &[(&str, &str)]); 2] = [
    ("alpha", &[("t-done", "done"), ("t-ask", "blocked"), ("t-boom", "error"), ("t-busy", "working"), ("t-chat", "idle")]),
    ("beta", &[("t-nogit", "done")]),
];
/// Each brief's first line tells the stub what to report.
pub const BRIEFS: [(&str, &str); 6] = [
    ("t-ask", "stub: blocked permission | allow npm test?\n\nThe stub waits on a permission prompt.\n"),
    ("t-boom", "stub: error | tests failed\n\nThe stub reports an error.\n"),
    ("t-busy", "stub: working\n\nThe stub keeps working.\n"),
    ("t-chat", "stub: idle\n\nThe stub is idle; send it a line and it works, then reports done.\n"),
    ("t-done", "stub: done | finished the thing\n\nThe stub reports done with a message.\n"),
    ("t-nogit", "stub: done | no repo here\n\nAn agent in a project that is not a git repo.\n"),
];

const STUB_CLAUDE: &str = include_str!("stub/claude");
const REX_OFF: &str = "#!/bin/sh\n# `rex` in a tmux sandbox, so nothing sees the live Rex server.\necho \"Error: Rex is switched off in this sandbox\" >&2\nexit 1\n";

pub fn brief(name: &str) -> &'static str {
    BRIEFS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b).expect("a fixture brief")
}

pub fn say(msg: &str) {
    eprintln!("{msg}");
}

pub fn sleep(secs: f64) {
    std::thread::sleep(Duration::from_secs_f64(secs));
}

/// Whether F turns true within LIMIT seconds, asked every half second.
pub fn wait_for(limit: f64, mut f: impl FnMut() -> bool) -> bool {
    wait_some(limit, || f().then_some(())).is_some()
}

/// F's first Some within LIMIT seconds, asked every half second.
pub fn wait_some<T>(limit: f64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + Duration::from_secs_f64(limit);
    while Instant::now() < end {
        if let Some(v) = f() {
            return Some(v);
        }
        sleep(0.5);
    }
    f()
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

pub fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// What a command printed, and its exit code (-1: timed out or killed).
#[derive(Debug, Clone)]
pub struct Out {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl std::fmt::Display for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}: {} | {}", self.code, self.out.trim(), self.err.trim())
    }
}

pub type Env = BTreeMap<String, String>;

/// A Command for ARGV with exactly ENV, in CWD, in a session of its own
/// (no controlling terminal), stdin from /dev/null.
pub fn command(argv: &[String], env: &Env, cwd: &Path) -> Command {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]).env_clear().envs(env).current_dir(cwd).stdin(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c
}

/// Run ARGV and wait for it, up to TIMEOUT (then kill its process group).
pub fn run(argv: &[String], env: &Env, cwd: &Path, timeout: Duration) -> Out {
    let mut child = match command(argv, env, cwd).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return Out { code: -1, out: String::new(), err: format!("{}: {e}", argv[0]) },
    };
    let (tx, rx) = mpsc::channel();
    for (i, mut pipe) in [Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>, Box::new(child.stderr.take().unwrap())]
        .into_iter()
        .enumerate()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            let _ = pipe.read_to_end(&mut s);
            let _ = tx.send((i, String::from_utf8_lossy(&s).into_owned()));
        });
    }
    let end = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if Instant::now() >= end => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                break -1;
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // what it left running may hold its pipes: read what came within a moment
    let mut got = [String::new(), String::new()];
    let until = Instant::now() + Duration::from_secs(5);
    for _ in 0..2 {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((i, s)) => got[i] = s,
            Err(_) => break,
        }
    }
    let [out, err] = got;
    Out { code, out, err }
}

/// A command run in the background (cbar watch), its output to a file.
pub struct Bg {
    pub child: Child,
    pub log: PathBuf,
}

impl Bg {
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn code(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().map(|st| st.code().unwrap_or(-1))
    }

    /// Whether it exits 0 within LIMIT seconds.
    pub fn exits_ok(&mut self, limit: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(limit);
        loop {
            if let Some(code) = self.code() {
                return code == 0;
            }
            if Instant::now() >= end {
                return false;
            }
            sleep(0.1);
        }
    }

    pub fn text(&self) -> String {
        read(&self.log)
    }

    pub fn err(&self) -> String {
        read(self.log.with_extension("err"))
    }

    /// Its non-blank lines, sorted.
    pub fn lines(&self) -> Vec<String> {
        let mut v: Vec<String> = self.text().lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
        v.sort();
        v
    }

    /// SIGTERM its process group, and wait.
    pub fn stop(&mut self) {
        if self.running() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM) };
            let end = Instant::now() + Duration::from_secs(10);
            while self.running() && Instant::now() < end {
                sleep(0.1);
            }
        }
    }
}

impl Drop for Bg {
    fn drop(&mut self) {
        if self.running() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }
}

/// A pane, as the checks need it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pane {
    pub session: String,
    pub sid: String,
    pub tab: String,
    pub pane: String,
    pub label: String,
    pub state: String,
    pub msg: String,
}

fn decode(v: &str) -> String {
    use base64::Engine;
    if v.is_empty() {
        return String::new();
    }
    let padded = format!("{v}{}", "=".repeat((4 - v.len() % 4) % 4));
    base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| v.to_string())
}

/// The built cbar, switchboard and claude-relay. claude-relay is another
/// package's binary, so cargo doesn't build it for these tests: build it
/// (a no-op when it's fresh) into the profile folder cbar is in.
fn binaries() -> &'static [PathBuf; 3] {
    static BINS: OnceLock<[PathBuf; 3]> = OnceLock::new();
    BINS.get_or_init(|| {
        let cbar = PathBuf::from(env!("CARGO_BIN_EXE_cbar"));
        let dir = cbar.parent().unwrap().to_path_buf();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut c = Command::new(cargo);
        c.args(["build", "-q", "-p", "claude-relay", "--manifest-path"]).arg(workspace.join("Cargo.toml"));
        if dir.file_name().is_some_and(|n| n == "release") {
            c.arg("--release");
        }
        let st = c.status().expect("cargo build -p claude-relay");
        assert!(st.success(), "cargo build -p claude-relay failed");
        let relay = dir.join("claude-relay");
        assert!(relay.is_file(), "no {} after cargo build -p claude-relay", relay.display());
        [cbar, PathBuf::from(env!("CARGO_BIN_EXE_switchboard")), relay]
    })
}

/// A PATH with tmux, git, base64 and uuidgen and none of the user's own bin
/// folders (where a real claude lives).
fn system_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<String> = Vec::new();
    for prog in ["tmux", "git", "base64", "uuidgen", "pgrep"] {
        let found = which(prog).unwrap_or_else(|| panic!("the sandbox needs {prog}"));
        let d = found.parent().unwrap().to_string_lossy().into_owned();
        if !dirs.contains(&d) && (home.is_empty() || !d.starts_with(&home)) {
            dirs.push(d);
        }
    }
    for d in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        if !dirs.iter().any(|x| x == d) {
            dirs.push(d.into());
        }
    }
    dirs.join(":")
}

pub struct Sandbox {
    pub rex: bool,
    pub tag: String,
    pub root: PathBuf,
    pub home: PathBuf,
    /// The first folder on PATH: claude (the stub) and rex.
    pub stub: PathBuf,
    /// cbar, switchboard and claude-relay, copied from the build.
    pub bin: PathBuf,
    /// CROSSBAR_HOME.
    pub cb: PathBuf,
    /// tmux -L.
    pub sock: String,
    pub rex_sock: PathBuf,
    pub env: Env,
    /// "central" or a project: (session id, its supervisor's or operator's pane).
    pub panes_at: BTreeMap<String, (String, String)>,
    rex_server: Option<Child>,
    stopped: bool,
    fails: RefCell<Vec<String>>,
    verbose: bool,
    /// Rex sandboxes run one at a time: under the load of three private Rex
    /// servers, a lone ESC and the rest of a mouse report reach the
    /// switchboard too far apart (and it jumps).
    _one_rex: Option<MutexGuard<'static, ()>>,
}

impl Sandbox {
    /// A sandbox for test TAG, on Rex or tmux; None (the test is skipped)
    /// without tmux, unless CBAR_TEST_REQUIRE_TMUX is set.
    pub fn new(tag: &str, rex: bool) -> Option<Sandbox> {
        if which("tmux").is_none() {
            assert!(!flag("CBAR_TEST_REQUIRE_TMUX"), "no tmux, and CBAR_TEST_REQUIRE_TMUX is set");
            say(&format!("{tag}: no tmux: skipped"));
            return None;
        }
        let real_rex = if rex { Some(which("rex").expect("the Rex tests need rex on PATH")) } else { None };
        static REX: Mutex<()> = Mutex::new(());
        let one_rex = rex.then(|| REX.lock().unwrap_or_else(|e| e.into_inner()));
        let bins = binaries();
        static N: AtomicUsize = AtomicUsize::new(0);
        // short, under /tmp: Rex's and tmux's sockets live here
        let root = PathBuf::from(format!("/tmp/cb{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let (home, zdot, stub, bin) = (root.join("home"), root.join("zdot"), root.join("stub"), root.join("bin"));
        for d in [&home, &zdot, &stub, &bin] {
            fs::create_dir_all(d).unwrap();
        }
        for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            fs::write(zdot.join(f), "").unwrap();
        }
        fs::write(zdot.join("shrc"), "PS1='$ '\n").unwrap(); // $ENV: what an interactive sh reads
        for b in bins {
            let to = bin.join(b.file_name().unwrap());
            if fs::hard_link(b, &to).is_err() {
                fs::copy(b, &to).unwrap();
            }
        }
        write_exec(&stub.join("claude"), STUB_CLAUDE);
        let rex_sock = root.join("rex.sock");
        match &real_rex {
            // rex ignores $REX_SERVER: every call names the private server, and never starts one
            Some(real) => write_exec(
                &stub.join("rex"),
                &format!("#!/bin/sh\nexec \"{}\" --autostart=false -S \"unix://{}\" \"$@\"\n", real.display(), rex_sock.display()),
            ),
            None => write_exec(&stub.join("rex"), REX_OFF),
        }
        // a real repo for alpha (agents get -w), a plain folder for beta
        let alpha = home.join(PROJECTS[0].1);
        fs::create_dir_all(&alpha).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "init"]] {
            let st = Command::new("git").arg("-C").arg(&alpha).args(args).stdin(Stdio::null()).output().expect("git");
            assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
        }
        fs::create_dir_all(home.join(PROJECTS[1].1)).unwrap();
        let cb = home.join(".crossbar");
        fs::create_dir_all(&cb).unwrap();
        let sock = format!("cbt-{tag}-{}", std::process::id());
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let utf8 = if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" };
        let mut env: Env = [
            ("HOME", s(&home)),
            ("ZDOTDIR", s(&zdot)),
            ("SHELL", "/bin/sh".into()),
            ("PATH", format!("{}:{}:{}", s(&stub), s(&bin), system_path())),
            ("USER", std::env::var("USER").unwrap_or_default()),
            ("LOGNAME", std::env::var("LOGNAME").unwrap_or_default()),
            ("TMPDIR", std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())),
            ("TMUX_TMPDIR", s(&root)),
            ("TERM", "xterm-256color".into()),
            ("LANG", utf8.into()),
            ("LC_ALL", utf8.into()),
            ("COLUMNS", "100".into()),
            ("LINES", "50".into()),
            ("CROSSBAR_HOME", s(&cb)),
            ("CROSSBAR_TMUX_SOCKET", sock.clone()),
            ("CROSSBAR_CLAUDE", s(&stub.join("claude"))),
            ("CLAUDE_RELAY_PROGRAM", s(&stub.join("claude"))),
            ("CBAR_STUB_WORK", "2".into()),
            ("ENV", s(&zdot.join("shrc"))), // sh's prompt: no host name
            ("PS1", "$ ".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        if rex {
            env.insert("REX_SERVER".into(), format!("unix://{}", rex_sock.display())); // for the real rex in Rex's panes
        }
        say(&format!("{tag}: sandbox at {} ({})", root.display(), if rex { "rex" } else { "tmux" }));
        Some(Sandbox {
            rex,
            tag: tag.into(),
            root,
            home,
            stub,
            bin,
            cb,
            sock,
            rex_sock,
            env,
            panes_at: BTreeMap::new(),
            rex_server: None,
            _one_rex: one_rex,
            stopped: false,
            fails: RefCell::new(Vec::new()),
            verbose: flag("CBAR_TEST_VERBOSE"),
        })
    }

    pub fn kind(&self) -> &'static str {
        if self.rex {
            "rex"
        } else {
            "tmux"
        }
    }

    // ---- checks -------------------------------------------------------------

    /// Print a check's line; a failed one is listed by `finish`.
    pub fn check(&self, name: impl AsRef<str>, ok: bool, detail: impl AsRef<str>) -> bool {
        let (name, detail) = (name.as_ref(), detail.as_ref());
        let shown = if !detail.is_empty() && (!ok || self.verbose) { format!("  ({detail})") } else { String::new() };
        println!("{}  {name}{shown}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            self.fails.borrow_mut().push(name.to_string());
        }
        ok
    }

    /// Stop the server, check nothing of the sandbox still runs, and fail
    /// the test if any check failed.
    pub fn finish(mut self) {
        self.stop_mux();
        let (n, ps) = self.leftovers();
        self.check("nothing left running", n == 0, ps);
        let fails = self.fails.borrow().clone();
        if fails.is_empty() {
            println!("all passed ({})", self.kind());
        } else {
            panic!("{} failed ({}):\n  {}", fails.len(), self.kind(), fails.join("\n  "));
        }
    }

    // ---- running things -------------------------------------------------------

    /// The sandbox's environment, as if in PANE (a key of panes_at).
    pub fn env_in(&self, pane: Option<&str>) -> Env {
        let mut env = self.env.clone();
        if let Some(p) = pane {
            let (sid, pane) = self.panes_at.get(p).unwrap_or_else(|| panic!("no pane for {p}"));
            env.extend(self.pane_env(sid, pane));
        }
        env
    }

    fn argv(&self, args: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        if matches!(args[0], "cbar" | "switchboard") {
            v[0] = self.bin.join(args[0]).to_string_lossy().into_owned();
        }
        v
    }

    /// Run ARGS (cbar and switchboard are the sandbox's) as if in PANE.
    pub fn run(&self, args: &[&str], pane: Option<&str>, timeout: f64) -> Out {
        run(&self.argv(args), &self.env_in(pane), &self.home, Duration::from_secs_f64(timeout))
    }

    pub fn cbar(&self, args: &[&str], pane: Option<&str>) -> Out {
        let mut v = vec!["cbar"];
        v.extend_from_slice(args);
        self.run(&v, pane, 180.0)
    }

    /// Start ARGS in the background as if in PANE, with ENV on top, its output in LOG.
    pub fn spawn(&self, args: &[&str], pane: Option<&str>, env: &[(&str, String)], log: &Path) -> Bg {
        let mut e = self.env_in(pane);
        for (k, v) in env {
            e.insert(k.to_string(), v.clone());
        }
        let child = command(&self.argv(args), &e, &self.home)
            .stdout(fs::File::create(log).unwrap())
            .stderr(fs::File::create(log.with_extension("err")).unwrap())
            .spawn()
            .expect("spawn");
        Bg { child, log: log.to_path_buf() }
    }

    pub fn session(&self, project: Option<&str>) -> String {
        match project {
            Some(p) => format!("{EXCHANGE}/{p}"),
            None => EXCHANGE.into(),
        }
    }

    pub fn exchange_dir(&self) -> PathBuf {
        self.cb.join("exchanges").join(EXCHANGE)
    }

    pub fn briefs(&self, project: &str) -> PathBuf {
        self.exchange_dir().join("projects").join(project).join("briefs")
    }

    /// PATH with ~ for HOME.
    pub fn short(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }

    // ---- the server ------------------------------------------------------------

    pub fn tmux(&self, args: &[&str]) -> Out {
        let mut v = vec!["tmux".to_string(), "-L".into(), self.sock.clone()];
        v.extend(args.iter().map(|s| s.to_string()));
        run(&v, &self.env, &self.home, Duration::from_secs(60))
    }

    pub fn rex(&self, args: &[&str]) -> Out {
        let mut v = vec![self.stub.join("rex").to_string_lossy().into_owned()];
        v.extend(args.iter().map(|s| s.to_string()));
        run(&v, &self.env, &self.home, Duration::from_secs(60))
    }

    /// A rex command's JSON, or Null.
    pub fn rex_json(&self, args: &[&str]) -> Value {
        let o = self.rex(args);
        if o.code != 0 {
            return Value::Null;
        }
        serde_json::from_str(&o.out).unwrap_or(Value::Null)
    }

    fn tmux_socket_path(&self) -> PathBuf {
        self.root.join(format!("tmux-{}", unsafe { libc::getuid() })).join(&self.sock)
    }

    pub fn start_mux(&mut self) {
        if !self.rex {
            return; // cbar new starts the tmux server
        }
        let real = which("rex").unwrap();
        let config = self.root.join("rex-init.lua");
        fs::write(&config, "").unwrap();
        let log = fs::File::create(self.root.join("rex.log")).unwrap();
        let argv: Vec<String> = [
            real.to_string_lossy().into_owned(),
            "--autostart=false".into(),
            "server".into(),
            "run".into(),
            "--listen".into(),
            format!("unix://{}", self.rex_sock.display()),
            "--state-dir".into(),
            self.root.join("rex-state").to_string_lossy().into_owned(),
            "--restore=false".into(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ]
        .into();
        let child = command(&argv, &self.env, &self.home).stdout(log.try_clone().unwrap()).stderr(log).spawn().expect("rex server");
        self.rex_server = Some(child);
        let up = (0..50).any(|_| {
            if self.rex(&["ls", "--json"]).code == 0 {
                return true;
            }
            sleep(0.2);
            false
        });
        assert!(up, "the private Rex server didn't start (see {})", self.root.join("rex.log").display());
        self.check_shell();
    }

    /// What runs in a Rex pane gets your login environment, with Rex's own
    /// bin folder first: make sure that, in a shell pane and in a pane
    /// running a command, claude is the stub, claude-relay runs the stub,
    /// the crossbar settings carry over and rex reaches this private server
    /// (through $REX_SERVER), or stop before anything types `claude` into a pane.
    fn check_shell(&self) {
        let report = r#"echo "claude=$(command -v claude)"; echo "relay=$CLAUDE_RELAY_PROGRAM"; echo "home=$CROSSBAR_HOME"; echo "sessions=$(rex --autostart=false ls --json | grep -c session_id)""#;
        let shell = self.root.join("probe-shell");
        let cmd = self.root.join("probe-command");
        let home = self.home.to_string_lossy().into_owned();
        let o = self.rex(&["new", "_probe_shell", "-c", &home, "--json"]);
        assert_eq!(o.code, 0, "rex new: {o}");
        let script = format!("{{ {report}; }} > {0}.tmp; mv {0}.tmp {0}; sleep 30", cmd.display());
        let o = self.rex(&["new", "_probe_command", "-c", &home, "--json", "--", "/bin/sh", "-c", &script]);
        assert_eq!(o.code, 0, "rex new: {o}");
        sleep(1.0);
        self.rex(&["send", "-s", "_probe_shell", &format!("{{ {report}; }} > {0}.tmp; mv {0}.tmp {0}\r", shell.display())]);
        for _ in 0..75 {
            if shell.exists() && cmd.exists() {
                break;
            }
            sleep(0.2);
        }
        for name in ["_probe_shell", "_probe_command"] {
            if let Some(sid) = self.session_id(name) {
                self.rex(&["kill", &sid]);
            }
        }
        let claude = self.stub.join("claude").to_string_lossy().into_owned();
        let want = [("claude", claude.clone()), ("relay", claude), ("home", self.cb.to_string_lossy().into_owned()), ("sessions", "2".into())];
        for (kind, probe) in [("shell", &shell), ("command", &cmd)] {
            let got: BTreeMap<String, String> =
                read(probe).lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
            let bad: Vec<String> = want
                .iter()
                .filter(|(k, v)| got.get(*k) != Some(v))
                .map(|(k, _)| format!("{k}={}", got.get(*k).map(String::as_str).unwrap_or("nothing")))
                .collect();
            assert!(bad.is_empty(), "in a Rex {kind} pane, {bad:?} isn't the sandbox's ({want:?}); not running Rex cases");
        }
    }

    pub fn stop_mux(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if !self.rex {
            self.tmux(&["kill-server"]);
            let _ = fs::remove_file(self.tmux_socket_path());
            return;
        }
        if let Some(mut server) = self.rex_server.take() {
            if matches!(server.try_wait(), Ok(None)) {
                unsafe { libc::kill(-(server.id() as i32), libc::SIGTERM) };
                let end = Instant::now() + Duration::from_secs(10);
                while matches!(server.try_wait(), Ok(None)) && Instant::now() < end {
                    sleep(0.1);
                }
                if matches!(server.try_wait(), Ok(None)) {
                    unsafe { libc::kill(-(server.id() as i32), libc::SIGKILL) };
                }
                let _ = server.wait();
            }
        }
        // and anything else still serving its socket, or the scratch HOME's
        // default one: a rex whose $REX_SERVER is gone falls back to (and
        // autostarts) the home server, which the scratch HOME keeps inside the sandbox
        sleep(2.0);
        let home = self.home.display().to_string();
        for mark in [self.rex_sock.display().to_string(), format!("{home}/Library/Application Support/rex"), format!("{home}/Library/Application%20Support/rex")] {
            for pid in pgrep(&mark) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// What of this sandbox still runs 2s after its server stopped: nothing
    /// should, as every pane's terminal is gone. (count, their command lines)
    pub fn leftovers(&self) -> (usize, String) {
        sleep(2.0);
        let mut pids: Vec<i32> = pgrep(&self.bin.display().to_string());
        pids.extend(pgrep(&self.stub.display().to_string()));
        pids.sort();
        pids.dedup();
        if pids.is_empty() {
            return (0, String::new());
        }
        (pids.len(), ps(&pids))
    }

    /// Kill anything still running from the sandbox (its path in the command
    /// line), after giving it two seconds to exit on its own.
    fn reap(&self) {
        let mark = self.root.display().to_string();
        let end = Instant::now() + Duration::from_secs(2);
        let mut pids = pgrep(&mark);
        while !pids.is_empty() && Instant::now() < end {
            sleep(0.2);
            pids = pgrep(&mark);
        }
        if pids.is_empty() {
            return;
        }
        let listing = ps(&pids);
        for &pid in &pids {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        say(&format!("{}: killed {} process(es) still running after their server stopped:", self.tag, pids.len()));
        for line in listing.lines() {
            say(&format!("  {}", line.trim().replace(&mark, "$SANDBOX")));
        }
    }

    pub fn session_id(&self, name: &str) -> Option<String> {
        self.rex_json(&["ls", "--json"])["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["label"] == name)
            .and_then(|s| s["session_id"].as_str().map(String::from))
    }

    /// What commands run "in" a pane get: tmux's or Rex's variables for it.
    pub fn pane_env(&self, sid: &str, pane: &str) -> Vec<(String, String)> {
        if self.rex {
            vec![("REX_SESSION".into(), sid.into()), ("REX_BLOCK".into(), pane.into())]
        } else {
            vec![("TMUX_PANE".into(), pane.into()), ("TMUX".into(), format!("{},0,0", self.tmux_socket_path().display()))]
        }
    }

    /// Every pane, with the state and message its Claude reports.
    pub fn panes(&self) -> Vec<Pane> {
        if !self.rex {
            let fields = ["session_name", "session_id", "window_name", "pane_id", "@cbar_label", "@cbar_state", "@cbar_msg"];
            let fmt: Vec<String> = fields.iter().map(|f| format!("#{{{f}}}")).collect();
            let o = self.tmux(&["list-panes", "-a", "-F", &fmt.join("\t")]);
            return o
                .out
                .lines()
                .filter_map(|l| {
                    let r: Vec<&str> = l.split('\t').collect();
                    (r.len() == fields.len()).then(|| Pane {
                        session: r[0].into(),
                        sid: r[1].into(),
                        tab: r[2].into(),
                        pane: r[3].into(),
                        label: r[4].into(),
                        state: r[5].into(),
                        msg: decode(r[6]),
                    })
                })
                .collect();
        }
        let mut out = Vec::new();
        let sessions = self.rex_json(&["ls", "--json"]);
        for s in sessions["sessions"].as_array().into_iter().flatten() {
            let Some(sid) = s["session_id"].as_str() else { continue };
            let windows = self.rex_json(&["window", "ls", "-s", sid, "--json"]);
            let tabs: BTreeMap<String, String> = windows["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|w| (w["window_id"].as_str().unwrap_or("").to_string(), w["label"].as_str().unwrap_or("").to_string()))
                .collect();
            let blocks = self.rex_json(&["block", "ls", "-s", sid, "--json"]);
            for b in blocks["blocks"].as_array().into_iter().flatten() {
                if b["placement"].as_str().unwrap_or("placed") != "placed" {
                    continue;
                }
                let Some(id) = b["block_id"].as_str() else { continue };
                let status = self.rex_json(&["block", "call", "-s", sid, "-b", id, "program_status"]);
                let root = status["records"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["id"].as_str().unwrap_or("").is_empty()).cloned())
                    .unwrap_or(Value::Null);
                out.push(Pane {
                    session: s["label"].as_str().unwrap_or("").into(),
                    sid: sid.into(),
                    tab: tabs.get(b["window_id"].as_str().unwrap_or("")).cloned().unwrap_or_default(),
                    pane: id.into(),
                    label: b["label"].as_str().unwrap_or("").into(),
                    state: root["state"].as_str().unwrap_or("").into(),
                    msg: root["msg"].as_str().unwrap_or("").into(),
                });
            }
        }
        out
    }

    /// The pane of tab TAB in SESSION that isn't a switchboard.
    pub fn pane_in(&self, session: &str, tab: &str) -> Option<Pane> {
        self.panes().into_iter().find(|p| p.session == session && p.tab == tab && p.label != "switchboard")
    }

    /// Split ARGV off PANE (of session SID in Rex), to its right, labelled LABEL.
    pub fn split(&self, sid: &str, pane: &str, cwd: &Path, argv: &[String], label: &str) -> String {
        let cwd = cwd.to_string_lossy();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        if self.rex {
            let mut a = vec!["split", "-s", sid, "-b", pane, "-c", &cwd, "--split=right", "--ratio", "30", "--focus=false", "--label", label, "--json", "--"];
            a.extend(&argv);
            let o = self.rex(&a);
            assert_eq!(o.code, 0, "rex split: {o}");
            let v: Value = serde_json::from_str(&o.out).unwrap();
            return v["block_ids"][0].as_str().unwrap().into();
        }
        let mut a = vec!["split-window", "-h", "-d", "-l", "30%", "-t", pane, "-c", &cwd, "-P", "-F", "#{pane_id}"];
        a.extend(&argv);
        let o = self.tmux(&a);
        assert_eq!(o.code, 0, "tmux split-window: {o}");
        let board = o.out.trim().to_string();
        self.tmux(&["set-option", "-p", "-t", &board, "@cbar_label", label]);
        board
    }

    /// Type TEXT into a pane, then Enter.
    pub fn send_line(&self, sid: &str, pane: &str, text: &str) {
        if self.rex {
            self.rex(&["send", "-s", sid, "-b", pane, text]);
            sleep(0.4);
            self.rex(&["send", "-s", sid, "-b", pane, "\r"]);
        } else {
            self.tmux(&["send-keys", "-t", pane, "-l", "--", text]);
            sleep(0.4);
            self.tmux(&["send-keys", "-t", pane, "Enter"]);
        }
    }

    /// Type DATA into a pane as one write.
    pub fn write(&self, sid: &str, pane: &str, data: &str) {
        if self.rex {
            let json = serde_json::json!({ "data": data }).to_string();
            self.rex(&["block", "call", "-s", sid, "-b", pane, "write", &json]);
        } else {
            let hex: Vec<String> = data.bytes().map(|b| format!("{b:02x}")).collect();
            let mut a = vec!["send-keys", "-t", pane, "-H"];
            a.extend(hex.iter().map(String::as_str));
            self.tmux(&a);
        }
    }

    /// The program in a pane's foreground (its name).
    pub fn program(&self, sid: &str, pane: &str) -> String {
        if self.rex {
            let v = self.rex_json(&["block", "call", "-s", sid, "-b", pane, "process"]);
            let name = v["foreground"]["name"].as_str().unwrap_or("");
            return Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
        self.tmux(&["display-message", "-p", "-t", pane, "#{pane_current_command}"]).out.trim().to_string()
    }

    pub fn screen(&self, sid: &str, pane: &str) -> String {
        if self.rex {
            self.rex(&["block", "capture", "-s", sid, "-b", pane, "--trim"]).out
        } else {
            self.tmux(&["capture-pane", "-p", "-t", pane]).out
        }
    }

    pub fn close(&self, sid: &str, pane: &str) {
        if self.rex {
            self.rex(&["block", "close", "-s", sid, pane]);
        } else {
            self.tmux(&["kill-pane", "-t", pane]);
        }
    }

    // ---- the exchange -------------------------------------------------------------

    /// The exchange with its supervisor and exchange switchboard, projects
    /// alpha and beta with their operators and project switchboards, and the
    /// agents in AGENTS, each reporting its state.
    pub fn setup(&mut self) {
        say(&format!("{}: setting up ({})", self.tag, self.kind()));
        self.start_mux();
        let term = if self.rex { "--rex" } else { "--tmux" };
        let o = self.cbar(&["new", "--exchange", EXCHANGE, term], None);
        self.check("setup: cbar new --exchange", o.code == 0, o.to_string());
        for (project, folder) in PROJECTS {
            let dir = self.home.join(folder).to_string_lossy().into_owned();
            let o = self.cbar(&["new", project, &dir, term], None);
            self.check(format!("setup: cbar new {project}"), o.code == 0, o.to_string());
        }
        let mut want: BTreeMap<(String, String), String> = BTreeMap::new();
        want.insert((self.session(None), "supervisor".into()), "idle".into());
        for (p, _) in PROJECTS {
            want.insert((self.session(Some(p)), "operator".into()), "idle".into());
        }
        self.wait_states(&want);
        self.locate();
        for (project, agents) in AGENTS {
            fs::create_dir_all(self.briefs(project)).unwrap();
            for (task, state) in agents {
                fs::write(self.briefs(project).join(format!("{task}.md")), brief(task)).unwrap();
                let o = self.cbar(&["new", task], Some(project));
                self.check(format!("setup: cbar new {task}"), o.code == 0, o.to_string());
                want.insert((self.session(Some(project)), task.to_string()), state.to_string());
            }
        }
        let ok = self.wait_states(&want);
        self.check("setup: every Claude reports its state", ok, "");
    }

    /// The supervisor's and each operator's pane, for commands run "in" them.
    fn locate(&mut self) {
        for p in self.panes() {
            for project in [None, Some(PROJECTS[0].0), Some(PROJECTS[1].0)] {
                let tab = if project.is_some() { "operator" } else { "supervisor" };
                if p.session == self.session(project) && p.tab == tab && p.label != "switchboard" {
                    self.panes_at.entry(project.unwrap_or(EXCHANGE).to_string()).or_insert((p.sid.clone(), p.pane.clone()));
                }
            }
        }
    }

    /// Wait until each (session, tab) in WANT reports its state.
    fn wait_states(&self, want: &BTreeMap<(String, String), String>) -> bool {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let have: BTreeMap<(String, String), String> =
                self.panes().into_iter().filter(|p| p.label != "switchboard").map(|p| ((p.session, p.tab), p.state)).collect();
            let missing: Vec<String> = want
                .iter()
                .filter(|(k, v)| have.get(*k) != Some(v))
                .map(|((s, t), v)| format!("{s} {t} is '{}', not {v}", have.get(&(s.clone(), t.clone())).map(String::as_str).unwrap_or("missing")))
                .collect();
            if missing.is_empty() {
                return true;
            }
            if Instant::now() > end {
                for m in missing {
                    say(&format!("{}: {m}", self.tag));
                }
                return false;
            }
            sleep(0.5);
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.stop_mux();
        self.reap();
        if flag("CBAR_TEST_KEEP") {
            say(&format!("{}: kept the sandbox at {}", self.tag, self.root.display()));
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// The pids whose command line has MARK (not this process).
pub fn pgrep(mark: &str) -> Vec<i32> {
    let me = std::process::id() as i32;
    Command::new("pgrep")
        .args(["-f", &regex_escape(mark)])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|p| p.parse().ok()).filter(|&p| p != me).collect())
        .unwrap_or_default()
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if ".^$*+?()[]{}|\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn ps(pids: &[i32]) -> String {
    let list: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
    let o = Command::new("ps").args(["-o", "pid=,args=", "-p", &list.join(",")]).stdin(Stdio::null()).output();
    let mut lines: Vec<String> = o.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default();
    lines.sort();
    lines.join("\n")
}

pub fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// The last N bytes of S, or a little more (on a char boundary).
pub fn last(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[i..]
}
