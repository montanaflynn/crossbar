//! A switchboard started as `cbar run --pane board` exits when its pane goes
//! away, and on SIGTERM. Runs on a private tmux server (skipped without tmux),
//! with a PATH that has tmux but no rex or claude.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn tmux_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("tmux")).find(|p| p.is_file())
}

struct Server {
    socket: String,
    env_path: PathBuf,
    home: PathBuf,
    /// The server's socket file, once it runs (tmux leaves it behind).
    socket_path: std::cell::RefCell<String>,
}

impl Server {
    fn new(tag: &str) -> Option<Server> {
        let tmux = tmux_path()?;
        let dir = std::env::temp_dir().join(format!("cbar-board-exits-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&tmux, bin.join("tmux")).unwrap();
        Some(Server { socket: format!("cbar-test-{tag}-{}", std::process::id()), env_path: bin, home: dir.join("home"), socket_path: Default::default() })
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .args(["-L", &self.socket])
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", self.env_path.display()))
            .env("CROSSBAR_HOME", &self.home)
            .env("CROSSBAR_TMUX_SOCKET", &self.socket)
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .expect("tmux");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Start a session running `cbar run --pane board`; the switchboard's pid.
    fn start_board(&self) -> i32 {
        let cbar = env!("CARGO_BIN_EXE_cbar");
        self.tmux(&["new-session", "-d", "-s", "b", "-x", "100", "-y", "30", cbar, "run", "--pane", "board"]);
        *self.socket_path.borrow_mut() = self.tmux(&["display-message", "-p", "#{socket_path}"]);
        let pane_pid = self.tmux(&["display-message", "-p", "-t", "=b:", "#{pane_pid}"]);
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            let out = Command::new("pgrep").args(["-P", &pane_pid, "switchboard"]).output().expect("pgrep");
            if let Ok(pid) = String::from_utf8_lossy(&out.stdout).trim().parse::<i32>() {
                std::thread::sleep(Duration::from_secs(2)); // let it settle in
                return pid;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no switchboard started");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.tmux(&["kill-server"]);
        let socket = self.socket_path.borrow();
        if !socket.is_empty() {
            let _ = std::fs::remove_file(&*socket);
        }
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap_or(Path::new("/nonexistent")));
    }
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether PID exits within a few seconds; kills it either way.
fn exits(pid: i32) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::kill(pid, libc::SIGKILL) };
    false
}

#[test]
fn board_exits_with_its_session() {
    let Some(s) = Server::new("session") else { return eprintln!("no tmux: skipped") };
    s.tmux(&["new-session", "-d", "-s", "keep", "sleep", "60"]); // the server outlives the pane
    let pid = s.start_board();
    assert!(alive(pid), "the board quit on its own");
    s.tmux(&["kill-session", "-t", "=b"]);
    assert!(exits(pid), "the switchboard outlived its session");
}

#[test]
fn board_exits_with_its_server() {
    let Some(s) = Server::new("server") else { return eprintln!("no tmux: skipped") };
    let pid = s.start_board();
    s.tmux(&["kill-server"]);
    assert!(exits(pid), "the switchboard outlived its tmux server");
}

#[test]
fn board_exits_on_sigterm() {
    let Some(s) = Server::new("term") else { return eprintln!("no tmux: skipped") };
    let pid = s.start_board();
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert!(exits(pid), "the switchboard ignored SIGTERM");
}
