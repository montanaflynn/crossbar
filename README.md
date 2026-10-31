<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/crossbar-hero-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="assets/crossbar-hero-light.png">
  <img src="assets/crossbar-hero-light.png" alt="Crossbar" width="960">
</picture>

- **Exchanges**: sets of projects you run together, such as `main`, `work` and `personal`.
  Each has its own supervisor, main switchboard, briefs and reports.
- **Supervisor**: a Claude in the exchange's session (`main`) that coordinates across its projects.
- **Operators**: one per project. Each operator is a Claude in tab 1 (`operator`) of the project's
  session (`main/stats`). It plans the work, hands tasks to agents in other tabs, watches them
  and reports back.
- **Agents**: Claudes that an operator or the supervisor starts in other tabs,
  each with a brief and a report file.
- **Switchboard**: a live TUI of an exchange's sessions, tabs and agents with their status
  (idle, working, done, blocked, error). Click or press enter on a pane to jump to it. Each
  project gets a local switchboard next to its operator.

crossbar doesn't bring conventions of its own. Operators and agents follow each project's
(its CLAUDE.md, AGENTS.md, CONTRIBUTING and history) and yours.

## Install

Requires [Claude Code](https://claude.com/claude-code) and Rex or tmux (3.0+), on macOS or
Linux. Any login shell works.

```sh
git clone https://github.com/montanaflynn/crossbar.git
cd crossbar && ./install
```

`./install` downloads `cbar`, `switchboard` and `claude-relay` for your platform from the
GitHub release (or builds them, if [cargo](https://rustup.rs) is installed), puts them in
`~/.crossbar/bin` with the role prompts, and links them into `~/.local/bin`. Update later with:

```sh
cbar update     # install the latest main
cbar version
```

## Use

Everything is a path, `exchange/project/agent`, and its depth says what it is. Paths are
relative to where you are: inside a project's session, `fix-x` is its agent; inside an
exchange's session (or outside crossbar, in the default exchange) `stats` is its project.
A name that isn't found nearby is looked up a level higher. `/main/stats` is absolute and
`..` goes up. `operator` and `supervisor` are the tabs a project and an exchange run in.

```
cbar new stats [DIR] [--tmux|--rex]  register a project and start its session with an operator
                                     and a local switchboard (DIR defaults to this folder; the
                                     terminal to the one you're in, else Rex if it's running)
cbar new stats/fix-x [BRIEF]         start an agent in a new tab of stats on its brief,
                                     briefs/fix-x.md (BRIEF is copied there); in its own git
                                     worktree when the project is a git repo (--no-worktree: not)
cbar new --agent research [BRIEF]    the same in the exchange's own session
cbar new --exchange work             create an exchange and start its supervisor and main board
cbar up [EXCHANGE]                   start the supervisor, the main board and every project's session
                                     and operator that isn't running; leaves whatever runs alone
cbar ls [PATH]                       exchanges > projects > agents, with their status
cbar open PATH                       switch to that session, or an agent's tab (outside tmux: attach)
cbar send PATH TEXT                  type a message, only if that Claude is idle or done
cbar status|tail|wait PATH           its status, its screen, or wait until it stops working
cbar rm [-y] [-f] PATH               an agent: close its tab, and remove its worktree if clean and
                                     merged; a project or exchange: kill its session and unregister
                                     it, briefs and reports too (asks first)
cbar watch                           a line each time one of your reports turns done, blocked or error,
                                     stops or closes (operators: their agents; supervisors: their agents
                                     and their projects' operators)
cbar board [EXCHANGE|PROJECT|--all]  the switchboard (--once prints once, --json prints JSON)
cbar migrate                         move a 0.4 layout into exchanges/main (see below)
```

A project's path in `send`, `status`, `tail` and `wait` means its operator; an exchange's, its
supervisor. Panes run `cbar run supervisor|operator|board|agent` as their own command, so
nothing is typed into a shell; when that program exits, the pane drops to your login shell.

The 0.4 forms still work and print a note: `cbar operator` and `cbar supervisor` (now
`cbar run …`), `cbar board --here` (`cbar run board`), and `cbar agent new|ls|status|tail|send|wait|close`
(`cbar new`, `ls .`, `status`, `tail`, `send`, `wait`, `rm`).

### The switchboard

`cbar board` shows the sessions of the exchange you're in (`a` switches to every exchange,
and back). In a project, `cbar run board` shows that session's tabs; its `↖ main` link (and
`0`) jumps to the exchange's main board. Keys: `j`/`k` or arrows to select, tab to the next
session, enter to jump, `1`-`9` for a session, `q` to quit. The mouse works too.

### Rex and tmux

Each project and exchange lives in one terminal, recorded in its toml file. A switchboard
shows Rex and tmux sessions together, wherever they run, and jumps to any pane: in Rex it
asks the Rex app, in tmux it switches the tmux client.

Claude Code reports its own status (state, what it's blocked on, and a one-line
summary) to terminals that answer its OSC 7501 probe. Rex does; tmux doesn't, and the
answer has to arrive in the byte stream, so nothing outside it can give one. So in
tmux, `cbar` starts Claude through `claude-relay`, a small proxy that answers the
probe and writes the status to the pane's tmux options, where the switchboard reads it.
tmux then shows the same states and messages as Rex.

`claude-relay` takes Claude's arguments and runs it on a pty of its own; outside tmux
it just runs `claude`. Ctrl-Z, resizing and exit codes work as usual. To get full status
for a Claude you start yourself in tmux, run `claude-relay` in its place. Without it,
tmux shows idle, working or blocked from `claude agents`, with no message.

To try tmux, from any terminal:

```sh
cbar new myproject ~/code/myproject --tmux
cbar open myproject          # attaches; inside tmux, switches to it
```

## Layout

```
~/.crossbar/
├── bin/                          cbar, switchboard, claude-relay: installed by ./install
├── operator.md, supervisor.md    the role prompts
├── config.toml                   default_exchange = "main"
├── version
└── exchanges/<exchange>/
    ├── exchange.toml             name, terminal, created
    ├── briefs/ reports/          the supervisor's agents
    └── projects/<project>/
        ├── project.toml          name, path, terminal, created
        └── briefs/ reports/      the operator's agents
```

Session names: the supervisor's is the exchange's (`main`); a project's is
`<exchange>/<project>` (`main/stats`), so two exchanges can each have a `stats`.

### From 0.4

0.4 kept `central/` and `projects/` directly in `~/.crossbar`. `cbar migrate` moves them into
`exchanges/main`: it backs both up to `backups/` first, leaves `central` and `projects` as links
to their new places (so paths given to running Claudes still work), and renames the sessions
(`central` → `main`, `stats` → `main/stats`) without stopping what runs in them. `-n` shows what
it would do; `--no-sessions` leaves the sessions alone. It's safe to run again. Until you run it,
the other commands say so and stop.

## Develop

crossbar is a cargo workspace: `crossbar/` (`cbar` and `switchboard`) and `claude-relay/`.
Edit this repo, then run `./install --build` to try your changes. Installing replaces `bin/` and
the role prompts in `~/.crossbar`, so make changes here, not in the installed copy. It never
touches your exchanges. Files are swapped in by rename, so panes already running the old copy
keep working. Pushing a tag `vX.Y.Z` that matches `VERSION` builds the release binaries in
GitHub Actions.

## License

MIT
