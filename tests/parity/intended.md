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
- Exchange naming (old side only): the new side's exchange is `central`, so 0.4's `central` keeps
  its name; project `alpha`'s session (`session 'alpha'`, `=alpha`, a JSON `"label": "alpha"`) is
  `central/alpha`; `~/.crossbar/projects/` is `~/.crossbar/exchanges/central/projects/` and
  `~/.crossbar/central/` is `~/.crossbar/exchanges/central/`.
- Strip rule `deprecation-note` (new side's stderr): `cbar: \`cbar agent …\` is deprecated; use
  \`cbar …\``. The old forms are kept as aliases with a note (exchanges.md), so cases that run an
  old form would otherwise all differ by that line.

## agent-new-t-nogit

A project that isn't a git repo gets no worktree (`-w`), and cbar says so (central.md report; v050.md: worktrees only when the project is a git repo).

```diff
--- old
+++ new
@@ -1,2 +1,2 @@
-started t-nogit in a new tab (%N)
+started t-nogit in a new tab (%N) (no worktree: ~/repo-b isn't a git repo)
 --- exit 0
```

## agent-tail-nogit

The same: the agent in a non-git project starts without `-w` (the stub prints what it got).

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 stub claude: t-nogit
-worktree: t-nogit
+worktree: -
 brief: t-nogit.md
 state: done | no repo here
```

## board-main-once

The main board shows one exchange and names it in its subtitle (exchanges.md).

```diff
--- old
+++ new
@@ -1,3 +1,3 @@
-◇ switchboard agents & panes by session                                                        HH:MM
+◇ switchboard central · agents & panes by session                                              HH:MM
 ◆ 1 blocked  ✗ 1 error  ✓ 2 done  ⠋ 1 working  ○ 4 idle
 
```

## board-main-once-color

The same as board-main-once, with colors.

```diff
--- old
+++ new
@@ -1,3 +1,3 @@
-[b fg=#be95ff]◇[/] [b fg=#be95ff]switchboard[/] [fg=#7b7c7e]agents[/] [fg=#7b7c7e]&[/] [fg=#7b7c7e]panes[/] [fg=#7b7c7e]by[/] [fg=#7b7c7e]session[/]                                                        [fg=#7b7c7e]HH:MM[/]
+[b fg=#be95ff]◇[/] [b fg=#be95ff]switchboard[/] [fg=#7b7c7e]central[/] [fg=#7b7c7e]·[/] [fg=#7b7c7e]agents[/] [fg=#7b7c7e]&[/] [fg=#7b7c7e]panes[/] [fg=#7b7c7e]by[/] [fg=#7b7c7e]session[/]                                              [fg=#7b7c7e]HH:MM[/]
 [fg=#ee5396]◆[/] [fg=#ee5396]1[/] [fg=#ee5396]blocked[/]  [fg=#ff7eb6]✗[/] [fg=#ff7eb6]1[/] [fg=#ff7eb6]error[/]  [fg=#25be6a]✓[/] [fg=#25be6a]2[/] [fg=#25be6a]done[/]  [fg=#78a9ff]⠋[/] [fg=#78a9ff]1[/] [fg=#78a9ff]working[/]  [fg=#7b7c7e]○[/] [fg=#7b7c7e]4[/] [fg=#7b7c7e]idle[/]
 
```

## err-agent-new-exists

Names the session, which now carries the exchange (`central/alpha`).

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: this session already has a tab 't-done'
+cbar: session 'central/alpha' already has a tab 't-done'
 --- exit 1
```

## err-agent-unknown

Any diff. `cbar agent` is a deprecated alias; its help maps each form to the path-based one.

## err-new-nodir

No z/zoxide (rust.md addendum): DIR is a folder or the current one, so there's no match to "pass"; the message names the missing folder.

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: no directory for 'gamma' (pass one)
+cbar: no folder /nonexistent/parity
 --- exit 1
```

## err-new-option

The new usage error appends the usage line, which has the path forms.

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: unknown option '--frob'
+cbar: unknown option '--frob' (usage: cbar new PATH [DIR|BRIEF] [--tmux|--rex] [--no-worktree] [--agent] [--exchange])
 --- exit 1
```

## err-new-usage

`cbar new` takes a PATH (project, project/agent, --exchange); exchanges.md.

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: usage: cbar new NAME [DIR] [--tmux|--rex]
+cbar: usage: cbar new PATH [DIR|BRIEF] [--tmux|--rex] [--no-worktree] [--agent] [--exchange]
 --- exit 1
```

## err-open-none

Paths: every command reports a path that resolves to nothing the same way. (The old hint `cbar new nope` is gone; confirm.)

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: no session 'nope' (cbar new nope)
+cbar: nothing at 'nope' (cbar ls)
 --- exit 1
```

## err-rm-none

Paths: the same message as err-open-none.

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: no project or session 'nope' (cbar ls)
+cbar: nothing at 'nope' (cbar ls)
 --- exit 1
```

## err-rm-notty

The project's path carries the exchange (`central/beta`).

```diff
--- old
+++ new
@@ -1,4 +1,4 @@
 
 --- stderr
-cbar: pass -y to remove 'beta' without asking
+cbar: pass -y to remove 'central/beta' without asking
 --- exit 1
```

## err-unknown-command

Any diff. The usage text is the new path-based CLI (exchanges.md).

## help

Any diff. The usage text is the new path-based CLI (exchanges.md).

## help-agent

Any diff. `cbar agent` is a deprecated alias; its help maps each form to the path-based one.

## ls

`cbar ls` is a tree: the exchange with its supervisor on top, projects under it, their agents under them; the last column is CLAUDE (central.md, exchanges.md).

```diff
--- old
+++ new
@@ -1,4 +1,11 @@
-NAME   TERMINAL  PATH      SESSION  OPERATOR
-alpha  tmux      ~/repo-a  running  idle
-beta   tmux      ~/repo-b  running  idle
+NAME           TERMINAL  PATH      SESSION  CLAUDE
+central        tmux      ~         running  supervisor idle
+├─ alpha       tmux      ~/repo-a  running  operator idle
+│  ├─ t-done                                done now
+│  ├─ t-ask                                 blocked now
+│  ├─ t-boom                                error now
+│  ├─ t-busy                                working now
+│  └─ t-chat                                idle now
+└─ beta        tmux      ~/repo-b  running  operator idle
+   └─ t-nogit                               done now
 --- exit 0
```

## ls-after-rm

The same tree as ls, after t-done and t-boom were closed and beta removed.

```diff
--- old
+++ new
@@ -1,3 +1,8 @@
-NAME   TERMINAL  PATH      SESSION  OPERATOR
-alpha  tmux      ~/repo-a  running  idle
+NAME          TERMINAL  PATH      SESSION  CLAUDE
+central       tmux      ~         running  supervisor idle
+└─ alpha      tmux      ~/repo-a  running  operator idle
+   ├─ t-ask                                blocked 1m
+   ├─ t-busy                               working 1m
+   ├─ t-chat                               done now
+   └─ t-path                               idle now
 --- exit 0
```

## new-project-alpha

The hint names the project by its path (`/central/alpha`).

```diff
--- old
+++ new
@@ -1,3 +1,3 @@
 created session 'central/alpha' in ~/repo-a with an operator and a switchboard (tmux)
-attach: cbar open alpha
+attach: cbar open /central/alpha
 --- exit 0
```

## new-project-beta

The hint names the project by its path (`/central/beta`).

```diff
--- old
+++ new
@@ -1,3 +1,3 @@
 created session 'central/beta' in ~/repo-b with an operator and a switchboard (tmux)
-attach: cbar open beta
+attach: cbar open /central/beta
 --- exit 0
```

## rm-project

The project's path carries the exchange (`central/beta`).

```diff
--- old
+++ new
@@ -1,2 +1,2 @@
-removed beta
+removed central/beta
 --- exit 0
```

## tail-chat

0.4 typed the agent's command line into a shell, so it shows up on the agent's screen; 0.5 starts it as the pane's own command (rust.md addendum: any shell).

```diff
--- old
+++ new
@@ -1,3 +1,2 @@
-$BIN/claude-relay -n t-chat -w t-chat Read\ ~/.crossbar/exchanges/central/projects/alpha/briefs/t-chat.md\ and\ do\ the\ task\ it\ describes.$ $BIN/claude-relay -n t-chat -w t-chat Read\ ~/.crossbar/exchanges/central/projects/alpha/briefs/t-chat.md\ and\ do\ the\ task\ it\ describes.
 stub claude: t-chat
 worktree: t-chat
```

## version

The Rust cbar knows its own version (VERSION is 0.5.0); 0.4 only read ~/.crossbar/version.

```diff
--- old
+++ new
@@ -1,2 +1,2 @@
-unknown (not installed by install)
+0.5.0 (not installed by install)
 --- exit 0
```

## watch-restart

The addendum "cbar watch at startup": a restarted watch prints what is blocked or in error again, but not a done the first watch already printed. 0.4 printed nothing at startup.

```diff
--- old
+++ new
@@ -1,2 +1,3 @@
-
+t-ask: blocked (permission) — allow npm test?
+t-boom: error — tests failed
 --- exit watch ran
```
