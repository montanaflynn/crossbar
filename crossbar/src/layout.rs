//! The ~/.crossbar layout ($CROSSBAR_HOME):
//!
//!   config.toml                     default_exchange = "main"
//!   operator.md, supervisor.md      role prompts, version, bin/
//!   exchanges/<exchange>/
//!     exchange.toml                 name, terminal, created
//!     briefs/ reports/              the supervisor's agents
//!     projects/<project>/
//!       project.toml                name, path, terminal, created
//!       briefs/ reports/            the operator's agents
//!
//! An exchange's supervisor runs in the session named after it (`main`), each
//! of its projects in `<exchange>/<project>` (`main/stats`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::mux::Term;
use crate::util;

pub const SEP: char = '/';
pub const DEFAULT_EXCHANGE: &str = "main";
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
        v => Some(v.to_string()),
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
    /// real folders, no exchanges/), which `cbar migrate` converts.
    pub fn needs_migrate(&self) -> bool {
        let real_dir = |p: PathBuf| fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
        !self.exchanges_dir().is_dir() && (real_dir(self.root.join("projects")) || real_dir(self.root.join("central")))
    }

    /// Stop with a pointer to `cbar migrate` if this home has the old layout.
    pub fn require_migrated(&self) {
        if self.needs_migrate() {
            util::die(format!(
                "{} has the 0.4 layout (central/, projects/); run `cbar migrate` to move it into exchanges/{}",
                util::short(&self.root),
                DEFAULT_EXCHANGE
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
        Some(Exchange { name: ex.to_string(), terminal: toml_get(&f, "terminal").and_then(|t| Term::parse(&t)) })
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
    /// and projects/.
    pub fn register_exchange(&self, ex: &str, term: Term) -> std::io::Result<()> {
        let home = self.exchange_dir(ex);
        for d in ["briefs", "reports", "projects"] {
            fs::create_dir_all(home.join(d))?;
        }
        let f = home.join("exchange.toml");
        let created = toml_get(&f, "created").unwrap_or_else(util::local_iso_now);
        write_file(&f, &format!("name = {}\nterminal = {}\ncreated = {created}\n", toml_str(ex), toml_str(term.as_str())))
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
        h.register_exchange("main", Term::Tmux).unwrap();
        h.register_project("main", "stats", Path::new("/tmp/a \"b\""), Term::Rex).unwrap();
        let created = toml_get(&h.project_toml("main", "stats"), "created").unwrap();
        h.register_project("main", "stats", Path::new("/tmp/c"), Term::Tmux).unwrap();
        assert_eq!(toml_get(&h.project_toml("main", "stats"), "created").unwrap(), created);
        let p = h.project("main", "stats").unwrap();
        assert_eq!((p.path.as_str(), p.terminal, p.session()), ("/tmp/c", Term::Tmux, "main/stats".to_string()));
        assert_eq!(h.exchanges().len(), 1);
        assert!(!h.needs_migrate());
        let _ = fs::remove_dir_all(&h.root);
    }

    #[test]
    fn old_layout_needs_migrate() {
        let h = sandbox("old");
        fs::create_dir_all(h.root.join("projects/stats")).unwrap();
        assert!(h.needs_migrate());
        let _ = fs::remove_dir_all(&h.root);
    }

    #[test]
    fn names() {
        assert!(valid_name("stats-2_x"));
        assert!(!valid_name("a.b") && !valid_name("a/b") && !valid_name(""));
        assert!(valid_task("fix.x") && !valid_task("..") && !valid_task("a b"));
    }
}
