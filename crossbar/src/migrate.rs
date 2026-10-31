//! cbar migrate: move a 0.4 layout into an exchange.
//!
//!   central/{briefs,reports,…}, central/central.toml  →  exchanges/main/…, exchange.toml
//!   projects/<project>/                               →  exchanges/main/projects/<project>/
//!
//! It copies both to backups/0.4-<time>/ first, then leaves `central` and
//! `projects` as links to their new places, so paths that running Claudes
//! were given still work. It renames the sessions too (`central` → `main`,
//! `stats` → `main/stats`) unless --no-sessions; the programs in them keep
//! running. Safe to re-run: whatever already moved is left alone.

use std::fs;
use std::path::{Path, PathBuf};

use crate::layout::{self, toml_get, toml_str, Home};
use crate::mux::{self, Term};
use crate::util::{self, die, short};

const USAGE: &str = "cbar migrate [-n] [--exchange NAME] [--central SESSION] [--no-sessions]";

struct Plan {
    dry: bool,
    steps: usize,
}

impl Plan {
    fn say(&mut self, what: String) {
        self.steps += 1;
        println!("{}{what}", if self.dry { "would " } else { "" });
    }
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let kind = fs::symlink_metadata(&src)?.file_type();
        if kind.is_dir() {
            copy_dir(&src, &dst)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Move FROM to TO; a folder whose destination exists is merged entry by
/// entry. A file that exists in both places stays where it is, with a warning.
fn move_into(plan: &mut Plan, from: &Path, to: &Path) -> bool {
    if !to.exists() {
        plan.say(format!("move {} → {}", short(from), short(to)));
        if !plan.dry {
            if let Some(parent) = to.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::rename(from, to) {
                util::warn(format!("could not move {}: {e}", short(from)));
                return false;
            }
        }
        return true;
    }
    if is_real_dir(from) && to.is_dir() {
        let mut all = true;
        for e in entries(from) {
            all &= move_into(plan, &e, &to.join(e.file_name().unwrap()));
        }
        if all && !plan.dry {
            let _ = fs::remove_dir(from);
        }
        return all;
    }
    util::warn(format!("{} is already at {}; left it where it is", short(from), short(to)));
    false
}

fn link(plan: &mut Plan, at: &Path, to: &Path) {
    // (a dry run hasn't moved the folder that will make way for the link)
    if fs::symlink_metadata(at).is_ok() && !(plan.dry && is_real_dir(at)) {
        return;
    }
    plan.say(format!("link {} → {}", short(at), short(to)));
    if !plan.dry {
        if let Err(e) = std::os::unix::fs::symlink(to, at) {
            util::warn(format!("could not link {}: {e}", short(at)));
        }
    }
}

/// Session NAME, if it runs: its terminal (PREF's first).
fn running(name: &str, pref: Option<Term>) -> Option<Term> {
    let first = pref.unwrap_or(Term::Rex);
    let other = if first == Term::Rex { Term::Tmux } else { Term::Rex };
    [first, other].into_iter().find(|t| mux::session_running(name, *t))
}

pub fn main(home: &Home, args: Vec<String>) {
    let mut dry = false;
    let mut sessions = true;
    let mut ex = layout::DEFAULT_EXCHANGE.to_string();
    let mut central = "central".to_string();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" | "--dry-run" => dry = true,
            "--no-sessions" => sessions = false,
            "--exchange" => ex = it.next().unwrap_or_else(|| die(format!("usage: {USAGE}"))),
            "--central" => central = it.next().unwrap_or_else(|| die(format!("usage: {USAGE}"))),
            _ => die(format!("usage: {USAGE}")),
        }
    }
    if !layout::valid_name(&ex) {
        die("an exchange name is letters, digits, '_' and '-'");
    }
    let root = &home.root;
    let old_central = root.join("central");
    let old_projects = root.join("projects");
    let dest = home.exchange_dir(&ex);
    let mut plan = Plan { dry, steps: 0 };

    // the projects to rename sessions for: those still in projects/, and those already moved
    let mut projects: Vec<(String, Term)> = Vec::new();
    let mut add_project = |toml: &Path| {
        if let Some(name) = toml.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()) {
            let term = toml_get(toml, "terminal").and_then(|t| Term::parse(&t)).unwrap_or(Term::Rex);
            if !projects.iter().any(|(n, _)| *n == name) {
                projects.push((name, term));
            }
        }
    };
    for dir in [&old_projects, &dest.join("projects")] {
        for p in entries(dir) {
            add_project(&p.join("project.toml"));
        }
    }
    let central_term = toml_get(&old_central.join("central.toml"), "terminal").or_else(|| toml_get(&home.exchange_toml(&ex), "terminal")).and_then(|t| Term::parse(&t));
    let created = toml_get(&old_central.join("central.toml"), "created");

    let moving = is_real_dir(&old_central) || is_real_dir(&old_projects);
    if moving {
        let base = root.join("backups").join(format!("0.4-{}", util::stamp()));
        let mut backup = base.clone();
        for n in 2.. {
            if !backup.exists() {
                break;
            }
            backup = PathBuf::from(format!("{}-{n}", base.display()));
        }
        plan.say(format!("back up central/ and projects/ to {}", short(&backup)));
        if !dry {
            for (dir, name) in [(&old_central, "central"), (&old_projects, "projects")] {
                if is_real_dir(dir) {
                    if let Err(e) = copy_dir(dir, &backup.join(name)) {
                        die(format!("could not back up {}: {e}; nothing was moved", short(dir)));
                    }
                }
            }
        }
    }

    // the exchange's own files
    if !home.exchange_toml(&ex).exists() {
        let term = central_term.or_else(|| running(&central, None)).or_else(|| running(&ex, None)).unwrap_or(Term::Rex);
        plan.say(format!("write {} (terminal {})", short(home.exchange_toml(&ex)), term.as_str()));
        if !dry {
            if let Err(e) = home.register_exchange(&ex, term) {
                die(format!("could not write {}: {e}", short(home.exchange_toml(&ex))));
            }
            if let Some(c) = &created {
                let text = format!("name = {}\nterminal = {}\ncreated = {c}\n", toml_str(&ex), toml_str(term.as_str()));
                let _ = fs::write(home.exchange_toml(&ex), text);
            }
        }
    }
    if is_real_dir(&old_central) {
        for e in entries(&old_central) {
            if e.file_name().is_some_and(|n| n == "central.toml") {
                plan.say(format!("remove {} (now {})", short(&e), short(home.exchange_toml(&ex))));
                if !dry {
                    let _ = fs::remove_file(&e);
                }
                continue;
            }
            move_into(&mut plan, &e, &dest.join(e.file_name().unwrap()));
        }
        if !dry && fs::remove_dir(&old_central).is_err() {
            util::warn(format!("{} still has files; left it", short(&old_central)));
        }
    }
    if is_real_dir(&old_projects) {
        for p in entries(&old_projects) {
            move_into(&mut plan, &p, &dest.join("projects").join(p.file_name().unwrap()));
        }
        if !dry && fs::remove_dir(&old_projects).is_err() {
            util::warn(format!("{} still has files; left it", short(&old_projects)));
        }
    }
    // a migration (this one, or one cut short) gets its links and its note in config.toml
    let ours = moving || home.migrated().is_some() || fs::symlink_metadata(&old_central).is_ok_and(|m| m.file_type().is_symlink());
    if ours {
        link(&mut plan, &old_central, &dest);
        link(&mut plan, &old_projects, &dest.join("projects"));
        let default = toml_get(&home.config(), "default_exchange");
        if default.is_none() || home.migrated().is_none() {
            let default = default.unwrap_or_else(|| ex.clone());
            plan.say(format!("write {} (default_exchange {default})", short(home.config())));
            if !dry {
                let _ = home.write_config(&default, Some(&home.migrated().unwrap_or_else(util::local_iso_now)));
            }
        }
    }

    if sessions {
        if let Some(term) = running(&central, central_term) {
            if running(&ex, Some(term)).is_none() {
                plan.say(format!("rename the {} session '{central}' → '{ex}'", term.as_str()));
                if !dry {
                    if let Err(e) = mux::rename_session(term, &central, &ex) {
                        util::warn(format!("could not rename '{central}': {e}"));
                    }
                }
            }
        }
        for (p, pterm) in &projects {
            let new = layout::session_of(&ex, Some(p));
            if mux::session_running(p, *pterm) && !mux::session_running(&new, *pterm) {
                plan.say(format!("rename the {} session '{p}' → '{new}'", pterm.as_str()));
                if !dry {
                    if let Err(e) = mux::rename_session(*pterm, p, &new) {
                        util::warn(format!("could not rename '{p}': {e}"));
                    }
                }
            }
        }
    }
    if plan.steps == 0 {
        println!("nothing to migrate: {} is in exchanges/ already", short(root));
    } else if dry {
        println!("(dry run: nothing changed)");
    } else {
        println!("migrated {} into exchange '{ex}'", short(root));
    }
}
