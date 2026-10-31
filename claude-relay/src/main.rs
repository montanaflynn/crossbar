//! claude-relay: run Claude Code on a pty of its own and stand in for a
//! terminal that understands OSC 7501 program status, as Rex does, so Claude
//! reports its state and status message inside tmux too.
//!
//!   claude-relay [CLAUDE ARGS...]
//!
//! Claude asks the terminal `ESC ] 7501 ; ?` at startup and, when answered,
//! writes `ESC ] 7501 ; state=…:app=claude-code[:kind=…][:msg=<base64>]` as its
//! state changes. The answer has to come in the byte stream, ahead of the
//! terminal's reply to the device-attributes query Claude sends after it, so
//! nothing outside the stream can give it. claude-relay answers the question
//! and writes the root status (the record without an id) to the tmux pane's
//! options @cbar_state, @cbar_kind, @cbar_msg (base64, as sent) and
//! @cbar_since, with its own pid in @cbar_relay so a reader can tell a live
//! status from a stale one. Everything else passes through untouched.
//!
//! Ctrl-Z works as it does without the relay: Claude runs as a job of a small
//! session leader on the inner pty, so it can stop itself; the relay then
//! stops too, back to your shell, and `fg` resumes both.
//!
//! Outside tmux (no $TMUX_PANE) or without a terminal it just runs claude.
//! $CLAUDE_RELAY_PROGRAM runs another program in claude's place;
//! $CROSSBAR_TMUX_SOCKET names a tmux server other than the default (tmux -L).

use std::env;
use std::ffi::CString;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const START: &[u8] = b"\x1b]7501;";
const REPLY: &[u8] = b"\x1b]7501;?\x1b\\";
const OPTIONS: [&str; 4] = ["@cbar_state", "@cbar_kind", "@cbar_msg", "@cbar_since"];
const LONGEST: usize = 8192; // past this, an unterminated OSC 7501 is not ours

static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let b = sig as u8;
    unsafe { libc::write(SIGNAL_FD.load(Ordering::Relaxed), &b as *const u8 as *const libc::c_void, 1) };
}

fn main() {
    let program = env::var("CLAUDE_RELAY_PROGRAM").unwrap_or_else(|_| "claude".into());
    let args: Vec<String> = env::args().skip(1).collect();
    let mut argv = vec![cstring(&program)];
    argv.extend(args.iter().map(|a| cstring(a)));

    let pane = env::var("TMUX_PANE").unwrap_or_default();
    if pane.is_empty() || unsafe { libc::isatty(0) } == 0 {
        exec(&argv);
    }
    std::process::exit(relay(&argv, pane));
}

fn cstring(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| {
        eprintln!("claude-relay: an argument holds a NUL byte");
        std::process::exit(2)
    })
}

/// Replace this process with ARGV, or exit 127.
fn exec(argv: &[CString]) -> ! {
    let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    unsafe { libc::execvp(ptrs[0], ptrs.as_ptr()) };
    let err = std::io::Error::last_os_error();
    eprintln!("claude-relay: {}: {}", argv[0].to_string_lossy(), err);
    unsafe { libc::_exit(127) }
}

fn cloexec(fd: libc::c_int) {
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

fn pipe() -> (libc::c_int, libc::c_int) {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        die("pipe");
    }
    cloexec(fds[0]);
    cloexec(fds[1]);
    (fds[0], fds[1])
}

fn die(what: &str) -> ! {
    eprintln!("claude-relay: {}: {}", what, std::io::Error::last_os_error());
    std::process::exit(1)
}

fn write_all(fd: libc::c_int, mut data: &[u8]) {
    while !data.is_empty() {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        data = &data[n as usize..];
    }
}

fn read(fd: libc::c_int, buf: &mut [u8]) -> isize {
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return n;
    }
}

fn copy_size(to: libc::c_int) {
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut size) == 0 {
            libc::ioctl(to, libc::TIOCSWINSZ, &size);
        }
    }
}

fn set_raw(saved: &libc::termios) {
    unsafe {
        let mut raw = *saved;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(0, libc::TCSAFLUSH, &raw);
    }
}

fn restore(saved: &libc::termios) {
    unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, saved) };
}

/// Run ARGV on a new pty as a job of a small session leader (so Ctrl-Z can
/// stop it), relay its bytes, and publish its status. Returns its exit code.
fn relay(argv: &[CString], pane: String) -> i32 {
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
        exec(argv);
    }
    let (mut master, mut slave) = (0, 0);
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        libc::ioctl(0, libc::TIOCGWINSZ, &mut size);
        if libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), &mut saved, &mut size) != 0 {
            die("openpty");
        }
    }
    cloexec(master);
    let (stopped_r, stopped_w) = pipe(); // leader → relay: "Claude stopped"
    let (resume_r, resume_w) = pipe(); // relay → leader: "go on"

    let leader = unsafe { libc::fork() };
    if leader < 0 {
        die("fork");
    }
    if leader == 0 {
        unsafe {
            libc::close(master);
            libc::close(stopped_r);
            libc::close(resume_w);
        }
        session_leader(argv, slave, stopped_w, resume_r);
    }
    unsafe {
        libc::close(slave);
        libc::close(stopped_w);
        libc::close(resume_r);
    }

    let (sig_r, sig_w) = pipe();
    SIGNAL_FD.store(sig_w, Ordering::Relaxed);
    for sig in [libc::SIGWINCH, libc::SIGCONT, libc::SIGTERM, libc::SIGHUP] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }

    let mut tmux = Tmux::new(pane);
    tmux.set(&[("@cbar_relay", &std::process::id().to_string())]);
    let mut status = Status::default();
    set_raw(&saved);

    let mut held: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 65536];
    let mut ended = false;
    loop {
        let mut fds = [
            libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: sig_r, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stopped_r, events: libc::POLLIN, revents: 0 },
        ];
        // bytes held back as a possible start of OSC 7501 go out anyway if
        // nothing follows them soon
        let timeout = if held.is_empty() { -1 } else { 50 };
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if n == 0 {
            write_all(1, &held);
            held.clear();
            continue;
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let got = read(0, &mut buf);
            if got <= 0 {
                break; // the pane is gone
            }
            write_all(master, &buf[..got as usize]);
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            let got = read(master, &mut buf);
            if got <= 0 {
                break; // every holder of the pty is gone
            }
            held.extend_from_slice(&buf[..got as usize]);
            let (out, rest) = filter(&held, master, &mut status, &mut tmux);
            write_all(1, &out);
            held = rest;
        }
        if fds[2].revents & libc::POLLIN != 0 {
            let got = read(sig_r, &mut buf);
            for &sig in &buf[..got.max(0) as usize] {
                match sig as libc::c_int {
                    libc::SIGWINCH | libc::SIGCONT => copy_size(master),
                    _ => ended = true, // SIGTERM, SIGHUP
                }
            }
            if ended {
                break;
            }
        }
        if fds[3].revents & libc::POLLIN != 0 {
            let mut b = [0u8; 1];
            if read(stopped_r, &mut b) == 1 {
                // Claude stopped itself (Ctrl-Z): stop too, back to the shell
                write_all(1, &held);
                held.clear();
                restore(&saved);
                unsafe { libc::kill(0, libc::SIGTSTP) };
                // resumed (or nobody does job control here): resume Claude
                set_raw(&saved);
                copy_size(master);
                write_all(resume_w, b"C");
            }
        } else if fds[3].revents & libc::POLLHUP != 0 {
            // the leader exited, so Claude did: drain what it wrote last
            unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) };
            loop {
                let got = read(master, &mut buf);
                if got <= 0 {
                    break;
                }
                held.extend_from_slice(&buf[..got as usize]);
            }
            let (out, rest) = filter(&held, master, &mut status, &mut tmux);
            write_all(1, &out);
            write_all(1, &rest);
            held.clear();
            break;
        }
    }

    restore(&saved);
    write_all(1, &held);
    let mut keys: Vec<&str> = OPTIONS.to_vec();
    keys.push("@cbar_relay");
    tmux.unset(&keys);
    tmux.wait();
    unsafe { libc::close(master) };
    reap(leader, ended)
}

/// The leader's exit code (Claude's), waiting at most a moment after a hangup.
fn reap(leader: libc::pid_t, hung_up: bool) -> i32 {
    let until = Instant::now() + Duration::from_secs(if hung_up { 2 } else { 30 });
    loop {
        let mut st = 0;
        let r = unsafe { libc::waitpid(leader, &mut st, libc::WNOHANG) };
        if r == leader {
            if libc::WIFEXITED(st) {
                return libc::WEXITSTATUS(st);
            }
            if libc::WIFSIGNALED(st) {
                return 128 + libc::WTERMSIG(st);
            }
        }
        if r < 0 || Instant::now() >= until {
            return if hung_up { 129 } else { 1 };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// In the forked child: lead a new session on SLAVE, run ARGV as its
/// foreground job, report stops on STOPPED and resume on a byte from RESUME.
/// Exits with ARGV's exit code.
fn session_leader(argv: &[CString], slave: libc::c_int, stopped: libc::c_int, resume: libc::c_int) -> ! {
    unsafe {
        libc::setsid();
        libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        let job = libc::fork();
        if job < 0 {
            libc::_exit(1);
        }
        if job == 0 {
            libc::setpgid(0, 0);
            libc::tcsetpgrp(slave, libc::getpid());
            libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            libc::dup2(slave, 0);
            libc::dup2(slave, 1);
            libc::dup2(slave, 2);
            if slave > 2 {
                libc::close(slave);
            }
            libc::close(stopped);
            libc::close(resume);
            exec(argv);
        }
        libc::setpgid(job, job);
        libc::tcsetpgrp(slave, job);
        loop {
            let mut st = 0;
            if libc::waitpid(job, &mut st, libc::WUNTRACED) < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                libc::_exit(1);
            }
            if libc::WIFSTOPPED(st) {
                write_all(stopped, b"S");
                let mut b = [0u8; 1];
                if read(resume, &mut b) <= 0 {
                    libc::kill(-job, libc::SIGHUP); // the relay is gone
                }
                libc::tcsetpgrp(slave, job);
                libc::kill(-job, libc::SIGCONT);
            } else if libc::WIFEXITED(st) {
                libc::_exit(libc::WEXITSTATUS(st));
            } else if libc::WIFSIGNALED(st) {
                libc::_exit(128 + libc::WTERMSIG(st));
            }
        }
    }
}

/// Take the OSC 7501 sequences out of BUF: answer the question on MASTER,
/// publish the statuses. Returns (bytes to pass on, bytes held for the next read).
fn filter(buf: &[u8], master: libc::c_int, status: &mut Status, tmux: &mut Tmux) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        let Some(j) = find(&buf[i..], START).map(|k| i + k) else {
            // a sequence may start in the last few bytes
            let from = buf.len().saturating_sub(START.len() - 1).max(i);
            if let Some(k) = buf[from..].iter().position(|&b| b == 0x1b).map(|k| from + k) {
                if START.starts_with(&buf[k..]) {
                    out.extend_from_slice(&buf[i..k]);
                    return (out, buf[k..].to_vec());
                }
            }
            out.extend_from_slice(&buf[i..]);
            return (out, Vec::new());
        };
        out.extend_from_slice(&buf[i..j]);
        let body = j + START.len();
        let mut end = None; // (body end, sequence end)
        let mut k = body;
        while k < buf.len() && k - body <= LONGEST {
            match buf[k] {
                0x07 => {
                    end = Some((k, k + 1));
                    break;
                }
                0x1b if k + 1 < buf.len() && buf[k + 1] == b'\\' => {
                    end = Some((k, k + 2));
                    break;
                }
                0x1b if k + 1 < buf.len() => break, // not a terminator: not ours
                _ => k += 1,
            }
        }
        match end {
            Some((b, e)) => {
                let data = &buf[body..b];
                if data.starts_with(b"?") {
                    write_all(master, REPLY);
                } else {
                    status.publish(data, tmux);
                }
                i = e;
            }
            None if k >= buf.len() && k - body <= LONGEST => return (out, buf[j..].to_vec()),
            None => {
                out.push(buf[j]);
                i = j + 1;
            }
        }
    }
    (out, Vec::new())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Default)]
struct Status {
    last: Option<(String, String, String)>, // state, kind, msg
}

impl Status {
    fn publish(&mut self, data: &[u8], tmux: &mut Tmux) {
        let text = String::from_utf8_lossy(data);
        let mut state = "";
        let (mut kind, mut msg) = ("", "");
        for part in text.split(':') {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            match k {
                "id" => return, // a subagent's or a task's own record
                "state" => state = v,
                "kind" => kind = v,
                "msg" => msg = v,
                _ => {}
            }
        }
        if state == "clear" {
            self.last = None;
            tmux.unset(&OPTIONS);
            return;
        }
        let now = (state.to_string(), kind.to_string(), msg.to_string());
        if self.last.as_ref() == Some(&now) {
            return;
        }
        let changed = self.last.as_ref().is_none_or(|l| l.0 != now.0);
        let since = iso_now();
        let mut pairs = vec![("@cbar_state", now.0.as_str()), ("@cbar_kind", now.1.as_str()), ("@cbar_msg", now.2.as_str())];
        if changed {
            pairs.push(("@cbar_since", since.as_str()));
        }
        tmux.set(&pairs);
        self.last = Some(now);
    }
}

/// Now in UTC as ISO 8601, e.g. 2026-10-30T04:39:48+00:00.
fn iso_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// tmux commands against this pane, run without waiting on them.
struct Tmux {
    socket: Option<String>,
    pane: String,
    running: Vec<Child>,
}

impl Tmux {
    fn new(pane: String) -> Self {
        let socket = env::var("CROSSBAR_TMUX_SOCKET").ok().filter(|s| !s.is_empty());
        Tmux { socket, pane, running: Vec::new() }
    }

    fn run(&mut self, args: Vec<String>) {
        self.running.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        let mut cmd = Command::new("tmux");
        if let Some(s) = &self.socket {
            cmd.arg("-L").arg(s);
        }
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(child) = cmd.spawn() {
            self.running.push(child);
        }
    }

    fn set(&mut self, pairs: &[(&str, &str)]) {
        let mut args = Vec::new();
        for (k, v) in pairs {
            if !args.is_empty() {
                args.push(";".into());
            }
            args.extend(["set-option", "-p", "-t", &self.pane, k, v].map(String::from));
        }
        self.run(args);
    }

    fn unset(&mut self, keys: &[&str]) {
        let mut args = Vec::new();
        for k in keys {
            if !args.is_empty() {
                args.push(";".into());
            }
            args.extend(["set-option", "-p", "-u", "-t", &self.pane, k].map(String::from));
        }
        self.run(args);
    }

    fn wait(&mut self) {
        let until = Instant::now() + Duration::from_secs(2);
        for child in &mut self.running {
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<String>) {
        let mut tmux = Tmux { socket: None, pane: "%0".into(), running: Vec::new() };
        let mut status = Status::default();
        let (r, w) = pipe();
        let (out, held) = filter(input, w, &mut status, &mut tmux);
        unsafe { libc::fcntl(r, libc::F_SETFL, libc::O_NONBLOCK) };
        let mut buf = [0u8; 64];
        let n = read(r, &mut buf).max(0) as usize;
        let mut seen = Vec::new();
        if n > 0 {
            seen.push(format!("reply {:?}", String::from_utf8_lossy(&buf[..n])));
        }
        if let Some(l) = status.last {
            seen.push(format!("status {} {} {}", l.0, l.1, l.2));
        }
        (out, held, seen)
    }

    #[test]
    fn passes_plain_bytes() {
        assert_eq!(run(b"plain \x1b[31mred\x1b[0m").0, b"plain \x1b[31mred\x1b[0m");
    }

    #[test]
    fn answers_the_probe() {
        let (out, held, seen) = run(b"a\x1b]7501;?\x1b\\b");
        assert_eq!((out.as_slice(), held.as_slice()), (&b"ab"[..], &b""[..]));
        assert_eq!(seen, vec!["reply \"\\u{1b}]7501;?\\u{1b}\\\\\"".to_string()]);
    }

    #[test]
    fn takes_status_out() {
        let (out, _, seen) = run(b"x\x1b]7501;state=blocked:app=claude-code:kind=permission:msg=aGk=\x07y");
        assert_eq!(out, b"xy");
        assert_eq!(seen, vec!["status blocked permission aGk=".to_string()]);
    }

    #[test]
    fn ignores_subagent_records() {
        assert!(run(b"\x1b]7501;state=working:app=claude-code:id=t1\x07").2.is_empty());
    }

    #[test]
    fn holds_a_split_sequence() {
        assert_eq!(run(b"frame\x1b").1, b"\x1b");
        assert_eq!(run(b"a\x1b]75").1, b"\x1b]75");
        assert_eq!(run(b"s \x1b]7501;state=work").1, b"\x1b]7501;state=work");
    }

    #[test]
    fn passes_a_foreign_escape() {
        assert_eq!(run(b"\x1b]7501;x\x1b[0mz").0, b"\x1b]7501;x\x1b[0mz");
    }

    #[test]
    fn dates() {
        assert!(iso_now().ends_with("+00:00") && iso_now().len() == 25);
    }
}
