//! cbar's paths: `exchange/project/agent`, where depth says what a path
//! names. Paths are relative to where you are: inside a project, `fix-x` is
//! that project's agent; inside an exchange, `stats` is that exchange's
//! project. A name that isn't found nearby is looked up a level higher, so
//! `stats` still finds the project from inside another project. `/central/stats`
//! is absolute; `..` steps up. A tab of the exchange's own session (the
//! supervisor's agents) sits at a project's depth: `central/research`.

use crate::layout::{self, Home, OPERATOR, SUPERVISOR};
use crate::mux;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Root,
    Exchange(String),
    Project(String, String),
    /// A tab: of an exchange's session (project None) or a project's.
    Agent { exchange: String, project: Option<String>, name: String },
}

impl Node {
    pub fn parent(&self) -> Option<Node> {
        match self {
            Node::Root => None,
            Node::Exchange(_) => Some(Node::Root),
            Node::Project(e, _) => Some(Node::Exchange(e.clone())),
            Node::Agent { exchange, project: Some(p), .. } => Some(Node::Project(exchange.clone(), p.clone())),
            Node::Agent { exchange, project: None, .. } => Some(Node::Exchange(exchange.clone())),
        }
    }

    pub fn exchange(&self) -> Option<&str> {
        match self {
            Node::Root => None,
            Node::Exchange(e) | Node::Project(e, _) => Some(e),
            Node::Agent { exchange, .. } => Some(exchange),
        }
    }

    pub fn project(&self) -> Option<&str> {
        match self {
            Node::Project(_, p) => Some(p),
            Node::Agent { project, .. } => project.as_deref(),
            _ => None,
        }
    }

    /// The session this node runs in (an agent's is its parent's).
    pub fn session(&self) -> Option<String> {
        let e = self.exchange()?;
        Some(layout::session_of(e, self.project()))
    }

    /// The tab that holds this node's Claude: an agent's own, a project's
    /// operator, an exchange's supervisor.
    pub fn tab(&self) -> Option<&str> {
        match self {
            Node::Root => None,
            Node::Exchange(_) => Some(SUPERVISOR),
            Node::Project(..) => Some(OPERATOR),
            Node::Agent { name, .. } => Some(name),
        }
    }

    /// The full path, e.g. central/stats/fix-x.
    pub fn path(&self) -> String {
        match self {
            Node::Root => "/".into(),
            Node::Exchange(e) => e.clone(),
            Node::Project(e, p) => format!("{e}/{p}"),
            Node::Agent { exchange, project: Some(p), name } => format!("{exchange}/{p}/{name}"),
            Node::Agent { exchange, project: None, name } => format!("{exchange}/{name}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Node::Root => "root",
            Node::Exchange(_) => "exchange",
            Node::Project(..) => "project",
            Node::Agent { .. } => "agent",
        }
    }
}

/// Where this process is: the project or exchange whose session it runs in,
/// else the default exchange.
pub fn here(home: &Home) -> Node {
    here_from(home, mux::here_session().as_deref())
}

pub fn here_from(home: &Home, session: Option<&str>) -> Node {
    let default = home.default_exchange();
    if let Some(s) = session {
        if let Some((e, p)) = s.split_once(layout::SEP) {
            if home.project_exists(e, p) || (home.exchange_exists(e) && layout::valid_name(p)) {
                return Node::Project(e.into(), p.into());
            }
        } else if home.exchange_exists(s) {
            return Node::Exchange(s.into());
        }
    }
    Node::Exchange(default)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// The node must exist.
    Existing,
    /// Its parent must exist; at an exchange's depth, AGENT says a tab of
    /// the exchange's session rather than a project.
    New { agent: bool },
}

/// Whether session SESSION has a tab TAB.
pub type TabCheck<'a> = &'a dyn Fn(&str, &str) -> bool;

pub fn tab_exists(session: &str, tab: &str) -> bool {
    let tabs = if mux::tmux_has(session) {
        let (_, out, _) = mux::tmux_s(&["list-windows", "-t", &format!("={session}"), "-F", "#{window_name}"]);
        out.lines().map(String::from).collect::<Vec<_>>()
    } else {
        mux::rex_json(&["window", "ls", "-s", session, "--json"])
            .and_then(|v| v["windows"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|w| w["label"].as_str().map(String::from))
            .collect()
    };
    tabs.iter().any(|t| t == tab)
}

fn exists(home: &Home, node: &Node, tabs: TabCheck) -> bool {
    match node {
        Node::Root => true,
        Node::Exchange(e) => home.exchange_exists(e),
        Node::Project(e, p) => home.project_exists(e, p),
        Node::Agent { name, project, .. } => {
            let parent = node.parent().unwrap();
            exists(home, &parent, tabs) && {
                let reserved = if project.is_some() { OPERATOR } else { SUPERVISOR };
                name == reserved || tabs(&parent.session().unwrap(), name)
            }
        }
    }
}

/// BASE + COMPS as a node, or None if that is too deep or not there.
fn descend(home: &Home, base: &Node, comps: &[&str], want: Want, tabs: TabCheck) -> Option<Node> {
    let mut node = base.clone();
    for (i, c) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        node = match node {
            Node::Root => Node::Exchange(c.to_string()),
            Node::Exchange(e) => {
                if home.project_exists(&e, c) || !last {
                    Node::Project(e, c.to_string())
                } else {
                    let agent = Node::Agent { exchange: e.clone(), project: None, name: c.to_string() };
                    match want {
                        Want::New { agent: true } => agent,
                        Want::New { agent: false } => Node::Project(e, c.to_string()),
                        Want::Existing if exists(home, &agent, tabs) => agent,
                        Want::Existing => Node::Project(e, c.to_string()),
                    }
                }
            }
            Node::Project(e, p) => Node::Agent { exchange: e, project: Some(p), name: c.to_string() },
            Node::Agent { .. } => return None,
        };
    }
    let ok = match want {
        Want::Existing => exists(home, &node, tabs),
        Want::New { .. } => node.parent().is_none_or(|p| exists(home, &p, tabs)),
    };
    ok.then_some(node)
}

/// PATH as seen from HERE.
pub fn resolve(home: &Home, here: &Node, path: &str, want: Want, tabs: TabCheck) -> Result<Node, String> {
    let mut comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let mut bases = if path.starts_with('/') {
        vec![Node::Root]
    } else {
        let mut chain = vec![here.clone()];
        while let Some(p) = chain.last().unwrap().parent() {
            chain.push(p);
        }
        chain
    };
    if comps.first() == Some(&"..") {
        let mut base = bases[0].clone();
        while comps.first() == Some(&"..") {
            base = base.parent().unwrap_or(Node::Root);
            comps.remove(0);
        }
        bases = vec![base];
    }
    if comps.contains(&"..") {
        return Err(format!("'{path}': '..' only goes at the start"));
    }
    if comps.is_empty() {
        return Ok(bases.remove(0));
    }
    for base in &bases {
        if let Some(node) = descend(home, base, &comps, want, tabs) {
            return Ok(node);
        }
    }
    Err(match want {
        Want::Existing => format!("nothing at '{path}' (cbar ls)"),
        Want::New { .. } => format!("no place for '{path}': its parent doesn't exist (cbar ls)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::Term;
    use std::path::Path;

    fn home(tag: &str) -> Home {
        let root = std::env::temp_dir().join(format!("cbar-paths-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let h = Home { root };
        for e in ["central", "work"] {
            h.register_exchange(e, Term::Tmux, None).unwrap();
        }
        h.register_project("central", "stats", Path::new("/tmp"), Term::Tmux).unwrap();
        h.register_project("central", "grail", Path::new("/tmp"), Term::Tmux).unwrap();
        h.register_project("work", "stats", Path::new("/tmp"), Term::Tmux).unwrap();
        h
    }

    fn agent(e: &str, p: Option<&str>, n: &str) -> Node {
        Node::Agent { exchange: e.into(), project: p.map(String::from), name: n.into() }
    }

    #[test]
    fn resolves_by_depth_and_place() {
        let h = home("resolve");
        let tabs = |s: &str, t: &str| (s, t) == ("central/stats", "fix-x") || (s, t) == ("central", "research");
        let r = |here: &Node, p: &str, w: Want| resolve(&h, here, p, w, &tabs);
        let ex = Node::Exchange("central".into());
        let pr = Node::Project("central".into(), "grail".into());
        let any = Want::Existing;
        let new = Want::New { agent: false };

        assert_eq!(r(&ex, "stats", any), Ok(Node::Project("central".into(), "stats".into())));
        assert_eq!(r(&ex, "stats/fix-x", any), Ok(agent("central", Some("stats"), "fix-x")));
        assert_eq!(r(&ex, "work/stats", any), Ok(Node::Project("work".into(), "stats".into())));
        assert_eq!(r(&ex, "research", any), Ok(agent("central", None, "research")));
        assert_eq!(r(&ex, "supervisor", any), Ok(agent("central", None, "supervisor")));
        assert_eq!(r(&ex, "stats/operator", any), Ok(agent("central", Some("stats"), "operator")));
        assert!(r(&ex, "nope", any).is_err());

        // inside a project: its agents first, then the exchange's projects
        assert_eq!(r(&pr, "stats", any), Ok(Node::Project("central".into(), "stats".into())));
        assert_eq!(r(&pr, "../stats", any), Ok(Node::Project("central".into(), "stats".into())));
        assert_eq!(r(&pr, "..", any), Ok(ex.clone()));
        assert_eq!(r(&pr, "/work/stats", any), Ok(Node::Project("work".into(), "stats".into())));
        assert_eq!(r(&pr, "new-task", new), Ok(agent("central", Some("grail"), "new-task")));

        // new things
        assert_eq!(r(&ex, "fresh", new), Ok(Node::Project("central".into(), "fresh".into())));
        assert_eq!(r(&ex, "fresh", Want::New { agent: true }), Ok(agent("central", None, "fresh")));
        assert_eq!(r(&ex, "stats/fix-y", new), Ok(agent("central", Some("stats"), "fix-y")));
        assert_eq!(r(&ex, "central/fresh", new), Ok(Node::Project("central".into(), "fresh".into())));
        assert_eq!(r(&ex, "/personal", new), Ok(Node::Exchange("personal".into())));
        assert!(r(&ex, "nope/x/y", new).is_err());
        let _ = std::fs::remove_dir_all(&h.root);
    }

    #[test]
    fn knows_where_it_is() {
        let h = home("here");
        assert_eq!(here_from(&h, Some("central/stats")), Node::Project("central".into(), "stats".into()));
        assert_eq!(here_from(&h, Some("work")), Node::Exchange("work".into()));
        assert_eq!(here_from(&h, Some("grail")), Node::Exchange("central".into())); // not a 0.4 session name
        assert_eq!(here_from(&h, Some("elsewhere")), Node::Exchange("central".into()));
        assert_eq!(here_from(&h, None), Node::Exchange("central".into()));
        let _ = std::fs::remove_dir_all(&h.root);
    }
}
