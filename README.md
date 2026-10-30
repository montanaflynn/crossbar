<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/crossbar-hero-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="assets/crossbar-hero-light.png">
  <img src="assets/crossbar-hero-light.png" alt="Crossbar" width="960">
</picture>

- **Supervisor**: a Claude in the `central` session that coordinates across projects.
- **Operators**: one per project. Each operator is a Claude in tab 1 (`operator`) of the project's
  session. It plans the work, hands tasks to agents in other tabs, watches them and reports back.
- **Agents**: Claudes that an operator or the supervisor starts in other tabs,
  each with a brief and a report file.
- **Switchboard**: a live TUI of every session, tab and agent with its status
  (idle, working, done, blocked, error). Click or press enter on a pane to jump to it. Each
  project gets a local switchboard next to its operator.

## Install

Requires zsh, Python 3, [jq](https://jqlang.org), [Claude Code](https://claude.com/claude-code),
and Rex or tmux (3.0+), on macOS or Linux. `./install` downloads `claude-relay` for your
platform from the GitHub release (or builds it, if [cargo](https://rustup.rs) is installed).

```sh
git clone https://github.com/montanaflynn/crossbar.git
cd crossbar && ./install
```

This copies `cbar`, `switchboard` and `operator.md` into `~/.crossbar` and
links `cbar` and `switchboard` into `~/.local/bin`. Update later with:

```sh
cbar update     # install the latest main
cbar version
```

## Use

```
cbar new NAME [DIR] [--tmux|--rex]   register a project and create its session with an operator
                                     and a local switchboard (DIR defaults to the `z` match for NAME;
                                     the terminal defaults to the one you're in, else Rex if it's running)
cbar operator                        run the operator Claude in this pane
cbar board [--here|--session N]      run the switchboard (--once prints once, --json prints JSON)
cbar ls                              registered projects, their terminal, sessions and operators
cbar open NAME                       switch to that project's session (outside tmux: attach to it)
cbar rm [-y] NAME                    kill its session and unregister it, briefs and reports too (asks first)
cbar watch                           a line each time one of your reports turns done, blocked or error,
                                     stops or closes (operators: their agents; central: also each operator)
```

Operators start and watch agents in their own session, the same way in Rex and tmux:

```
cbar agent new TASK [--no-worktree]  start Claude in a new tab on briefs/TASK.md
cbar agent ls                        agents in this session and their status
cbar agent status|tail|wait TASK     one agent's status, its screen, or wait until it stops working
cbar agent send TASK TEXT            type a message, only if the agent is idle or done
cbar agent close [-f] TASK           close its tab; remove its worktree if clean and merged
```

### Rex and tmux

Each project lives in one terminal, recorded in its `project.toml`. A switchboard
shows every Rex and tmux session together, wherever it runs, and jumps to any pane:
in Rex it asks the Rex app, in tmux it switches the tmux client.

Claude Code reports its own status (state, what it's blocked on, and a one-line
summary) to terminals that answer its OSC 7501 probe. Rex does; tmux doesn't, and the
answer has to arrive in the byte stream, so nothing outside it can give one. So in
tmux, `cbar` starts Claude through `claude-relay`, a small Rust proxy that answers the
probe and writes the status to the pane's tmux options, where switchboard reads it.
tmux then shows the same states and messages as Rex.

`claude-relay` takes Claude's arguments and runs it on a pty of its own; outside tmux
it just runs `claude`. Ctrl-Z, resizing and exit codes work as usual. To get full status
for a Claude you start yourself in tmux, run `claude-relay` in its place (or
`alias claude=claude-relay`). Without it, tmux shows idle, working or blocked from
`claude agents`, with no message.

To try tmux, from any terminal:

```sh
cbar new myproject ~/code/myproject --tmux
cbar open myproject          # attaches; inside tmux, switches to it
cbar board                   # in another tmux window: the main switchboard
```

## Layout

```
~/.crossbar/
├── bin/                         cbar, switchboard, claude-relay: installed by ./install
│                                (don't edit, see below)
├── operator.md                  the operator role prompt
├── version
├── central/{briefs,reports}/    the supervisor's agents
└── projects/<name>/             one per project: project.toml (name, path, terminal, created),
                                 briefs/ and reports/
```

Operators save each agent's brief to `projects/<name>/briefs/<task>.md` and
have the agent write its results to `projects/<name>/reports/<task>.md`.

## Develop

Edit this repo, then run `./install` to try your changes (`./install --build` to build
`claude-relay` from your checkout instead of downloading the release). Pushing a tag
`vX.Y.Z` that matches `VERSION` builds the release binaries in GitHub Actions. Installing replaces
`bin/` and `operator.md` in `~/.crossbar`, so make changes here, not in the installed
copy. It never touches `projects/` or `central/`. Files are swapped in by
rename, so panes already running the old copy keep working.

## License

MIT
