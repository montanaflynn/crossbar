# crossbar

Run a team of Claude Code agents across projects in the Rex
terminal multiplexer.

- **Supervisor**: a Claude in the `central` session that coordinates across projects.
- **Operators**: one per project. Each operator is a Claude in tab 1 (`operator`) of the project's Rex
  session. It plans the work, hands tasks to agents in other tabs, watches them and reports back.
- **Agents**: Claudes that an operator or the supervisor starts in other tabs,
  each with a brief and a report file.
- **Switchboard**: a live TUI of every session, tab and agent with its status
  (idle, working, done, blocked, error). Click or press enter on a pane to jump to it. Each
  project gets a local switchboard next to its operator.

## Install

Requires zsh, Python 3, [jq](https://jqlang.org), [Claude Code](https://claude.com/claude-code)
and Rex.

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
cbar new NAME [DIR]               register a project and create its Rex session with an operator
                                  and a local switchboard (DIR defaults to the `z` match for NAME)
cbar operator                     run the operator Claude in this pane
cbar board [--here|--session N]   run the switchboard (--once prints once and exits)
cbar ls                           registered projects, their sessions and operators
cbar open NAME                    switch the Rex app to that project's session
```

## Layout

```
~/.crossbar/
├── bin/cbar, bin/switchboard    installed by ./install (don't edit, see below)
├── operator.md                  the operator role prompt
├── version
├── central/{briefs,reports}/    the supervisor's agents
└── projects/<name>/             one per project: project.toml (name, path, created),
                                 briefs/ and reports/
```

Operators save each agent's brief to `projects/<name>/briefs/<task>.md` and
have the agent write its results to `projects/<name>/reports/<task>.md`.

## Develop

Edit this repo, then run `./install` to try your changes. Installing replaces
`bin/` and `operator.md` in `~/.crossbar`, so make changes here, not in the installed
copy. It never touches `projects/` or `central/`. Files are swapped in by
rename, so panes already running the old copy keep working.
