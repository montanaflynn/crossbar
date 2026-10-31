# Parity harness

Runs the 0.4 crossbar (zsh `bin/cbar` and Python `bin/switchboard` from `main`) and the Rust one
(built from a checkout) side by side in a sandbox, and diffs what they print.

```
tests/parity/run NEW_CHECKOUT          # tmux (a private server per side)
tests/parity/run --rex NEW_CHECKOUT    # Rex (a private server per side, never the live one)
tests/parity/run -v --only board NEW_CHECKOUT
tests/parity/run --accept ls NEW_CHECKOUT   # take ls's current diff as intended
```

It needs tmux, zsh, python3 and git (the old side's own requirements), cargo for the build, and for
`--rex`, rex with zsh as your login shell. It prints one line per case (`pass`, `intended`, `DIFF`,
`CHANGED`) and exits 1 on any DIFF or CHANGED. The last run's normalized and raw outputs and diffs
are in `tests/parity/out/<case>/`.

## The sandbox

Each side gets, under one `/tmp/cbpar-*` folder that is removed afterwards (`--keep` keeps it):

- a scratch HOME (with an empty ZDOTDIR) holding the project folders `repo-a` (a git repo, project
  `alpha`) and `repo-b` (not one, project `beta`), and the side's CROSSBAR_HOME;
- its own tmux server (`tmux -L cbpar-old-PID`), or its own Rex server on a private socket. A tmux
  run puts a `rex` on PATH that always fails, so neither side sees the live Rex server. A Rex run
  puts one there that always passes `-S <private socket> --autostart=false` (rex falls back to
  the home server when `$REX_SERVER`'s socket is missing), starts the server with `REX_SERVER` set
  to its own socket (a Rex pane runs Rex's own `rex` first), and stops unless a probe in a shell
  pane and in a command pane finds the stub `claude`, the sandbox's settings, and only the probe
  sessions in `rex ls`;
- `stub/claude` in place of Claude. It reports a state with OSC 7501 (Rex reads it, and in tmux the
  real claude-relay turns it into pane options), taken from the first line of the agent's brief
  (`stub: blocked permission | allow npm test?`); typed lines make it work, then report done.
  claude-relay for both sides comes from the new checkout's build.

The old side's `central` is built by hand (a supervisor stub and a main switchboard), as the live one
was, since 0.4's cbar has no `cbar up`; the new side runs `cbar new --exchange main`. Then both run
`cbar new alpha|beta DIR`, start the agents in `fixtures/briefs` with `cbar agent new`, wait for
their states, and run the cases in `run` (`cases()`): boards (`--once`, `--json`, local and main),
`ls`, `help`, the agent commands, errors and exit codes, `watch`, `rm`. The tmux server or Rex
server, and anything else a run started, is stopped when it ends, however it ends.

## cbar restart

`tests/restart/run [--rex] CHECKOUT` uses this sandbox for the new side only and tests
`cbar restart`: the stub records each start's argv (`$CBAR_STUB_LOG`) and writes
`~/.claude/sessions/PID.json` under the scratch HOME (`$CBAR_STUB_SESSIONS`), as Claude Code does.
Neither is set in a parity run.

## Intended differences

`intended.md` lists every normalization and every difference that is meant to be there, with why.
Nothing is hidden by the harness that isn't listed there.
