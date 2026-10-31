//! The ~/.crossbar layout ($CROSSBAR_HOME):
//!
//!   config.toml                     default_exchange = "central"
//!   operator.md, supervisor.md      role prompts, version, bin/
//!   exchanges/<exchange>/
//!     exchange.toml                 name, terminal, created, path (optional: the
//!                                   folder its supervisor and agents start in;
//!                                   without it, this exchange's own folder)
//!     briefs/ reports/              the supervisor's agents
//!     projects/<project>/
//!       project.toml                name, path, terminal, created
//!       briefs/ reports/            the operator's agents
//!
//! An exchange's supervisor runs in the session named after it (`central`), each
//! of its projects in `<exchange>/<project>` (`central/stats`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::mux::Term;
use crate::util;

pub const SEP: char = '/';
pub const DEFAULT_EXCHANGE: &str = "central";
/// Reserved tab names: a project's operator and an exchange's supervisor.
pub const OPERATOR: &str = "operator";
pub const SUPERVISOR: &str = "supervisor";

#[derive(Clone, Debug)]
pub struct Home {
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub exchange: String,
    pub name: String,
    pub path: String,
    pub terminal: Term,
}

impl Project {
    pub fn session(&self) -> String {
        session_of(&self.exchange, Some(&self.name))
    }
}

#[derive(Clone, Debug)]
pub struct Exchange {
    pub name: String,
    pub terminal: Option<Term>,
    /// `path` in exchange.toml, as written (it may start with `~`).
    pub path: Option<String>,
}

/// The session for an exchange, or one of its projects.
pub fn session_of(exchange: &str, project: Option<&str>) -> String {
    match project {
        Some(p) => format!("{exchange}{SEP}{p}"),
        None => exchange.to_string(),
    }
}

/// A valid exchange or project name: it becomes (part of) a tmux session
/// name, which can't hold '.' or ':'.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A valid agent (tab) name.
pub fn valid_task(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A top-level KEY of a toml file, as a string (a datetime as written).
pub fn toml_get(file: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    let table: toml::Table = text.parse().ok()?;
    match table.get(key)? {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Datetime(d) => Some(d.to_string()),
        v => Some(v.to_string()),
    }
}

/// PATH with a leading `~` (or `~/`) as the home folder.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some("") => util::home(),
        Some(rest) if rest.starts_with('/') => util::home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// A toml basic string.
pub fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

impl Home {
    pub fn from_env() -> Home {
        let root = util::env("CROSSBAR_HOME").map(PathBuf::from).unwrap_or_else(|| util::home().join(".crossbar"));
        Home { root }
    }

    pub fn exchanges_dir(&self) -> PathBuf {
        self.root.join("exchanges")
    }

    pub fn exchange_dir(&self, ex: &str) -> PathBuf {
        self.exchanges_dir().join(ex)
    }

    pub fn exchange_toml(&self, ex: &str) -> PathBuf {
        self.exchange_dir(ex).join("exchange.toml")
    }

    pub fn project_dir(&self, ex: &str, p: &str) -> PathBuf {
        self.exchange_dir(ex).join("projects").join(p)
    }

    pub fn project_toml(&self, ex: &str, p: &str) -> PathBuf {
        self.project_dir(ex, p).join("project.toml")
    }

    /// The crossbar folder of an exchange or one of its projects.
    pub fn dir_of(&self, ex: &str, project: Option<&str>) -> PathBuf {
        match project {
            Some(p) => self.project_dir(ex, p),
            None => self.exchange_dir(ex),
        }
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Whether this home still has the 0.4 layout (central/ and projects/ as
    /// real folders, no exchanges/).
    pub fn old_layout(&self) -> bool {
        let real_dir = |p: PathBuf| fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
        !self.exchanges_dir().is_dir() && (real_dir(self.root.join("projects")) || real_dir(self.root.join("central")))
    }

    /// Stop, saying what to move, if this home has the 0.4 layout.
    pub fn require_layout(&self) {
        let left = self.root.join("projects");
        let ex = self.exchange_dir(&self.default_exchange());
        if !self.old_layout() && fs::symlink_metadata(&left).is_ok_and(|m| m.is_dir()) {
            util::warn(format!(
                "{} is a 0.4 folder that cbar no longer reads: move each project in it to {}/",
                util::short(&left),
                util::short(ex.join("projects"))
            ));
        }
        if self.old_layout() {
            util::die(format!(
                "{} has the 0.4 layout, which cbar no longer reads: move central/ to {}/ (central.toml becomes exchange.toml) and each projects/<name>/ to {}/<name>/",
                util::short(&self.root),
                util::short(&ex),
                util::short(ex.join("projects"))
            ));
        }
    }

    pub fn default_exchange(&self) -> String {
        toml_get(&self.config(), "default_exchange").filter(|s| valid_name(s)).unwrap_or_else(|| DEFAULT_EXCHANGE.into())
    }

    pub fn set_default_exchange(&self, ex: &str) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        write_file(&self.config(), &format!("default_exchange = {}\n", toml_str(ex)))
    }

    pub fn exchange_exists(&self, ex: &str) -> bool {
        valid_name(ex) && self.exchange_toml(ex).is_file()
    }

    pub fn project_exists(&self, ex: &str, p: &str) -> bool {
        valid_name(ex) && valid_name(p) && self.project_toml(ex, p).is_file()
    }

    /// Exchanges in name order, the default first.
    pub fn exchanges(&self) -> Vec<Exchange> {
        let mut names: Vec<String> = fs::read_dir(self.exchanges_dir())
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.retain(|n| self.exchange_exists(n));
        let first = self.default_exchange();
        names.sort_by_key(|n| (n != &first, n.clone()));
        names.into_iter().filter_map(|n| self.exchange(&n)).collect()
    }

    pub fn exchange(&self, ex: &str) -> Option<Exchange> {
        let f = self.exchange_toml(ex);
        if !f.is_file() {
            return None;
        }
        Some(Exchange {
            name: ex.to_string(),
            terminal: toml_get(&f, "terminal").and_then(|t| Term::parse(&t)),
            path: toml_get(&f, "path").filter(|p| !p.trim().is_empty()),
        })
    }

    /// The folder exchange EX's supervisor and agents start in: `path` in its
    /// exchange.toml, else the exchange's own folder (never ~, which Claude
    /// Code asks to trust each time it starts there).
    pub fn exchange_work_dir(&self, ex: &str) -> PathBuf {
        match self.exchange(ex).and_then(|x| x.path) {
            Some(p) => expand_home(&p),
            None => self.exchange_dir(ex),
        }
    }

    /// The projects of exchange EX in name order.
    pub fn projects(&self, ex: &str) -> Vec<Project> {
        let mut names: Vec<String> = fs::read_dir(self.exchange_dir(ex).join("projects"))
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names.into_iter().filter_map(|n| self.project(ex, &n)).collect()
    }

    pub fn project(&self, ex: &str, p: &str) -> Option<Project> {
        let f = self.project_toml(ex, p);
        if !f.is_file() {
            return None;
        }
        Some(Project {
            exchange: ex.to_string(),
            name: toml_get(&f, "name").unwrap_or_else(|| p.to_string()),
            path: toml_get(&f, "path").unwrap_or_default(),
            terminal: toml_get(&f, "terminal").and_then(|t| Term::parse(&t)).unwrap_or(Term::Rex),
        })
    }

    /// Write exchanges/EX/projects/P/project.toml (keeping its created time)
    /// and make briefs/ and reports/.
    pub fn register_project(&self, ex: &str, p: &str, dir: &Path, term: Term) -> std::io::Result<()> {
        let home = self.project_dir(ex, p);
        fs::create_dir_all(home.join("briefs"))?;
        fs::create_dir_all(home.join("reports"))?;
        let f = home.join("project.toml");
        let created = toml_get(&f, "created").unwrap_or_else(util::local_iso_now);
        write_file(
            &f,
            &format!(
                "name = {}\npath = {}\nterminal = {}\ncreated = {created}\n",
                toml_str(p),
                toml_str(&dir.to_string_lossy()),
                toml_str(term.as_str())
            ),
        )
    }

    /// Write exchanges/EX/exchange.toml the same way, with briefs/, reports/
    /// and projects/. It keeps the `path` already there unless PATH is given.
    pub fn register_exchange(&self, ex: &str, term: Term, path: Option<&Path>) -> std::io::Result<()> {
        let home = self.exchange_dir(ex);
        for d in ["briefs", "reports", "projects"] {
            fs::create_dir_all(home.join(d))?;
        }
        let f = home.join("exchange.toml");
        let created = toml_get(&f, "created").unwrap_or_else(util::local_iso_now);
        let path = path.map(|p| p.to_string_lossy().into_owned()).or_else(|| toml_get(&f, "path"));
        let path = path.map(|p| format!("path = {}\n", toml_str(&p))).unwrap_or_default();
        write_file(&f, &format!("name = {}\nterminal = {}\n{path}created = {created}\n", toml_str(ex), toml_str(term.as_str())))
    }

    /// The role prompt NAME.md: the installed copy, else the one built in.
    pub fn role_prompt(&self, name: &str) -> String {
        if let Ok(s) = fs::read_to_string(self.root.join(format!("{name}.md"))) {
            return s;
        }
        match name {
            "operator" => include_str!("../../operator.md").to_string(),
            _ => include_str!("../../supervisor.md").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> Home {
        let root = std::env::temp_dir().join(format!("cbar-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Home { root }
    }

    #[test]
    fn registers_and_reads_back() {
        let h = sandbox("reg");
        h.register_exchange("central", Term::Tmux, None).unwrap();
        h.register_project("central", "stats", Path::new("/tmp/a \"b\""), Term::Rex).unwrap();
        let created = toml_get(&h.project_toml("central", "stats"), "created").unwrap();
        h.register_project("central", "stats", Path::new("/tmp/c"), Term::Tmux).unwrap();
        assert_eq!(toml_get(&h.project_toml("central", "stats"), "created").unwrap(), created);
        assert!(crate::util::parse_iso(&created).is_some(), "{created}");
        let text = fs::read_to_string(h.project_toml("central", "stats")).unwrap();
        assert!(text.parse::<toml::Table>().unwrap()["created"].is_datetime(), "{text}");
        let p = h.project("central", "stats").unwrap();
        assert_eq!((p.path.as_str(), p.terminal, p.session()), ("/tmp/c", Term::Tmux, "central/stats".to_string()));
        assert_eq!(h.exchanges().len(), 1);
        assert!(!h.old_layout());
        let _ = fs::remove_dir_all(&h.root);
    }

    #[test]
    fn an_exchange_starts_in_its_path() {
        let h = sandbox("expath");
        h.register_exchange("central", Term::Rex, None).unwrap();
        // no path: the exchange's own folder, not ~
        assert_eq!(h.exchange_work_dir("central"), h.exchange_dir("central"));
        assert_eq!(h.exchange("central").unwrap().path, None);
        // a path written by hand, with ~, survives cbar rewriting the file
        let f = h.exchange_toml("central");
        let text = fs::read_to_string(&f).unwrap();
        fs::write(&f, format!("{text}path = \"~/central\"\n")).unwrap();
        h.register_exchange("central", Term::Tmux, None).unwrap();
        assert_eq!(h.exchange("central").unwrap().path.as_deref(), Some("~/central"));
        assert_eq!(h.exchange_work_dir("central"), util::home().join("central"));
        assert_eq!(h.exchange("central").unwrap().terminal, Some(Term::Tmux));
        // one given replaces it
        h.register_exchange("central", Term::Tmux, Some(Path::new("/srv/x"))).unwrap();
        assert_eq!(h.exchange_work_dir("central"), PathBuf::from("/srv/x"));
        assert!(fs::read_to_string(&f).unwrap().parse::<toml::Table>().is_ok());
        assert_eq!(expand_home("~"), util::home());
        assert_eq!(expand_home("~x/y"), PathBuf::from("~x/y"));
        let _ = fs::remove_dir_all(&h.root);
    }

    #[test]
    fn old_layout_old_layout() {
        let h = sandbox("old");
        fs::create_dir_all(h.root.join("projects/stats")).unwrap();
        assert!(h.old_layout());
        let _ = fs::remove_dir_all(&h.root);
    }

    #[test]
    fn names() {
        assert!(valid_name("stats-2_x"));
        assert!(!valid_name("a.b") && !valid_name("a/b") && !valid_name(""));
        assert!(valid_task("fix.x") && !valid_task("..") && !valid_task("a b"));
    }
}
