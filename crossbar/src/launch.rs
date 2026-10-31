//! Starting programs in panes without typing into a shell.
//!
//! cbar gives each pane it makes `[env K=V…] cbar run --pane ROLE …` as the
//! pane's own command. `cbar run` then runs the program (Claude, a
//! switchboard) as the terminal's foreground job, and with --pane falls back
//! to your login shell when it exits, so the pane stays open as it did when
//! the command was typed into a shell. Nothing depends on which shell that is.

use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::util;

/// Settings carried into the panes cbar starts.
pub const CARRY: [&str; 3] = ["CROSSBAR_HOME", "CROSSBAR_TMUX_SOCKET", "CROSSBAR_CLAUDE"];

/// This cbar, by absolute path.
pub fn self_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cbar"));
    exe.canonicalize().unwrap_or(exe)
}

/// A program installed next to this one (switchboard, claude-relay).
pub fn sibling(name: &str) -> Option<PathBuf> {
    let p = self_exe().parent()?.join(name);
    util::is_executable(&p).then_some(p)
}

/// The argv for a pane that runs `cbar run --pane SUB…`, with crossbar's
/// settings carried over through env(1).
pub fn pane_argv(sub: &[&str]) -> Vec<String> {
    let mut argv = Vec::new();
    for k in CARRY {
        if let Some(v) = util::env(k) {
            let v = if k == "CROSSBAR_HOME" { absolute(&v) } else { v };
            argv.push(format!("{k}={v}"));
        }
    }
    if !argv.is_empty() {
        argv.insert(0, "env".into());
    }
    argv.push(self_exe().to_string_lossy().into_owned());
    argv.push("run".into());
    argv.push("--pane".into());
    argv.extend(sub.iter().map(|s| s.to_string()));
    argv
}

fn absolute(p: &str) -> String {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        std::env::current_dir().map(|d| d.join(path).to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
    }
}

/// Claude's argv prefix: claude-relay (which reports Claude's status in tmux
/// and just runs Claude elsewhere) when it's installed next to cbar, else
/// claude. $CROSSBAR_CLAUDE runs another program in claude's place.
pub fn claude_cmd() -> (Vec<String>, Vec<(String, String)>) {
    let program = util::env("CROSSBAR_CLAUDE").unwrap_or_else(|| "claude".into());
    match sibling("claude-relay") {
        Some(relay) => (vec![relay.to_string_lossy().into_owned()], vec![("CLAUDE_RELAY_PROGRAM".into(), program)]),
        None => (vec![program], Vec::new()),
    }
}

const JOB_SIGNALS: [libc::c_int; 5] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTOU, libc::SIGTTIN];

fn set_job_signals(action: libc::sighandler_t) {
    for s in JOB_SIGNALS {
        unsafe { libc::signal(s, action) };
    }
}

/// Run ARGV (with ENVS). Without PANE, or without a terminal, it replaces
/// this process. With PANE it runs as the terminal's foreground job, and
/// when it ends this process becomes your login shell.
pub fn run(argv: &[String], envs: &[(String, String)], pane: bool) -> ! {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if !pane || unsafe { libc::isatty(0) } == 0 {
        let err = cmd.exec();
        util::die(format!("{}: {err}", argv[0]));
    }
    set_job_signals(libc::SIG_IGN);
    cmd.process_group(0);
    unsafe {
        cmd.pre_exec(|| {
            libc::tcsetpgrp(0, libc::getpid());
            for s in JOB_SIGNALS {
                libc::signal(s, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id() as libc::pid_t;
            unsafe { libc::tcsetpgrp(0, pid) };
            loop {
                let mut st = 0;
                let r = unsafe { libc::waitpid(pid, &mut st, libc::WUNTRACED) };
                if r < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if libc::WIFSTOPPED(st) {
                    // stopped (Ctrl-Z) with no shell to come back to: carry on
                    unsafe {
                        libc::tcsetpgrp(0, pid);
                        libc::kill(-pid, libc::SIGCONT);
                    }
                    continue;
                }
                break;
            }
            unsafe { libc::tcsetpgrp(0, libc::getpgrp()) };
        }
        Err(e) => util::warn(format!("{}: {e}", argv[0])),
    }
    set_job_signals(libc::SIG_DFL);
    login_shell()
}

/// Become the user's login shell: $SHELL, else the account's, else /bin/sh.
pub fn login_shell() -> ! {
    let shell = util::env("SHELL").or_else(account_shell).unwrap_or_else(|| "/bin/sh".into());
    let dash = format!("-{}", util::basename(&shell));
    let err = Command::new(&shell).arg0(&dash).exec();
    util::warn(format!("{shell}: {err}"));
    let err = Command::new("/bin/sh").arg0("-sh").exec();
    util::die(format!("/bin/sh: {err}"))
}

fn account_shell() -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned();
        (!s.is_empty()).then_some(s)
    }
}

/// ARGV as one line for a person to read or paste (single-quoted as needed).
pub fn show(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(dead_code)]
fn cstring(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}
