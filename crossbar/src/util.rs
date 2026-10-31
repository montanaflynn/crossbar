//! Small helpers shared by cbar and switchboard: paths, time, subprocesses.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;

/// The program name for messages ("cbar" or "switchboard").
static PROG: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

pub fn prog() -> &'static str {
    PROG.get().copied().unwrap_or("crossbar")
}

pub fn set_prog(name: &'static str) {
    let _ = PROG.set(name);
}

static ON_DIE: std::sync::OnceLock<fn() -> !> = std::sync::OnceLock::new();

/// What die does instead of exiting (`cbar run --pane`: become the login shell).
pub fn on_die(f: fn() -> !) {
    let _ = ON_DIE.set(f);
}

/// Print `PROG: MSG` to stderr and exit 1.
pub fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{}: {}", prog(), msg.as_ref());
    if let Some(f) = ON_DIE.get() {
        f()
    }
    std::process::exit(1)
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("{}: {}", prog(), msg.as_ref());
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// PATH with $HOME shown as ~.
pub fn short(path: impl AsRef<Path>) -> String {
    let p = path.as_ref().to_string_lossy().into_owned();
    let h = home().to_string_lossy().into_owned();
    if !h.is_empty() && h != "/" && (p == h || p.starts_with(&format!("{h}/"))) {
        format!("~{}", &p[h.len()..])
    } else {
        p
    }
}

/// An env var, if set and not empty.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Run PROGRAM ARGS with a timeout; (exit code, stdout, stderr trimmed).
/// A missing program or a timeout gives code 1 and a message.
pub fn run(program: &str, args: &[String], timeout: Duration) -> (i32, String, String) {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return (1, String::new(), format!("{program} is not installed")),
    };
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });
    let until = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(1),
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return (1, String::new(), "timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break 1,
        }
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).trim().to_string();
    (code, out, err)
}

pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Whether NAME is a program on PATH.
pub fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(name))))
        .unwrap_or(false)
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Free-text status values are base64 per the protocol; show them decoded.
pub fn text(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let padded = format!("{value}{}", "=".repeat((4 - value.len() % 4) % 4));
    match base64::engine::general_purpose::STANDARD.decode(padded.as_bytes()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) if s.chars().all(|c| !c.is_control()) => s,
            _ => value.to_string(),
        },
        Err(_) => value.to_string(),
    }
}

pub fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if month <= 2 { 1 } else { 0 }, month, day)
}

/// Seconds since the epoch for an ISO 8601 time with an offset
/// (2026-10-30T04:39:48+00:00, …Z, fractional seconds allowed). A time
/// without an offset is taken as UTC.
pub fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut frac = 0.0;
    if b.get(i) == Some(&b'.') {
        let start = i;
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac = format!("0{}", &s[start..i]).parse().unwrap_or(0.0);
    }
    let offset = match b.get(i) {
        None => 0,
        Some(b'Z') | Some(b'z') => 0,
        Some(&c) if c == b'+' || c == b'-' => {
            let rest = s[i + 1..].replace(':', "");
            let oh: i64 = rest.get(0..2)?.parse().ok()?;
            let om: i64 = rest.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
            let o = oh * 3600 + om * 60;
            if c == b'-' {
                -o
            } else {
                o
            }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset;
    Some(secs as f64 + frac)
}

/// Now in UTC as ISO 8601, as the relay writes it.
pub fn utc_iso_now() -> String {
    let secs = now_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[allow(deprecated)] // libc::time_t on musl
fn local_tm(secs: i64) -> libc::tm {
    unsafe {
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

/// Local time now with its offset, e.g. 2026-10-30T17:15:31-05:00.
pub fn local_iso_now() -> String {
    let tm = local_tm(now_secs() as i64);
    let off = tm.tm_gmtoff as i64;
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        off / 3600,
        off % 3600 / 60
    )
}

/// The local clock, HH:MM.
pub fn clock() -> String {
    let tm = local_tm(now_secs() as i64);
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// A compact local timestamp for file names, 20261030-171531.
pub fn stamp() -> String {
    let tm = local_tm(now_secs() as i64);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// How long ago STAMP was: 3d, 2h, 5m or now; "" if it isn't a time.
pub fn age(stamp: &str) -> String {
    let Some(t) = parse_iso(stamp) else { return String::new() };
    let secs = now_secs() - t;
    for (unit, size) in [("d", 86400.0), ("h", 3600.0), ("m", 60.0)] {
        if secs >= size {
            return format!("{}{unit}", (secs / size).floor() as i64);
        }
    }
    "now".into()
}

/// Whether PID is a live process.
pub fn alive(pid: &str) -> bool {
    let Ok(pid) = pid.trim().parse::<i32>() else { return false };
    if pid <= 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Cut plain text to N columns.
pub fn fit(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Columns of ROWS (tab-separated fields) padded to line up, two spaces apart.
pub fn columns(rows: &[Vec<String>]) -> String {
    let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0; n];
    for r in rows {
        for (i, f) in r.iter().enumerate() {
            widths[i] = widths[i].max(f.chars().count());
        }
    }
    let mut out = String::new();
    for r in rows {
        let last = r.iter().rposition(|f| !f.is_empty()).map_or(0, |i| i + 1);
        let mut line = String::new();
        for (i, f) in r.iter().take(last).enumerate() {
            line.push_str(f);
            if i + 1 < last {
                line.push_str(&" ".repeat(widths[i] - f.chars().count() + 2));
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trip() {
        assert_eq!(parse_iso("1970-01-01T00:00:00+00:00"), Some(0.0));
        assert_eq!(parse_iso("1970-01-01T01:00:00+01:00"), Some(0.0));
        assert_eq!(parse_iso("1970-01-01T00:00:01Z"), Some(1.0));
        assert_eq!(parse_iso("1970-01-01T00:00:00.5-00:00"), Some(0.5));
        assert_eq!(parse_iso("2026-10-30T17:15:31-05:00"), Some(1793398531.0));
        assert_eq!(parse_iso("nope"), None);
        let now = utc_iso_now();
        assert!((parse_iso(&now).unwrap() - now_secs()).abs() < 2.0);
        assert!((parse_iso(&local_iso_now()).unwrap() - now_secs()).abs() < 2.0);
    }

    #[test]
    fn decodes_status_text() {
        assert_eq!(text("aGk="), "hi");
        assert_eq!(text("aGk"), "hi");
        assert_eq!(text("not base64!"), "not base64!");
        assert_eq!(text(""), "");
    }

    #[test]
    fn fits() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
    }

    #[test]
    fn lines_up_columns() {
        let rows = vec![strings(&["NAME", "X", ""]), strings(&["a", "", "z"])];
        assert_eq!(columns(&rows), "NAME  X\na        z\n");
    }
}
