# Intended differences

What the parity harness (`tests/parity/run`) accepts as meant to differ between the 0.4 crossbar
(zsh `bin/cbar`, Python `bin/switchboard`) and the Rust one. Each `## case` says why, then either
the exact diff it accepts (a ```diff block, written by `run --accept CASE`) or "Any diff." for a case
whose output is expected to keep changing. A case whose diff isn't the one here shows as CHANGED.

## Normalizations

Applied to both sides' output before comparing; the summary says which strip rules fired.

- Paths: each side's CROSSBAR_HOME is `~/.crossbar`, the scratch HOME is `~`, the sandbox `$SANDBOX`,
  the side's bin folder `$BIN`, its stub folder `$STUB`, its tmux socket name `$SOCK`.
- Ids: tmux `%12` `@3` `$1` become `%N` `@N` `$N`; Rex `session:<uuid>` (and window, block, client)
  become `session:ID`.
- Times: the board clock `HH:MM`; in `--json`, a non-empty `since` is `<age>`. Ages in text (`now`,
  `2m`) are left as they are: every side captures within its first minute.
- Escape sequences: removed, except in `-color` cases, where each run of text is shown with its
  style (`[fg=#be95ff b]…[/]`), however the SGR codes were written.
- `--json`: groups sorted by label, panes by tab and label, keys sorted.
- Exchange naming (old side only): `central` is `main`; project `alpha`'s session (`session 'alpha'`,
  `=alpha`, a JSON `"label": "alpha"`) is `main/alpha`; `~/.crossbar/projects/` is
  `~/.crossbar/exchanges/main/projects/`.
- Strip rule `deprecation-note` (new side's stderr): `cbar: \`cbar agent …\` is deprecated; use
  \`cbar …\``. The old forms are kept as aliases with a note (exchanges.md), so cases that run an
  old form would otherwise all differ by that line.
