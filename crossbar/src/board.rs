//! switchboard: a live overview of the sessions of an exchange (or of every
//! exchange), and a way to jump to any pane.
//!
//! Each session is a card; each pane a row with its tab name, program status
//! (idle / working / done / blocked / error) and how long it has been in that
//! state. Rex reports status via OSC 7501; in tmux it comes from claude-relay
//! or `claude agents` (see status.rs).
//!
//!   switchboard                   the main board: this exchange's sessions
//!                                 (the one this pane's session belongs to,
//!                                 else the default exchange)
//!   switchboard --exchange NAME   the main board of exchange NAME
//!   switchboard --all             every session, by exchange
//!   switchboard --session NAME    live view of one session: a card per tab
//!   switchboard --here            same, for this pane's session ($TMUX_PANE or $REX_SESSION)
//!   switchboard --once            print once and exit (combines with the above)
//!   switchboard --json            print the panes as JSON and exit (combines with the above)
//!   switchboard --watch --here    print a line as each report turns done, blocked or error,
//!                                 stops or closes (reports: this session's agents; in an
//!                                 exchange's session, also each of its projects' operators)
//!
//! $CROSSBAR_TMUX_SOCKET names a tmux server other than the default (tmux -L).
//!
//! Keys:  j/k ↑/↓ select · J/K tab/shift-tab next/prev session · g/G first/last
//!        enter jump to pane · 1-9 jump to session N · a this exchange / all
//!        exchanges · q/esc quit (scoped to one session, "session" above means
//!        tab; 0/~ jumps to the exchange's main switchboard)
//! Mouse: click a row to jump to that pane, click a card header to jump to that
//!        session, wheel moves the selection; scoped, click "↖ <exchange>" in
//!        the header to jump to the main switchboard.
//!
//! Jumping asks the Rex app (a client) to show the session and tab with the
//! client action session.select, then focuses the pane. In tmux it selects
//! the window and pane and switches this client (or, outside tmux, the most
//! recently active one) to the session.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::layout::{Home, SEP};
use crate::mux::{self, rex_json};
use crate::status::{self, rank, Group, Looker, Pane, Snapshot};
use crate::util::{self, fit};

// CarbonFox
const FG: Color = Color::Rgb(0xf2, 0xf4, 0xf8);
const MUTED: Color = Color::Rgb(0x7b, 0x7c, 0x7e);
const FAINT: Color = Color::Rgb(0x35, 0x35, 0x35);
const DIM: Color = Color::Rgb(0x52, 0x52, 0x53);
const ACCENT: Color = Color::Rgb(0xbe, 0x95, 0xff);
const SEL: Color = Color::Rgb(0x2a, 0x2a, 0x2a);
const BLOCKED: Color = Color::Rgb(0xee, 0x53, 0x96);
const ERROR: Color = Color::Rgb(0xff, 0x7e, 0xb6);
const DONE: Color = Color::Rgb(0x25, 0xbe, 0x6a);
const WORKING: Color = Color::Rgb(0x78, 0xa9, 0xff);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn color(state: &str) -> Option<Color> {
    match state {
        "blocked" => Some(BLOCKED),
        "error" => Some(ERROR),
        "done" => Some(DONE),
        "working" => Some(WORKING),
        "idle" => Some(MUTED),
        _ => None,
    }
}

fn glyph(state: &str, tick: usize) -> String {
    match state {
        "working" => SPINNER[tick % SPINNER.len()].to_string(),
        "blocked" => "◆".into(),
        "error" => "✗".into(),
        "done" => "✓".into(),
        "idle" => "○".into(),
        _ => "·".into(),
    }
}

/// What a click or a key on a line means.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Session(String),
    Tab(String, String),
    Pane(String, String, String), // session, window, pane
}

fn s(text: impl Into<String>, fg: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(fg))
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Which sessions a main board shows, and how it names them.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Exchange(String),
    All,
    /// One session's tabs: its id, and its label as shown.
    Scoped(String, String),
}

/// A card's title: a project's session without its exchange's prefix.
fn title(label: &str, view: &View) -> String {
    match view {
        View::Exchange(e) => label.strip_prefix(&format!("{e}{SEP}")).unwrap_or(label).to_string(),
        _ => label.to_string(),
    }
}

/// The sessions of DATA that VIEW shows, in order: in an exchange, its own
/// session first; for all, exchange by exchange and the rest last.
pub fn pick(data: &Snapshot, view: &View, exchanges: &[String]) -> Snapshot {
    let of = |label: &str| -> Option<usize> {
        let ex = label.split(SEP).next().unwrap_or(label);
        exchanges.iter().position(|e| e == ex)
    };
    match view {
        View::Scoped(..) => data.clone(),
        View::Exchange(e) => {
            let mut out: Snapshot =
                data.iter().filter(|(g, _)| g.label == *e || g.label.starts_with(&format!("{e}{SEP}"))).cloned().collect();
            out.sort_by_key(|(g, _)| g.label != *e);
            out
        }
        View::All => {
            let mut out = data.clone();
            out.sort_by_key(|(g, _)| (of(&g.label).unwrap_or(usize::MAX), g.label.contains(SEP)));
            out
        }
    }
}

type Lines = (Vec<Line<'static>>, Vec<Option<Target>>);

/// Lines for one card (a session, or scoped a tab), with a target per line.
fn card(number: usize, group: &Group, name: &str, panes: &[Pane], cols: usize, tick: usize, selected: Option<&str>, scoped: bool) -> Lines {
    let mut panes = panes.to_vec();
    panes.sort_by_key(|p| rank(&p.state));
    let top = panes.first().map(|p| p.state.as_str()).unwrap_or("");
    let edge = if ["blocked", "error", "done", "working"].contains(&top) { color(top).unwrap() } else { FAINT };
    let inner = cols.saturating_sub(4);
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut first_seen = Vec::new();
    for p in &panes {
        if !p.cwd.is_empty() {
            if !counts.contains_key(p.cwd.as_str()) {
                first_seen.push(p.cwd.as_str());
            }
            *counts.entry(p.cwd.as_str()).or_insert(0) += 1;
        }
    }
    // the most common folder (the first seen of the most common, as Counter does)
    let home = first_seen.iter().max_by(|a, b| counts[*a].cmp(&counts[*b]).then(std::cmp::Ordering::Greater)).map(|s| s.to_string()).unwrap_or_default();
    let sid = group.session_id.clone();
    let head_target = if scoped {
        Target::Tab(sid.clone(), group.window_id.clone().unwrap_or_default())
    } else {
        Target::Session(sid.clone())
    };

    let num = if number <= 9 { format!("{number} ") } else { String::new() };
    let tag = if group.mux == "tmux" && !scoped { " tmux" } else { "" };
    let title_len = format!(" {num}{name}{tag} ").chars().count();
    let path = fit(&home, inner.saturating_sub(title_len + 4));
    let path_part = if path.is_empty() { String::new() } else { format!(" {path} ") };
    let rule = "─".repeat((cols as isize - 3 - title_len as isize - path_part.chars().count() as isize - 1).max(1) as usize);
    let mut lines = vec![Line::from(vec![
        s("╭─", edge),
        Span::raw(" "),
        s(num.clone(), MUTED),
        Span::styled(name.to_string(), Style::default().fg(FG).add_modifier(Modifier::BOLD)),
        s(tag, DIM),
        Span::raw(" "),
        s(rule, edge),
        s(path_part, MUTED),
        s("─╮", edge),
    ])];
    let mut targets = vec![Some(head_target.clone())];

    let mut row = |content: Vec<Span<'static>>, target: Option<Target>, hot: bool, lines: &mut Vec<Line<'static>>| {
        let pad = inner.saturating_sub(width(&content));
        let mut spans = vec![s("│", edge)];
        if hot {
            spans.push(Span::styled("▎", Style::default().fg(ACCENT).bg(SEL)));
            for c in content {
                let st = c.style.bg(SEL);
                spans.push(Span::styled(c.content, st));
            }
            spans.push(Span::styled(" ".repeat(pad + 1), Style::default().bg(SEL)));
        } else {
            spans.push(Span::raw(" "));
            spans.extend(content);
            spans.push(Span::raw(" ".repeat(pad + 1)));
        }
        spans.push(s("│", edge));
        lines.push(Line::from(spans));
        targets.push(target);
    };

    if panes.is_empty() {
        row(vec![Span::styled("no panes", Style::default().fg(MUTED).add_modifier(Modifier::ITALIC))], Some(head_target.clone()), false, &mut lines);
    }
    let mut agents_seen = false;
    for p in &panes {
        let is_agent = !p.state.is_empty();
        if is_agent {
            agents_seen = true;
        } else if agents_seen {
            row(vec![s("┄".repeat(inner), FAINT)], None, false, &mut lines);
            agents_seen = false;
        }
        let hot = selected == Some(p.block_id.as_str());
        let target = Target::Pane(sid.clone(), p.window_id.clone(), p.block_id.clone());
        let c = color(&p.state).unwrap_or(MUTED);
        let name = if scoped {
            // the card is the tab; name the pane by its own label
            if p.label.is_empty() || p.label == "shell" { p.program.clone() } else { p.label.clone() }
        } else if p.tab.is_empty() {
            p.program.clone()
        } else {
            p.tab.clone()
        };
        let name = fit(&name, 16);
        let board = !is_agent && p.program == "switchboard";
        let mark = if is_agent { glyph(&p.state, tick) } else if board { "◇".into() } else { "▪".into() };
        let word = if is_agent { p.state.clone() } else { p.program.clone() };
        let right = if is_agent { p.since.clone() } else if board { "view".into() } else { "process".into() };
        let name_color = if is_agent || hot { FG } else { MUTED };
        let mark_color = if is_agent { c } else if board { ACCENT } else { FAINT };
        let mut name_style = Style::default().fg(name_color);
        if hot {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let mut left = vec![
            s(mark, mark_color),
            Span::raw(" "),
            Span::styled(format!("{name:<16}"), name_style),
            Span::raw(" "),
            s(word, if is_agent { c } else { MUTED }),
        ];
        if !p.cwd.is_empty() && p.cwd != home {
            let room = inner as isize - width(&left) as isize - right.chars().count() as isize - 3;
            if room > 6 {
                left.push(Span::raw("  "));
                left.push(s(fit(&p.cwd, room as usize), MUTED));
            }
        }
        let gap = (inner as isize - width(&left) as isize - right.chars().count() as isize).max(1) as usize;
        left.push(Span::raw(" ".repeat(gap)));
        left.push(s(right, if is_agent { MUTED } else { FAINT }));
        row(left, Some(target.clone()), hot, &mut lines);
        let detail: Vec<&str> = [p.kind.as_str(), p.msg.as_str()].into_iter().filter(|x| !x.is_empty()).collect();
        if !detail.is_empty() && ["blocked", "error", "done"].contains(&p.state.as_str()) {
            let text = fit(&detail.join(" · "), inner.saturating_sub(2));
            row(vec![Span::raw("  "), Span::styled(text, Style::default().fg(c).add_modifier(Modifier::ITALIC))], Some(target), hot, &mut lines);
        }
    }
    lines.push(Line::from(s(format!("╰{}╯", "─".repeat(cols.saturating_sub(2))), edge)));
    targets.push(None);
    (lines, targets)
}

/// Columns [start, end) of the header's main link, in line coordinates.
fn link_span(cols: usize, link: &str) -> (usize, usize) {
    let cols = cols.clamp(36, 100);
    let end = cols - util::clock().chars().count() - 2;
    (end.saturating_sub(link.chars().count()), end)
}

pub struct Frame {
    pub lines: Vec<Line<'static>>,
    pub targets: Vec<Option<Target>>,
}

/// The whole board for DATA (already picked for VIEW). LINK names the main
/// board a scoped board links to; LINK_HOT colors it.
pub fn render(data: &Snapshot, view: &View, cols: usize, tick: usize, selected: Option<&str>, link: &str, link_hot: bool) -> Frame {
    let cols = cols.clamp(36, 100);
    let scoped = matches!(view, View::Scoped(..));
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut others = 0;
    for (_, panes) in data {
        for p in panes {
            if p.state.is_empty() {
                others += 1;
            } else {
                *counts.entry(p.state.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut pills: Vec<Span<'static>> = Vec::new();
    for st in status::ORDER {
        if let Some(n) = counts.get(st) {
            if !pills.is_empty() {
                pills.push(Span::raw("  "));
            }
            pills.push(s(format!("{} {n} {st}", glyph(st, tick)), color(st).unwrap()));
        }
    }
    if others > 0 {
        if !pills.is_empty() {
            pills.push(Span::raw("  "));
        }
        pills.push(s(format!("▪ {others} other {}", if others == 1 { "pane" } else { "panes" }), MUTED));
    }
    let clock = util::clock();
    let mut subtitle = match view {
        View::Scoped(_, label) => format!("{label} · tabs & agents"),
        View::Exchange(e) => format!("{e} · agents & panes by session"),
        View::All => "all exchanges · agents & panes by session".into(),
    };
    let brand = || Span::styled("◇ switchboard", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    let mut head;
    if scoped {
        let link_text = format!("↖ {link}");
        let (start, _) = link_span(cols, &link_text);
        subtitle = fit(&subtitle, start.saturating_sub("◇ switchboard ".chars().count() + 1).max(1));
        head = vec![brand(), Span::raw(" "), s(subtitle, MUTED)];
        let pad = (start as isize - width(&head) as isize).max(1) as usize;
        head.push(Span::raw(" ".repeat(pad)));
        head.push(s(link_text, if link_hot { ACCENT } else { DIM }));
        head.push(Span::raw("  "));
        head.push(s(clock, MUTED));
    } else {
        subtitle = fit(&subtitle, cols.saturating_sub("◇ switchboard ".chars().count() + clock.chars().count() + 1).max(1));
        head = vec![brand(), Span::raw(" "), s(subtitle, MUTED)];
        let pad = (cols as isize - width(&head) as isize - clock.chars().count() as isize).max(1) as usize;
        head.push(Span::raw(" ".repeat(pad)));
        head.push(s(clock, MUTED));
    }
    let mut lines = vec![Line::from(head), Line::from(if pills.is_empty() { vec![s("no agents", MUTED)] } else { pills }), Line::default()];
    let mut targets = vec![None, None, None];
    if data.is_empty() {
        let what = match view {
            View::Exchange(e) => format!("no sessions in exchange '{e}' (cbar up {e})"),
            _ => "no sessions".into(),
        };
        lines.push(Line::from(Span::styled(what, Style::default().fg(MUTED).add_modifier(Modifier::ITALIC))));
        targets.push(None);
    }
    for (n, (g, panes)) in data.iter().enumerate() {
        let (l, t) = card(n + 1, g, &title(&g.label, view), panes, cols, tick, selected, scoped);
        lines.extend(l);
        targets.extend(t);
    }
    Frame { lines, targets }
}

fn footer(cols: usize, status: Option<&(Color, String)>, view: &View, link: &str) -> Line<'static> {
    let scoped = matches!(view, View::Scoped(..));
    let group = if scoped { "tab" } else { "session" };
    let mut keys: Vec<(&str, String)> =
        vec![("↑↓", "select".into()), ("⇥", group.into()), ("⏎", "jump".into()), ("1-9", group.into()), ("click", "jump".into()), ("q", "quit".into())];
    match view {
        View::Scoped(..) => keys.insert(keys.len() - 1, ("0", link.to_string())),
        View::Exchange(_) => keys.insert(keys.len() - 1, ("a", "all".into())),
        View::All => keys.insert(keys.len() - 1, ("a", link.to_string())),
    }
    let hint = |keys: &[(&str, String)]| -> Vec<Span<'static>> {
        let mut out = Vec::new();
        for (i, (k, v)) in keys.iter().enumerate() {
            if i > 0 {
                out.push(Span::raw("  "));
            }
            out.push(s(k.to_string(), MUTED));
            out.push(Span::raw(" "));
            out.push(s(v.clone(), DIM));
        }
        out
    };
    let mut spans = hint(&keys);
    for spare in ["click", "⇥", "↑↓"] {
        // a narrow pane: drop hints rather than wrap
        if width(&spans) <= cols {
            break;
        }
        keys.retain(|(k, _)| *k != spare);
        spans = hint(&keys);
    }
    if let Some((c, msg)) = status {
        let msg = fit(msg, (cols as isize - width(&spans) as isize - 2).max(12) as usize);
        let gap = cols as isize - width(&spans) as isize - msg.chars().count() as isize;
        if gap < 2 {
            return Line::from(s(fit(&msg, cols), *c));
        }
        spans.push(Span::raw(" ".repeat(gap as usize)));
        spans.push(s(msg, *c));
    }
    Line::from(spans)
}

/// LINES as text with ANSI colors, as --once prints them.
pub fn ansi(lines: &[Line]) -> String {
    let code = |c: Color, bg: bool| match c {
        Color::Rgb(r, g, b) => format!("\x1b[{};2;{r};{g};{b}m", if bg { 48 } else { 38 }),
        _ => String::new(),
    };
    let mut out = String::new();
    for l in lines {
        for sp in &l.spans {
            let st = sp.style;
            let mut pre = String::new();
            if st.add_modifier.contains(Modifier::BOLD) {
                pre.push_str("\x1b[1m");
            }
            if st.add_modifier.contains(Modifier::ITALIC) {
                pre.push_str("\x1b[3m");
            }
            if let Some(c) = st.fg {
                pre.push_str(&code(c, false));
            }
            if let Some(c) = st.bg {
                pre.push_str(&code(c, true));
            }
            if pre.is_empty() {
                out.push_str(&sp.content);
            } else {
                out.push_str(&format!("{pre}{}\x1b[0m", sp.content));
            }
        }
        out.push('\n');
    }
    out
}

// ---- finding the main board ------------------------------------------------------

/// The main switchboard's pane: (session id, session label, window id, pane id).
pub type MainHit = (String, String, String, String);

/// How well a main board showing BOARD_EX (from its argv: "" for the
/// default, "*" for all) in session LABEL fits exchange EX: lower is better.
fn fit_score(board_ex: &str, label: &str, ex: &str, default: &str) -> Option<u8> {
    let shown = if board_ex.is_empty() {
        if label.split(SEP).next() == Some(ex) || (ex == default && !label.contains(SEP)) {
            ex
        } else {
            default
        }
    } else {
        board_ex
    };
    if shown == ex {
        Some(if label == ex { 0 } else { 1 })
    } else if shown == "*" {
        Some(2)
    } else {
        None
    }
}

fn tmux_find_main(look: &Looker, ex: &str, default: &str) -> Option<(u8, MainHit)> {
    status::tmux_rows()
        .into_iter()
        .filter(|r| r["@cbar_role"] == "main" && !look.hide.contains(&r["pane_id"]))
        .filter_map(|r| {
            let score = fit_score(&r["@cbar_exchange"], &r["session_name"], ex, default)?;
            Some((score, (r["session_id"].clone(), r["session_name"].clone(), r["window_id"].clone(), r["pane_id"].clone())))
        })
        .min_by_key(|(s, _)| *s)
}

/// Whether Rex block BLOCK of session SID runs a main switchboard; the
/// exchange it shows from its argv ("" default, "*" all).
fn rex_main_board(look: &Looker, sid: &str, block: &serde_json::Value) -> Option<String> {
    if block["label"] != "switchboard" || block["placement"].as_str().unwrap_or("placed") != "placed" {
        return None;
    }
    let bid = block["block_id"].as_str()?;
    if look.hide.contains(bid) {
        return None;
    }
    let proc = rex_json(&["block", "call", "-s", sid, "-b", bid, "process"])?;
    let pid = proc["foreground"]["pid"].as_i64()?;
    status::board_kind(&status::argv_of(pid))?
}

fn rex_find_main(look: &Looker, ex: &str, default: &str) -> Option<(u8, MainHit)> {
    let mut sessions = mux::rex_sessions();
    sessions.sort_by_key(|(_, l)| l != ex);
    let mut best: Option<(u8, MainHit)> = None;
    for (sid, label) in sessions {
        let blocks = rex_json(&["block", "ls", "-s", &sid, "--json"]).and_then(|v| v["blocks"].as_array().cloned()).unwrap_or_default();
        for b in &blocks {
            if let Some(shown) = rex_main_board(look, &sid, b) {
                if let Some(score) = fit_score(&shown, &label, ex, default) {
                    let hit = (sid.clone(), label.clone(), b["window_id"].as_str().unwrap_or("").to_string(), b["block_id"].as_str().unwrap_or("").to_string());
                    if score == 0 {
                        return Some((0, hit));
                    }
                    if best.as_ref().is_none_or(|(s, _)| score < *s) {
                        best = Some((score, hit));
                    }
                }
            }
        }
    }
    best
}

/// The main switchboard for exchange EX, the terminal this pane runs in first.
pub fn find_main(look: &Looker, ex: &str, default: &str) -> Option<MainHit> {
    let in_tmux = util::env("TMUX_PANE").is_some();
    let a = if in_tmux { tmux_find_main(look, ex, default) } else { rex_find_main(look, ex, default) };
    if let Some((0, hit)) = a {
        return Some(hit);
    }
    let b = if in_tmux { rex_find_main(look, ex, default) } else { tmux_find_main(look, ex, default) };
    [a, b].into_iter().flatten().min_by_key(|(s, _)| *s).map(|(_, h)| h)
}

/// The window PANE is in if it still runs a main switchboard, else None.
fn main_window(look: &Looker, sid: &str, pane: &str) -> Option<String> {
    if mux::is_tmux_id(pane) {
        let (code, out, _) = mux::tmux_s(&["display-message", "-p", "-t", pane, "#{@cbar_role}\t#{window_id}"]);
        let (role, wid) = out.trim().split_once('\t')?;
        return (code == 0 && role == "main" && !look.hide.contains(pane)).then(|| wid.to_string());
    }
    let blocks = rex_json(&["block", "ls", "-s", sid, "--json"])?;
    let b = blocks["blocks"].as_array()?.iter().find(|b| b["block_id"] == pane)?;
    rex_main_board(look, sid, b).map(|_| b["window_id"].as_str().unwrap_or("").to_string())
}

// ---- the live board --------------------------------------------------------------

struct Shared {
    data: Snapshot,
    client: Option<String>,
    main: Option<MainHit>,
    status: Option<(Color, String, Instant)>,
}

struct Board {
    view: View,
    /// For a main board: the exchange it toggles back to from all.
    exchange: String,
    default: String,
    exchanges: Vec<String>,
    shared: Arc<Mutex<Shared>>,
    look: Arc<Looker>,
    selected: Option<String>,
    targets: Vec<Option<Target>>,
    offset: usize,
    last_index: usize,
    focus_in: Option<Instant>,
    link_hover: bool,
    link_at: Option<Instant>,
    cols: usize,
}

impl Board {
    fn scope(&self) -> Option<&str> {
        match &self.view {
            View::Scoped(sid, _) => Some(sid),
            _ => None,
        }
    }

    fn say(&self, c: Color, msg: impl Into<String>) {
        self.shared.lock().unwrap().status = Some((c, msg.into(), Instant::now()));
    }

    /// Selectable panes in screen order, as (pane, group): the group is the
    /// session id, or the tab (window id) when scoped to one session.
    fn order(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for t in self.targets.iter().flatten() {
            if let Target::Pane(sid, wid, bid) = t {
                if !out.iter().any(|(b, _)| b == bid) {
                    out.push((bid.clone(), if self.scope().is_some() { wid.clone() } else { sid.clone() }));
                }
            }
        }
        out
    }

    fn groups(&self, data: &Snapshot) -> Vec<Target> {
        data.iter()
            .map(|(g, _)| match &g.window_id {
                Some(w) if self.scope().is_some() => Target::Tab(g.session_id.clone(), w.clone()),
                _ => Target::Session(g.session_id.clone()),
            })
            .collect()
    }

    fn keep_selection(&mut self) {
        let ids: Vec<String> = self.order().into_iter().map(|(b, _)| b).collect();
        if ids.is_empty() {
            return;
        }
        if !self.selected.as_ref().is_some_and(|s| ids.contains(s)) {
            // fall back to where it was, by screen position
            self.selected = Some(ids[self.last_index.min(ids.len() - 1)].clone());
        }
        self.last_index = ids.iter().position(|b| Some(b) == self.selected.as_ref()).unwrap_or(0);
    }

    fn step(&mut self, step: isize) {
        let ids: Vec<String> = self.order().into_iter().map(|(b, _)| b).collect();
        if ids.is_empty() {
            return;
        }
        let i = self.selected.as_ref().and_then(|s| ids.iter().position(|b| b == s)).unwrap_or(0) as isize;
        self.selected = Some(ids[(i + step).clamp(0, ids.len() as isize - 1) as usize].clone());
    }

    fn edge(&mut self, last: bool) {
        let ids: Vec<String> = self.order().into_iter().map(|(b, _)| b).collect();
        if !ids.is_empty() {
            self.selected = Some(if last { ids[ids.len() - 1].clone() } else { ids[0].clone() });
        }
    }

    /// Select the first pane of the next/previous session that has panes.
    fn hop(&mut self, step: isize) {
        let order = self.order();
        if order.is_empty() {
            return;
        }
        let current = order.iter().find(|(b, _)| Some(b) == self.selected.as_ref()).map(|(_, g)| g.clone()).unwrap_or(order[0].1.clone());
        let mut firsts: Vec<(String, String)> = Vec::new();
        for (b, g) in &order {
            if firsts.last().is_none_or(|(_, last)| last != g) {
                firsts.push((b.clone(), g.clone()));
            }
        }
        let i = firsts.iter().position(|(_, g)| *g == current).unwrap_or(0) as isize;
        let n = firsts.len() as isize;
        self.selected = Some(firsts[((i + step) % n + n) as usize % n as usize].0.clone());
    }

    fn first_pane_of(&self, group: &str) -> Option<String> {
        self.order().into_iter().find(|(_, g)| g == group).map(|(b, _)| b)
    }

    fn names(&self, sid: &str, wid: Option<&str>, bid: Option<&str>) -> (String, String) {
        let sh = self.shared.lock().unwrap();
        if let View::Scoped(_, label) = &self.view {
            let mut tab = sh.data.iter().find(|(g, _)| g.window_id.as_deref() == wid).map(|(g, _)| g.label.clone()).unwrap_or_default();
            if let Some(p) = sh.data.iter().flat_map(|(_, ps)| ps).find(|p| Some(p.block_id.as_str()) == bid) {
                let pn = if p.label.is_empty() || p.label == "shell" { p.program.clone() } else { p.label.clone() };
                tab = [tab, pn].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" › ");
            }
            return (label.clone(), tab);
        }
        for (g, panes) in &sh.data {
            if g.session_id == sid {
                let tab = panes
                    .iter()
                    .find(|p| Some(p.block_id.as_str()) == bid)
                    .map(|p| if p.tab.is_empty() { p.program.clone() } else { p.tab.clone() })
                    .unwrap_or_default();
                return (title(&g.label, &self.view), tab);
            }
        }
        (sid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect(), String::new())
    }

    fn jump(&self, sid: String, wid: Option<String>, bid: Option<String>) {
        let (label, tab) = self.names(&sid, wid.as_deref(), bid.as_deref());
        let place = if tab.is_empty() { label } else { format!("{label} › {tab}") };
        self.say(MUTED, format!("→ {place}"));
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            jump_to(&shared, &sid, wid.as_deref(), bid.as_deref(), &place);
        });
    }

    fn jump_main(&mut self) {
        self.link_at = Some(Instant::now());
        let ex = self.exchange.clone();
        self.say(MUTED, format!("→ {ex}"));
        let (shared, look, default) = (self.shared.clone(), self.look.clone(), self.default.clone());
        std::thread::spawn(move || {
            let cached = shared.lock().unwrap().main.clone();
            // still there, still the main switchboard? it may have moved tabs
            let cached = cached.and_then(|(sid, l, _, bid)| main_window(&look, &sid, &bid).map(|w| (sid, l, w, bid)));
            let was_cached = cached.is_some();
            let mut target = cached.or_else(|| find_main(&look, &ex, &default));
            shared.lock().unwrap().main = target.clone();
            for attempt in 0..2 {
                let Some((sid, label, wid, bid)) = target.clone() else {
                    shared.lock().unwrap().status = Some((BLOCKED, format!("✗ no {ex} switchboard"), Instant::now()));
                    return;
                };
                if jump_to(&shared, &sid, Some(&wid), Some(&bid), &label) || attempt == 1 || !was_cached {
                    return;
                }
                // the cached pane may have just gone; look again once
                target = find_main(&look, &ex, &default);
                shared.lock().unwrap().main = target.clone();
            }
        });
    }

    fn link_hot(&self) -> bool {
        self.link_hover || self.link_at.is_some_and(|t| t.elapsed() < Duration::from_millis(600))
    }

    /// Whether screen column X (0-based) on screen line LINE is on the main link.
    fn on_link(&self, x: usize, line: usize) -> bool {
        if self.scope().is_none() || self.offset + line != 0 {
            return false;
        }
        let (start, end) = link_span(self.cols, &format!("↖ {}", self.exchange));
        // each screen line is drawn after one space
        x >= 1 && start < x && x - 1 < end
    }

    fn activate(&mut self, t: Target) {
        match t {
            Target::Pane(sid, wid, bid) => {
                self.selected = Some(bid.clone());
                self.jump(sid, Some(wid), Some(bid));
            }
            Target::Tab(sid, wid) => {
                self.selected = self.first_pane_of(&wid).or(self.selected.clone());
                self.jump(sid, Some(wid), None);
            }
            Target::Session(sid) => {
                self.selected = self.first_pane_of(&sid).or(self.selected.clone());
                self.jump(sid, None, None);
            }
        }
    }

    fn just_focused(&self) -> bool {
        self.focus_in.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    fn click(&mut self, x: usize, line: usize) {
        if self.on_link(x, line) {
            if !self.just_focused() {
                self.jump_main();
            }
            return;
        }
        let Some(Some(t)) = self.targets.get(self.offset + line).cloned() else { return };
        if self.just_focused() {
            // this click most likely just focused the pane: select, don't jump
            if let Target::Pane(_, _, b) = t {
                self.selected = Some(b);
            }
            return;
        }
        self.activate(t);
    }

    /// Handle a key; false to quit.
    fn key(&mut self, code: KeyCode, mods: KeyModifiers, data: &Snapshot) -> bool {
        match code {
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::Char('J') | KeyCode::Tab | KeyCode::Right | KeyCode::PageDown => self.hop(1),
            KeyCode::Char('K') | KeyCode::BackTab | KeyCode::Left | KeyCode::PageUp => self.hop(-1),
            KeyCode::Char('g') | KeyCode::Home => self.edge(false),
            KeyCode::Char('G') | KeyCode::End => self.edge(true),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char(' ') => {
                let sel = self.selected.clone();
                if let Some(p) = data.iter().flat_map(|(_, ps)| ps).find(|p| Some(&p.block_id) == sel.as_ref()) {
                    if self.order().iter().any(|(b, _)| *b == p.block_id) {
                        self.jump(p.session_id.clone(), Some(p.window_id.clone()), Some(p.block_id.clone()));
                    }
                }
            }
            KeyCode::Char('0') | KeyCode::Char('~') => {
                if self.scope().is_some() {
                    self.jump_main();
                }
            }
            KeyCode::Char('a') => match &self.view {
                View::Exchange(_) => self.view = View::All,
                View::All => self.view = View::Exchange(self.exchange.clone()),
                View::Scoped(..) => {}
            },
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap() as usize;
                if let Some(t) = self.groups(data).get(n.wrapping_sub(1)).cloned() {
                    self.activate(t);
                }
            }
            _ => {}
        }
        true
    }
}

/// Show the session (and tab) and focus the pane; true on success.
fn jump_to(shared: &Arc<Mutex<Shared>>, sid: &str, wid: Option<&str>, bid: Option<&str>, place: &str) -> bool {
    let set = |c: Color, m: String| shared.lock().unwrap().status = Some((c, m, Instant::now()));
    let err = if mux::is_tmux_id(sid) {
        mux::tmux_jump(sid, wid, bid)
    } else {
        let client = shared.lock().unwrap().client.clone();
        mux::rex_jump(client, sid, wid, bid)
    };
    match err {
        None => {
            set(DONE, format!("✓ {place}"));
            true
        }
        Some(e) if e.starts_with('?') => {
            set(ERROR, format!("? {place}: {}", &e[1..]));
            false
        }
        Some(e) => {
            set(BLOCKED, format!("✗ {e}"));
            false
        }
    }
}

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_quit(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

fn live(mut board: Board) {
    {
        let (shared, look) = (board.shared.clone(), board.look.clone());
        let scope = board.scope().map(String::from);
        let (ex, default) = (board.exchange.clone(), board.default.clone());
        std::thread::spawn(move || {
            let mut looked: Option<Instant> = None;
            loop {
                let fresh = look.snapshot(scope.as_deref());
                let client = mux::app_client();
                {
                    let mut sh = shared.lock().unwrap();
                    sh.data = fresh;
                    sh.client = client;
                }
                if scope.is_some() && looked.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                    looked = Some(Instant::now());
                    let hit = find_main(&look, &ex, &default);
                    shared.lock().unwrap().main = hit;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_quit as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_quit as libc::sighandler_t);
    }
    let tty = unsafe { libc::isatty(0) } == 1;
    let mut out = std::io::stdout();
    if tty {
        let _ = terminal::enable_raw_mode();
    }
    let _ = execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange);
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => util::die(format!("no terminal: {e}")),
    };
    let _ = term.hide_cursor();
    let mut tick = 0usize;
    let mut loaded = false;
    while !QUIT.load(Ordering::Relaxed) {
        let (w, h) = terminal_size();
        board.cols = (w as usize).saturating_sub(2);
        let rows = (h as usize).saturating_sub(1).max(1);
        let (data, status) = {
            let mut sh = board.shared.lock().unwrap();
            if sh.status.as_ref().is_some_and(|(_, _, t)| t.elapsed() > Duration::from_secs(3)) {
                sh.status = None;
            }
            (sh.data.clone(), sh.status.as_ref().map(|(c, m, _)| (*c, m.clone())))
        };
        loaded = loaded || !data.is_empty() || tick > 40;
        let shown = pick(&data, &board.view, &board.exchanges);
        let link = board.exchange.clone();
        let frame = if loaded || !shown.is_empty() {
            let hot = board.link_hot();
            let f = render(&shown, &board.view, board.cols, tick, board.selected.as_deref(), &link, hot);
            board.targets = f.targets;
            board.keep_selection();
            render(&shown, &board.view, board.cols, tick, board.selected.as_deref(), &link, hot)
        } else {
            Frame { lines: vec![Line::from(s("loading…", MUTED))], targets: vec![None] }
        };
        board.targets = frame.targets.clone();
        // keep the selected pane (and its detail line) on screen
        let hit: Vec<usize> = frame
            .targets
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Some(Target::Pane(_, _, b)) if Some(b) == board.selected.as_ref()))
            .map(|(i, _)| i)
            .collect();
        if let (Some(&a), Some(&z)) = (hit.first(), hit.last()) {
            if a < board.offset {
                board.offset = a.saturating_sub(1); // include the card header
            } else if z >= board.offset + rows {
                board.offset = z + 2 - rows;
            }
        }
        board.offset = board.offset.min(frame.lines.len().saturating_sub(rows));
        let view: Vec<Line> = frame.lines[board.offset..frame.lines.len().min(board.offset + rows)].to_vec();
        let foot = footer(board.cols, status.as_ref(), &board.view, &link);
        let _ = term.draw(|f| {
            let area = f.area();
            let body = Rect { x: area.x + 1, y: area.y, width: area.width.saturating_sub(1), height: area.height.saturating_sub(1) };
            f.render_widget(Paragraph::new(view), body);
            let foot_area = Rect { x: area.x + 1, y: area.y + area.height.saturating_sub(1), width: area.width.saturating_sub(1), height: 1 };
            f.render_widget(Paragraph::new(foot), foot_area);
        });
        tick += 1;
        if !tty {
            std::thread::sleep(Duration::from_millis(120));
            continue;
        }
        if !event::poll(Duration::from_millis(120)).unwrap_or(false) {
            continue;
        }
        let mut going = true;
        while going && event::poll(Duration::ZERO).unwrap_or(false) {
            let Ok(ev) = event::read() else { break };
            match ev {
                Event::Key(k) if k.kind != KeyEventKind::Release => going = board.key(k.code, k.modifiers, &shown),
                Event::Mouse(m) => {
                    let (x, y) = (m.column as usize, m.row as usize);
                    match m.kind {
                        MouseEventKind::Moved => board.link_hover = board.on_link(x, y),
                        MouseEventKind::ScrollUp => board.step(-1),
                        MouseEventKind::ScrollDown => board.step(1),
                        MouseEventKind::Down(MouseButton::Left) => board.click(x, y),
                        _ => {}
                    }
                }
                Event::FocusGained => board.focus_in = Some(Instant::now()),
                Event::FocusLost => board.link_hover = false,
                _ => {}
            }
        }
        if !going {
            break;
        }
    }
    let _ = execute!(term.backend_mut(), DisableFocusChange, DisableMouseCapture, LeaveAlternateScreen);
    let _ = term.show_cursor();
    if tty {
        let _ = terminal::disable_raw_mode();
    }
    let _ = std::io::stdout().flush();
}

// ---- main ------------------------------------------------------------------

fn usage() -> ! {
    util::die("usage: switchboard [--exchange NAME|--all|--here|--session NAME] [--once|--json|--watch]")
}

pub fn main(args: Vec<String>) {
    let home = Home::from_env();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| -> Option<String> {
        let i = args.iter().position(|a| a == f)?;
        Some(args.get(i + 1).cloned().unwrap_or_else(|| usage()))
    };
    for a in &args {
        if a.starts_with('-') && !["--exchange", "--all", "--here", "--session", "--once", "--json", "--watch", "-h", "--help"].contains(&a.as_str()) {
            usage();
        }
    }
    if has("-h") || has("--help") {
        println!("{}", HELP.trim());
        return;
    }
    let default = home.default_exchange();
    let mut scope: Option<Group> = None;
    if has("--here") {
        let here = util::env("TMUX_PANE")
            .and_then(|p| {
                let (code, out, _) = mux::tmux_s(&["display-message", "-p", "-t", &p, "#{session_id}"]);
                (code == 0 && !out.trim().is_empty()).then(|| out.trim().to_string())
            })
            .or_else(|| util::env("REX_SESSION"))
            .unwrap_or_else(|| util::die("--here needs a tmux or Rex pane"));
        scope = Some(status::resolve(&here).unwrap_or_else(|| util::die(format!("no session '{here}'"))));
    } else if let Some(name) = value("--session") {
        scope = Some(status::resolve(&name).unwrap_or_else(|| util::die(format!("no session '{name}'"))));
    }
    // the exchange: named, else the one this pane's session belongs to, else the default
    let here_ex = || {
        mux::here_session()
            .map(|s| s.split(SEP).next().unwrap_or("").to_string())
            .filter(|e| home.exchange_exists(e))
            .unwrap_or_else(|| default.clone())
    };
    let exchange = match (&scope, value("--exchange")) {
        (_, Some(e)) => e,
        (Some(g), None) => g.label.split(SEP).next().filter(|e| g.label.contains(SEP) && home.exchange_exists(e)).map(String::from).unwrap_or_else(here_ex),
        (None, None) => here_ex(),
    };
    let view = match &scope {
        Some(g) => {
            let label = if g.label.is_empty() { g.session_id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect() } else { title(&g.label, &View::Exchange(exchange.clone())) };
            View::Scoped(g.session_id.clone(), label)
        }
        // a 0.4 layout has no exchanges yet: show everything
        None if has("--all") || home.needs_migrate() => View::All,
        None => View::Exchange(exchange.clone()),
    };
    let exchanges: Vec<String> = home.exchanges().into_iter().map(|x| x.name).collect();
    let scope_id = scope.as_ref().map(|g| g.session_id.clone());

    if has("--watch") {
        let Some(g) = &scope else { util::die("--watch needs --here or --session NAME") };
        crate::watch::watch(&home, &g.session_id, &g.label);
        return;
    }
    if has("--json") {
        let look = Looker::new(false);
        let data = look.snapshot(scope_id.as_deref());
        let data = if value("--exchange").is_some() || has("--all") { pick(&data, &view, &exchanges) } else { data };
        let out: Vec<serde_json::Value> = data.iter().map(|(g, ps)| serde_json::json!({"group": g, "panes": ps})).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }
    let look = Looker::new(true);
    if has("--once") {
        let data = pick(&look.snapshot(scope_id.as_deref()), &view, &exchanges);
        let cols = terminal::size().ok().map(|(w, _)| w as usize).or_else(|| util::env("COLUMNS").and_then(|c| c.parse().ok())).unwrap_or(80);
        print!("{}", ansi(&render(&data, &view, cols, 0, None, &exchange, false).lines));
        return;
    }
    // in tmux, mark this pane so others can find the main switchboard
    let pane = util::env("TMUX_PANE");
    let marked = pane.as_ref().is_some_and(|p| {
        let role = if scope.is_some() { "local" } else { "main" };
        let shown = match &view {
            View::All => "*".to_string(),
            _ => exchange.clone(),
        };
        mux::tmux_s(&["set-option", "-p", "-t", p, "@cbar_role", role, ";", "set-option", "-p", "-t", p, "@cbar_exchange", &shown]).0 == 0
    });
    let board = Board {
        view,
        exchange,
        default,
        exchanges,
        shared: Arc::new(Mutex::new(Shared { data: Vec::new(), client: None, main: None, status: None })),
        look: Arc::new(look),
        selected: None,
        targets: Vec::new(),
        offset: 0,
        last_index: 0,
        focus_in: None,
        link_hover: false,
        link_at: None,
        cols: 80,
    };
    live(board);
    if marked {
        let p = pane.unwrap();
        mux::tmux_s(&["set-option", "-p", "-u", "-t", &p, "@cbar_role", ";", "set-option", "-p", "-u", "-t", &p, "@cbar_exchange"]);
    }
}

const HELP: &str = "
usage: switchboard [--exchange NAME|--all|--here|--session NAME] [--once|--json|--watch]

  switchboard                   the main board: this exchange's sessions
  switchboard --exchange NAME   the main board of exchange NAME
  switchboard --all             every session, by exchange
  switchboard --session NAME    live view of one session: a card per tab
  switchboard --here            same, for this pane's session
  --once                        print once and exit
  --json                        print the panes as JSON and exit
  --watch --here                a line as each report turns done, blocked or error,
                                stops or closes

Keys:  j/k ↑/↓ select · J/K tab/shift-tab next/prev session · g/G first/last
       enter jump · 1-9 session N · a this exchange / all · 0 main board · q quit
Mouse: click a row or a card header to jump, wheel to move, ↖ to the main board
";

#[cfg(test)]
mod tests {
    use super::*;

    fn g(label: &str) -> (Group, Vec<Pane>) {
        (Group { session_id: label.into(), label: label.into(), mux: "rex".into(), window_id: None }, Vec::new())
    }

    #[test]
    fn picks_an_exchanges_sessions() {
        let data = vec![g("main/stats"), g("other"), g("main"), g("work/stats"), g("work")];
        let ex: Vec<String> = vec!["main".into(), "work".into()];
        let labels = |v: &View| pick(&data, v, &ex).into_iter().map(|(g, _)| g.label).collect::<Vec<_>>();
        assert_eq!(labels(&View::Exchange("main".into())), ["main", "main/stats"]);
        assert_eq!(labels(&View::All), ["main", "main/stats", "work", "work/stats", "other"]);
        assert_eq!(title("main/stats", &View::Exchange("main".into())), "stats");
        assert_eq!(title("main/stats", &View::All), "main/stats");
    }

    #[test]
    fn scores_main_boards() {
        assert_eq!(fit_score("main", "main", "main", "main"), Some(0));
        assert_eq!(fit_score("", "central", "main", "main"), Some(1));
        assert_eq!(fit_score("*", "x", "main", "main"), Some(2));
        assert_eq!(fit_score("work", "work", "main", "main"), None);
        assert_eq!(fit_score("", "work", "work", "main"), Some(0));
    }

    #[test]
    fn renders_a_card() {
        let p = Pane {
            session_id: "s".into(),
            block_id: "b".into(),
            window_id: "w".into(),
            tab: "fix-x".into(),
            label: "shell".into(),
            program: "claude".into(),
            cwd: "~/x".into(),
            state: "done".into(),
            kind: "".into(),
            msg: "all good".into(),
            since: "2m".into(),
        };
        let data = vec![(Group { session_id: "s".into(), label: "main/stats".into(), mux: "tmux".into(), window_id: None }, vec![p])];
        let f = render(&data, &View::Exchange("main".into()), 60, 0, None, "main", false);
        let text: Vec<String> = f.lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert!(text[0].starts_with("◇ switchboard main · agents & panes by session"));
        assert_eq!(text[1], "✓ 1 done");
        assert!(text[3].starts_with("╭─ 1 stats tmux ─"), "{}", text[3]);
        assert!(text[4].contains("✓ fix-x            done") && text[4].ends_with("2m │"), "{}", text[4]);
        assert!(text[5].contains("all good"));
        assert!(text.iter().all(|l| l.chars().count() == 60 || l.is_empty() || !l.starts_with(['╭', '│', '╰'])));
        assert_eq!(f.targets[4], Some(Target::Pane("s".into(), "w".into(), "b".into())));
    }
}
