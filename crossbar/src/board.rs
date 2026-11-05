//! switchboard: a live overview of the sessions of an exchange (or of every
//! exchange), or of one session's tabs, and a way to jump to any pane. An
//! exchange's switchboard runs next to its supervisor, a project's next to
//! its operator; agents have none.
//!
//! Each session is a card; each pane a row with its tab name, program status
//! (idle / working / done / blocked / error) and how long it has been in that
//! state. Rex reports status via OSC 7501; in tmux it comes from claude-relay
//! or `claude agents` (see status.rs).
//!
//!   switchboard                   the exchange switchboard: this exchange's sessions
//!                                 (the one this pane's session belongs to,
//!                                 else the default exchange)
//!   switchboard --exchange NAME   the switchboard of exchange NAME
//!   switchboard --all             every session, by exchange
//!   switchboard --session NAME    live view of one session: a card per tab (a
//!                                 project's session: its project switchboard)
//!   switchboard --here            same, for this pane's session ($TMUX_PANE or $REX_SESSION)
//!   switchboard --once            print once and exit (combines with the above)
//!   switchboard --json            print the panes as JSON and exit (combines with the above)
//!   switchboard --watch --here    print a line as each report turns done, blocked or error,
//!                                 stops or closes (reports: this session's agents; in an
//!                                 exchange's session, also each of its projects' operators)
//!   switchboard --watch --next --here   the same until the first event (and any others
//!                                 within a second), then exit 0
//!
//! $CROSSBAR_TMUX_SOCKET names a tmux server other than the default (tmux -L).
//!
//! Keys:  j/k ↑/↓ select · J/K tab/shift-tab next/prev session · g/G first/last
//!        enter jump to pane · 1-9 jump to session N · a this exchange / all
//!        exchanges · q quit (scoped to one session, "session" above means
//!        tab; 0/~ jumps to the exchange switchboard)
//! Mouse: click a row to jump to that pane, click a card header to jump to that
//!        session, wheel moves the selection; scoped, click "↖ <exchange>" in
//!        the header to jump to the exchange switchboard.
//!
//! Jumping asks the Rex app (a client) to show the session and tab with the
//! client action session.select, then focuses the pane. In tmux it selects
//! the window and pane and switches this client (or, outside tmux, the most
//! recently active one) to the session.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::{cursor, execute};
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
/// How long each spinner frame shows.
const SPIN_STEP: Duration = Duration::from_millis(120);

/// The spinner frame for time SINCE the board started: by the clock, not by
/// redraws, so bursts of input or resizes don't make it spin faster.
pub fn spin_tick(since: Duration) -> usize {
    (since.as_millis() / SPIN_STEP.as_millis()) as usize
}

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

/// Which sessions a switchboard shows, and how it names them: an
/// exchange's, every exchange's, or one session's tabs (a project
/// switchboard shows its project's session).
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Exchange(String),
    All,
    /// One session's tabs: its id, its label as shown, and whose session it
    /// is.
    Scoped(String, String, Whose),
}

/// Whose session a scoped switchboard shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Whose {
    /// A project's: this is its project switchboard.
    Project,
    /// An exchange's own (the supervisor's).
    Exchange,
    /// A session crossbar didn't make.
    Other,
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

/// The session an exchange switchboard numbers 0: its exchange's own (the
/// supervisor's).
fn zero_label<'a>(view: &'a View, exchange: &'a str) -> Option<&'a str> {
    match view {
        View::Exchange(e) => Some(e),
        View::All => Some(exchange),
        View::Scoped(..) => None,
    }
}

/// (number, card) in card order: on an exchange switchboard the exchange's session
/// comes first as 0 and the rest count from 1.
pub fn numbered<'a>(data: &'a Snapshot, view: &View, exchange: &str) -> Vec<(usize, &'a (Group, Vec<Pane>))> {
    let zero = zero_label(view, exchange);
    let mut out: Vec<(usize, &(Group, Vec<Pane>))> = data.iter().filter(|(g, _)| Some(g.label.as_str()) == zero).map(|c| (0, c)).collect();
    let rest = data.iter().filter(|(g, _)| Some(g.label.as_str()) != zero);
    out.extend(rest.enumerate().map(|(n, c)| (n + 1, c)));
    out
}

/// A switchboard pane: left off the cards.
fn is_board(p: &Pane) -> bool {
    p.state.is_empty() && (p.program == "switchboard" || p.label == "switchboard")
}

/// Lines for one card (a session, or scoped a tab), with a target per line.
/// Switchboards are left off.
fn card(number: usize, group: &Group, name: &str, panes: &[Pane], cols: usize, tick: usize, selected: Option<&str>, scoped: bool) -> Lines {
    let mut panes: Vec<Pane> = panes.iter().filter(|p| !is_board(p)).cloned().collect();
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

    // ╭─ name tmux ~/path ──────── 2 ─╮
    let num = if number <= 9 { format!(" {number} ") } else { String::new() };
    let tag = if group.mux == "tmux" && !scoped { " tmux" } else { "" };
    let title_len = format!(" {name}{tag} ").chars().count();
    let path = fit(&home, inner.saturating_sub(title_len + num.chars().count() + 4));
    let path_part = if path.is_empty() { String::new() } else { format!("{path} ") };
    let rule = "─".repeat((cols as isize - 4 - title_len as isize - path_part.chars().count() as isize - num.chars().count() as isize).max(1) as usize);
    let mut lines = vec![Line::from(vec![
        s("╭─", edge),
        Span::raw(" "),
        Span::styled(name.to_string(), Style::default().fg(FG).add_modifier(Modifier::BOLD)),
        s(tag, DIM),
        Span::raw(" "),
        s(path_part, MUTED),
        s(rule, edge),
        s(num, MUTED),
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
        let mark = if is_agent { glyph(&p.state, tick) } else { "▪".into() };
        let word = if is_agent { p.state.clone() } else { p.program.clone() };
        let right = if is_agent { p.since.clone() } else { "process".into() };
        let name_color = if is_agent || hot { FG } else { MUTED };
        let mark_color = if is_agent { c } else { FAINT };
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

/// Columns [start, end) of the header's link to the exchange switchboard, in
/// line coordinates.
fn link_span(cols: usize, link: &str) -> (usize, usize) {
    let cols = cols.clamp(36, 100);
    let end = cols - util::clock().chars().count() - 2;
    (end.saturating_sub(link.chars().count()), end)
}

pub struct Frame {
    pub lines: Vec<Line<'static>>,
    pub targets: Vec<Option<Target>>,
}

/// The whole board for DATA (already picked for VIEW). LINK names the exchange
/// whose switchboard a scoped board links to; LINK_HOT colors it.
pub fn render(data: &Snapshot, view: &View, cols: usize, tick: usize, selected: Option<&str>, link: &str, link_hot: bool) -> Frame {
    let cols = cols.clamp(36, 100);
    let scoped = matches!(view, View::Scoped(..));
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut others = 0;
    for (_, panes) in data {
        for p in panes {
            if is_board(p) {
                continue;
            }
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
    // say which switchboard this is: an exchange's or a project's
    let mut subtitle = match view {
        View::Scoped(_, label, Whose::Project) => format!("project {label} · tabs & agents"),
        View::Scoped(_, label, Whose::Exchange) => format!("exchange {label} · tabs & agents"),
        View::Scoped(_, label, Whose::Other) => format!("{label} · tabs & agents"),
        View::Exchange(e) => format!("exchange {e} · projects & agents"),
        View::All => "all exchanges · projects & agents".into(),
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
    for (n, (g, panes)) in numbered(data, view, link) {
        let (l, t) = card(n, g, &title(&g.label, view), panes, cols, tick, selected, scoped);
        lines.extend(l);
        targets.extend(t);
    }
    Frame { lines, targets }
}

/// The hint line; ZERO says the board has a card 0 (the exchange's session).
fn footer(cols: usize, status: Option<&(Color, String)>, view: &View, link: &str, zero: bool) -> Line<'static> {
    let scoped = matches!(view, View::Scoped(..));
    let group = if scoped { "tab" } else { "session" };
    let mut keys: Vec<(&str, String)> =
        vec![("↑↓", "select".into()), ("⇥", group.into()), ("⏎", "jump".into()), ("1-9", group.into()), ("click", "jump".into()), ("q", "quit".into())];
    if scoped || zero {
        keys.insert(keys.len() - 1, ("0", link.to_string()));
    }
    match view {
        View::Scoped(..) => {}
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

// ---- finding the exchange switchboard ---------------------------------------------

/// An exchange switchboard's pane: (session id, session label, window id, pane id).
pub type ExchangeBoard = (String, String, String, String);

/// How well an exchange switchboard showing BOARD_EX (from its argv: "" for the
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

/// The @cbar_role a board sets on its tmux pane: "main" for an exchange
/// switchboard (the pane others jump to), "local" for one session's. The
/// values are older than these names and stay, so that boards of either
/// version find each other.
const ROLE_EXCHANGE: &str = "main";
const ROLE_SESSION: &str = "local";

fn tmux_find_exchange_board(look: &Looker, ex: &str, default: &str) -> Option<(u8, ExchangeBoard)> {
    status::tmux_rows()
        .into_iter()
        .filter(|r| r["@cbar_role"] == ROLE_EXCHANGE && !look.hide.contains(&r["pane_id"]))
        .filter_map(|r| {
            let score = fit_score(&r["@cbar_exchange"], &r["session_name"], ex, default)?;
            Some((score, (r["session_id"].clone(), r["session_name"].clone(), r["window_id"].clone(), r["pane_id"].clone())))
        })
        .min_by_key(|(s, _)| *s)
}

/// Whether Rex block BLOCK of session SID runs an exchange switchboard; the
/// exchange it shows from its argv ("" default, "*" all).
fn rex_exchange_board(look: &Looker, sid: &str, block: &serde_json::Value) -> Option<String> {
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

fn rex_find_exchange_board(look: &Looker, ex: &str, default: &str) -> Option<(u8, ExchangeBoard)> {
    let mut sessions = mux::rex_sessions();
    sessions.sort_by_key(|(_, l)| l != ex);
    let mut best: Option<(u8, ExchangeBoard)> = None;
    for (sid, label) in sessions {
        let blocks = rex_json(&["block", "ls", "-s", &sid, "--json"]).and_then(|v| v["blocks"].as_array().cloned()).unwrap_or_default();
        for b in &blocks {
            if let Some(shown) = rex_exchange_board(look, &sid, b) {
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

/// The switchboard of exchange EX, the terminal this pane runs in first.
pub fn find_exchange_board(look: &Looker, ex: &str, default: &str) -> Option<ExchangeBoard> {
    let in_tmux = util::env("TMUX_PANE").is_some();
    let a = if in_tmux { tmux_find_exchange_board(look, ex, default) } else { rex_find_exchange_board(look, ex, default) };
    if let Some((0, hit)) = a {
        return Some(hit);
    }
    let b = if in_tmux { rex_find_exchange_board(look, ex, default) } else { tmux_find_exchange_board(look, ex, default) };
    [a, b].into_iter().flatten().min_by_key(|(s, _)| *s).map(|(_, h)| h)
}

/// The window PANE is in if it still runs an exchange switchboard, else None.
fn exchange_board_window(look: &Looker, sid: &str, pane: &str) -> Option<String> {
    if mux::is_tmux_id(pane) {
        let (code, out, _) = mux::tmux_s(&["display-message", "-p", "-t", pane, "#{@cbar_role}\t#{window_id}"]);
        let (role, wid) = out.trim().split_once('\t')?;
        return (code == 0 && role == ROLE_EXCHANGE && !look.hide.contains(pane)).then(|| wid.to_string());
    }
    let blocks = rex_json(&["block", "ls", "-s", sid, "--json"])?;
    let b = blocks["blocks"].as_array()?.iter().find(|b| b["block_id"] == pane)?;
    rex_exchange_board(look, sid, b).map(|_| b["window_id"].as_str().unwrap_or("").to_string())
}

// ---- the live board --------------------------------------------------------------

struct Shared {
    data: Snapshot,
    client: Option<String>,
    /// The exchange switchboard a scoped board's link jumps to, once found.
    exchange_board: Option<ExchangeBoard>,
    status: Option<(Color, String, Instant)>,
}

struct Board {
    view: View,
    /// The exchange: an exchange switchboard toggles back to it from all; a
    /// scoped one's link and 0 jump to its switchboard.
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
            View::Scoped(sid, ..) => Some(sid),
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

    /// Number-key targets: card number → what it opens.
    fn groups(&self, data: &Snapshot) -> HashMap<usize, Target> {
        numbered(data, &self.view, &self.exchange)
            .into_iter()
            .map(|(n, (g, _))| {
                let t = match &g.window_id {
                    Some(w) if self.scope().is_some() => Target::Tab(g.session_id.clone(), w.clone()),
                    _ => Target::Session(g.session_id.clone()),
                };
                (n, t)
            })
            .collect()
    }

    /// The tab a session card opens: the operator's (the supervisor's for an
    /// exchange), else the one its switchboard sits in.
    fn home_tab(&self, sid: &str) -> Option<String> {
        let sh = self.shared.lock().unwrap();
        let panes = sh.data.iter().find(|(g, _)| g.session_id == sid).map(|(_, ps)| ps.clone()).unwrap_or_default();
        let role = |p: &Pane| p.tab == crate::layout::OPERATOR || p.tab == crate::layout::SUPERVISOR;
        panes.iter().find(|p| role(p)).or_else(|| panes.iter().find(|p| is_board(p))).map(|p| p.window_id.clone())
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
        if let View::Scoped(_, label, _) = &self.view {
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

    fn jump_exchange_board(&mut self) {
        self.link_at = Some(Instant::now());
        let ex = self.exchange.clone();
        self.say(MUTED, format!("→ {ex}"));
        let (shared, look, default) = (self.shared.clone(), self.look.clone(), self.default.clone());
        std::thread::spawn(move || {
            let cached = shared.lock().unwrap().exchange_board.clone();
            // still there, still the exchange switchboard? it may have moved tabs
            let cached = cached.and_then(|(sid, l, _, bid)| exchange_board_window(&look, &sid, &bid).map(|w| (sid, l, w, bid)));
            let was_cached = cached.is_some();
            let mut target = cached.or_else(|| find_exchange_board(&look, &ex, &default));
            shared.lock().unwrap().exchange_board = target.clone();
            for attempt in 0..2 {
                let Some((sid, label, wid, bid)) = target.clone() else {
                    shared.lock().unwrap().status = Some((BLOCKED, format!("✗ no switchboard for exchange {ex}"), Instant::now()));
                    return;
                };
                if jump_to(&shared, &sid, Some(&wid), Some(&bid), &label) || attempt == 1 || !was_cached {
                    return;
                }
                // the cached pane may have just gone; look again once
                target = find_exchange_board(&look, &ex, &default);
                shared.lock().unwrap().exchange_board = target.clone();
            }
        });
    }

    fn link_hot(&self) -> bool {
        self.link_hover || self.link_at.is_some_and(|t| t.elapsed() < Duration::from_millis(600))
    }

    /// Whether screen column X (0-based) on screen line LINE is on the exchange link.
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
                let tab = self.home_tab(&sid);
                self.jump(sid, tab, None);
            }
        }
    }

    fn just_focused(&self) -> bool {
        self.focus_in.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    fn click(&mut self, x: usize, line: usize) {
        if self.on_link(x, line) {
            if !self.just_focused() {
                self.jump_exchange_board();
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

    /// Handle a key; Some(why) to quit. Esc doesn't quit: see Stray.
    fn key(&mut self, code: KeyCode, mods: KeyModifiers, data: &Snapshot) -> Option<&'static str> {
        match code {
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return Some("ctrl-c pressed"),
            KeyCode::Char('q') => return Some("q pressed"),
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
            KeyCode::Char('0') | KeyCode::Char('~') if self.scope().is_some() => self.jump_exchange_board(),
            KeyCode::Char('a') => match &self.view {
                View::Exchange(_) => self.view = View::All,
                View::All => self.view = View::Exchange(self.exchange.clone()),
                View::Scoped(..) => {}
            },
            KeyCode::Char(c) if c.is_ascii_digit() || c == '~' => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                if let Some(t) = self.groups(data).get(&n).cloned() {
                    self.activate(t);
                }
            }
            _ => {}
        }
        None
    }
}

/// Keys that are really the rest of an escape sequence. The board turns on
/// mouse and focus reports, so the terminal sends it sequences such as
/// ESC [ < 35 ; 5 ; 3 M as the mouse moves. When one reaches it in two reads,
/// crossterm reads the lone ESC as the Esc key and the rest as typed keys
/// (`[`, `<`, `3`, `5`…, and a digit jumps to a session). Esc used to quit:
/// that is how an exchange switchboard in a pane the Rex app was showing dropped to a
/// shell. Now Esc does nothing, and the characters that closely follow it
/// are dropped.
#[derive(Default)]
struct Stray {
    esc_at: Option<Instant>,
}

impl Stray {
    /// How soon after an Esc (or a dropped character) a character counts as
    /// part of its sequence.
    const WINDOW: Duration = Duration::from_millis(250);

    /// Whether key CODE (with MODS) at NOW is one to act on.
    fn admit(&mut self, code: KeyCode, mods: KeyModifiers, now: Instant) -> bool {
        if code == KeyCode::Esc && mods.is_empty() {
            self.esc_at = Some(now);
            return false;
        }
        if let (Some(t), KeyCode::Char(_)) = (self.esc_at, code) {
            if now.saturating_duration_since(t) < Self::WINDOW {
                self.esc_at = Some(now);
                return false;
            }
        }
        self.esc_at = None;
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
/// The signal that asked the board to quit.
static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_quit(sig: libc::c_int) {
    SIGNAL.store(sig, Ordering::Relaxed);
    QUIT.store(true, Ordering::Relaxed);
}

fn signal_reason(sig: libc::c_int) -> String {
    match sig {
        libc::SIGHUP => "SIGHUP (its terminal hung up)".into(),
        libc::SIGTERM => "SIGTERM".into(),
        n => format!("signal {n}"),
    }
}

// ---- why it exits ----------------------------------------------------------------

/// Where a live board logs its start and its exit, and how it names itself
/// there (its argv and its pane).
static EXIT_LOG: OnceLock<(PathBuf, String, String)> = OnceLock::new();

/// The switchboards' log: ~/.crossbar/logs/switchboard.log.
pub fn log_path(home: &Home) -> PathBuf {
    home.root.join("logs").join("switchboard.log")
}

/// A line of that log: TIME ARGV [PANE] (pid N): WHAT.
pub fn log_line(argv: &str, pane: &str, pid: u32, what: &str) -> String {
    format!("{} {argv} [{pane}] (pid {pid}): {what}", util::local_iso_now())
}

/// How the log names a board run with ARGS here: its argv and its pane.
pub fn log_names(args: &[String]) -> (String, String) {
    let argv = std::iter::once("switchboard".to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    let at = match mux::here_pane() {
        Some(p) if mux::is_tmux_id(&p) => format!("tmux {p}"),
        Some(p) => format!("rex {p}"),
        None => "no pane".into(),
    };
    (argv, at)
}

fn note(what: &str) {
    if let Some((path, argv, pane)) = EXIT_LOG.get() {
        util::append_log(path, &log_line(argv, pane, std::process::id(), what));
    }
}

/// Log why the board exits, and say it on the pane, so a board that drops
/// to a shell is never silent.
fn farewell(why: &str) {
    note(&format!("exited: {why}"));
    // not eprintln: with the pane gone, stderr is too, and eprintln would panic
    let _ = writeln!(std::io::stderr(), "switchboard: exited: {why}");
}

fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/// Why this board's pane is gone, if it is: its terminal hung up or no
/// longer answers, or the process that started it (the pane's shell or cbar)
/// died.
fn pane_gone(parent: libc::pid_t) -> Option<String> {
    unsafe {
        if libc::getppid() != parent {
            return Some(format!("its parent (pid {parent}) exited (its pane closed?)"));
        }
        if libc::tcgetpgrp(0) < 0 {
            return Some("its terminal is gone".into());
        }
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        (libc::poll(&mut fd, 1, 0) > 0 && fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
            .then(|| "its terminal hung up".into())
    }
}

/// Exit when the pane goes away, or soon after SIGTERM/SIGHUP even if the
/// main loop is stuck (a hung-up terminal can keep crossterm reading forever).
fn watchdog() {
    let parent = unsafe { libc::getppid() };
    std::thread::spawn(move || {
        let mut asked: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if let Some(why) = pane_gone(parent) {
                farewell(&why);
                unsafe { libc::_exit(0) };
            }
            if QUIT.load(Ordering::Relaxed) {
                // give the main loop a moment to put the terminal back
                let t = *asked.get_or_insert_with(Instant::now);
                if t.elapsed() > Duration::from_secs(1) {
                    farewell(&format!("{}; its main loop didn't stop", signal_reason(SIGNAL.load(Ordering::Relaxed))));
                    unsafe { libc::_exit(0) };
                }
            }
        }
    });
}

/// Put the terminal back as the board found it.
fn restore_terminal(tty: bool) {
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
    if tty {
        let _ = terminal::disable_raw_mode();
    }
    let _ = std::io::stdout().flush();
}

fn live(mut board: Board) {
    note("started");
    if unsafe { libc::isatty(0) } == 1 {
        watchdog();
    }
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
                    let hit = find_exchange_board(&look, &ex, &default);
                    shared.lock().unwrap().exchange_board = hit;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_quit as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_quit as *const () as libc::sighandler_t);
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
    {
        // a panic (release builds abort) would print on the alternate screen and be lost
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(tty);
            farewell(&format!("it crashed: {info}"));
            hook(info);
        }));
    }
    let started = Instant::now();
    let mut loaded = false;
    let mut stray = Stray::default();
    let mut why: Option<String> = None;
    while !QUIT.load(Ordering::Relaxed) {
        let tick = spin_tick(started.elapsed());
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
        loaded = loaded || !data.is_empty() || started.elapsed() > Duration::from_secs(5);
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
        let zero = zero_label(&board.view, &board.exchange).is_some_and(|z| shown.iter().any(|(g, _)| g.label == z));
        let foot = footer(board.cols, status.as_ref(), &board.view, &link, zero);
        let _ = term.draw(|f| {
            let area = f.area();
            let body = Rect { x: area.x + 1, y: area.y, width: area.width.saturating_sub(1), height: area.height.saturating_sub(1) };
            f.render_widget(Paragraph::new(view), body);
            let foot_area = Rect { x: area.x + 1, y: area.y + area.height.saturating_sub(1), width: area.width.saturating_sub(1), height: 1 };
            f.render_widget(Paragraph::new(foot), foot_area);
        });
        if !tty {
            std::thread::sleep(Duration::from_millis(120));
            continue;
        }
        if !event::poll(Duration::from_millis(120)).unwrap_or(false) {
            continue;
        }
        while why.is_none() && event::poll(Duration::ZERO).unwrap_or(false) {
            let Ok(ev) = event::read() else { break };
            match ev {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if stray.admit(k.code, k.modifiers, Instant::now()) {
                        why = board.key(k.code, k.modifiers, &shown).map(String::from);
                    }
                }
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
        if why.is_some() {
            break;
        }
    }
    drop(term);
    restore_terminal(tty);
    let why = why.unwrap_or_else(|| signal_reason(SIGNAL.load(Ordering::Relaxed)));
    farewell(&why);
}

// ---- main ------------------------------------------------------------------

fn usage() -> ! {
    util::die("usage: switchboard [--exchange NAME|--all|--here|--session NAME] [--once|--json|--watch [--next]]")
}

pub fn main(args: Vec<String>) {
    let home = Home::from_env();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| -> Option<String> {
        let i = args.iter().position(|a| a == f)?;
        Some(args.get(i + 1).cloned().unwrap_or_else(|| usage()))
    };
    for a in &args {
        if a.starts_with('-') && !["--exchange", "--all", "--here", "--session", "--once", "--json", "--watch", "--next", "-h", "--help"].contains(&a.as_str()) {
            usage();
        }
    }
    if has("-h") || has("--help") {
        println!("{}", HELP.trim());
        return;
    }
    if !(has("--once") || has("--json") || has("--watch")) {
        // a live board: log its start and why it exits
        let (argv, at) = log_names(&args);
        let _ = EXIT_LOG.set((log_path(&home), argv, at));
        util::on_die_note(|m| note(&format!("exited: {m}")));
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
            let whose = match g.label.split_once(SEP) {
                Some((e, _)) if home.exchange_exists(e) => Whose::Project,
                None if home.exchange_exists(&g.label) => Whose::Exchange,
                _ => Whose::Other,
            };
            View::Scoped(g.session_id.clone(), label, whose)
        }
        None if has("--all") => View::All,
        None => View::Exchange(exchange.clone()),
    };
    let exchanges: Vec<String> = home.exchanges().into_iter().map(|x| x.name).collect();
    let scope_id = scope.as_ref().map(|g| g.session_id.clone());

    if has("--next") && !has("--watch") {
        util::die("--next goes with --watch");
    }
    if has("--watch") {
        let Some(g) = &scope else { util::die("--watch needs --here or --session NAME") };
        crate::watch::watch(&home, &g.session_id, &g.label, has("--next"));
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
    // in tmux, mark this pane so others can find the exchange switchboard
    let pane = util::env("TMUX_PANE");
    let marked = pane.as_ref().is_some_and(|p| {
        let role = if scope.is_some() { ROLE_SESSION } else { ROLE_EXCHANGE };
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
        shared: Arc::new(Mutex::new(Shared { data: Vec::new(), client: None, exchange_board: None, status: None })),
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
usage: switchboard [--exchange NAME|--all|--here|--session NAME] [--once|--json|--watch [--next]]

  switchboard                   the exchange switchboard: this exchange's sessions
  switchboard --exchange NAME   the switchboard of exchange NAME
  switchboard --all             every session, by exchange
  switchboard --session NAME    one session's tabs, a card each (a project's session:
                                its project switchboard)
  switchboard --here            same, for this pane's session
  --once                        print once and exit
  --json                        print the panes as JSON and exit
  --watch --here                a line as each report turns done, blocked or error,
                                stops or closes
  --watch --next --here         the same until the first event, then exit

Keys:  j/k ↑/↓ select · J/K tab/shift-tab next/prev session · g/G first/last
       enter jump · 1-9 session N · a this exchange / all · 0 exchange switchboard · q quit
Mouse: click a row or a card header to jump, wheel to move, ↖ to the exchange switchboard

A live board logs its start, and why it exits, to ~/.crossbar/logs/switchboard.log,
and says why on its pane.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn g(label: &str) -> (Group, Vec<Pane>) {
        (Group { session_id: label.into(), label: label.into(), mux: "rex".into(), window_id: None }, Vec::new())
    }

    /// The live drop: the Rex app sent the board a mouse or focus report
    /// that arrived as a lone ESC and then the rest. Esc quit the board.
    #[test]
    fn a_split_escape_sequence_is_not_keys() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let none = KeyModifiers::NONE;
        let mut s = Stray::default();
        assert!(!s.admit(KeyCode::Esc, none, at(0)));
        // ESC [ < 3 5 ; 5 ; 3 M: none of the tail are keys (3 and 5 would jump)
        for (i, c) in "[<35;5;3M".chars().enumerate() {
            assert!(!s.admit(KeyCode::Char(c), none, at(1 + i as u64)), "{c}");
        }
        // a key well after is one
        assert!(s.admit(KeyCode::Char('j'), none, at(400)));
        assert!(s.admit(KeyCode::Char('3'), none, at(401)));
        // a key that isn't a character right after Esc still counts
        assert!(!s.admit(KeyCode::Esc, none, at(500)));
        assert!(s.admit(KeyCode::Down, none, at(501)));
        // alt-x (ESC x in one read) isn't Esc
        let mut s = Stray::default();
        assert!(s.admit(KeyCode::Char('x'), KeyModifiers::ALT, at(0)));
    }

    #[test]
    fn quits_on_q_and_ctrl_c_not_esc() {
        let mut b = Board {
            view: View::Exchange("central".into()),
            exchange: "central".into(),
            default: "central".into(),
            exchanges: vec!["central".into()],
            shared: Arc::new(Mutex::new(Shared { data: Vec::new(), client: None, exchange_board: None, status: None })),
            look: Arc::new(Looker::new(false)),
            selected: None,
            targets: Vec::new(),
            offset: 0,
            last_index: 0,
            focus_in: None,
            link_hover: false,
            link_at: None,
            cols: 80,
        };
        let none = KeyModifiers::NONE;
        assert_eq!(b.key(KeyCode::Esc, none, &Vec::new()), None);
        assert_eq!(b.key(KeyCode::Char('q'), none, &Vec::new()), Some("q pressed"));
        assert_eq!(b.key(KeyCode::Char('c'), KeyModifiers::CONTROL, &Vec::new()), Some("ctrl-c pressed"));
    }

    #[test]
    fn spinner_follows_the_clock() {
        let ms = |n| spin_tick(Duration::from_millis(n));
        assert_eq!((ms(0), ms(119), ms(120), ms(1199), ms(1200)), (0, 0, 1, 9, 10));
        // however often it redraws in between: 50 redraws within one step show one frame
        let frames: std::collections::HashSet<usize> = (0..50).map(|i| ms(240 + i * 2)).collect();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn picks_an_exchanges_sessions() {
        let data = vec![g("central/stats"), g("other"), g("central"), g("work/stats"), g("work")];
        let ex: Vec<String> = vec!["central".into(), "work".into()];
        let labels = |v: &View| pick(&data, v, &ex).into_iter().map(|(g, _)| g.label).collect::<Vec<_>>();
        assert_eq!(labels(&View::Exchange("central".into())), ["central", "central/stats"]);
        assert_eq!(labels(&View::All), ["central", "central/stats", "work", "work/stats", "other"]);
        assert_eq!(title("central/stats", &View::Exchange("central".into())), "stats");
        assert_eq!(title("central/stats", &View::All), "central/stats");
    }

    #[test]
    fn scores_exchange_boards() {
        assert_eq!(fit_score("central", "central", "central", "central"), Some(0));
        assert_eq!(fit_score("", "central", "central", "central"), Some(0));
        assert_eq!(fit_score("", "other", "central", "central"), Some(1));
        assert_eq!(fit_score("*", "x", "central", "central"), Some(2));
        assert_eq!(fit_score("work", "work", "central", "central"), None);
        assert_eq!(fit_score("", "work", "work", "central"), Some(0));
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
            since_at: String::new(),
            owner: None,
        };
        let data = vec![(Group { session_id: "s".into(), label: "central/stats".into(), mux: "tmux".into(), window_id: None }, vec![p])];
        let f = render(&data, &View::Exchange("central".into()), 60, 0, None, "central", false);
        let text: Vec<String> = f.lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert!(text[0].starts_with("◇ switchboard exchange central · projects & agents "));
        assert_eq!(text[1], "✓ 1 done");
        assert!(text[3].starts_with("╭─ stats tmux ~/x ─") && text[3].ends_with("─ 1 ─╮"), "{}", text[3]);
        assert!(text[4].contains("✓ fix-x            done") && text[4].ends_with("2m │"), "{}", text[4]);
        assert!(text[5].contains("all good"));
        assert!(text.iter().all(|l| l.chars().count() == 60 || l.is_empty() || !l.starts_with(['╭', '│', '╰'])));
        assert_eq!(f.targets[4], Some(Target::Pane("s".into(), "w".into(), "b".into())));
        // a scoped board says whose it is: a project's, the exchange's, or just a session's
        let head = |label: &str, whose| {
            let f = render(&data, &View::Scoped("s".into(), label.into(), whose), 80, 0, None, "central", false);
            f.lines[0].spans.iter().map(|s| s.content.to_string()).collect::<String>()
        };
        let h = head("stats", Whose::Project);
        assert!(h.starts_with("◇ switchboard project stats · tabs & agents") && h.contains("↖ central"), "{h}");
        let h = head("central", Whose::Exchange);
        assert!(h.starts_with("◇ switchboard exchange central · tabs & agents"), "{h}");
        let h = head("notes", Whose::Other);
        assert!(h.starts_with("◇ switchboard notes · tabs & agents"), "{h}");
    }
}
