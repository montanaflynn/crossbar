# Intended differences

What the parity harness (`tests/parity/run`) accepts as meant to differ between the 0.4 crossbar
(zsh `bin/cbar`, Python `bin/switchboard`) and the Rust one. Each `## case` says why, then either
the exact diff it accepts (a ```diff block, written by `run --accept CASE`) or "Any diff." for a case
whose output is expected to keep changing. A case whose diff isn't the one here shows as CHANGED.

## Normalizations

Applied to both sides' output before comparing; the summary says which strip rules fired.

- Paths: each side's CROSSBAR_HOME is `~/.crossbar`, the scratch HOME is `~`, the sandbox `$SANDBOX`,
  the side's bin folder `$BIN`, its stub folder `$STUB`, its tmux socket name `$SOCK`.
- Ids, the same for both terminals: tmux `%12` and Rex `block:<uuid>` become `<pane>`, tmux `@3`
  and Rex `window:<uuid>` `<window>`, tmux `$1` and Rex `session:<uuid>` `<session>` (Rex `client:`
  and `layer:` ids likewise). A case whose Rex diff can't match its tmux one has a `## CASE (rex)`
  entry of its own, used only with `--rex`.
- Times: the board clock `HH:MM`; in `--json`, a non-empty `since` is `<age>`; at the end of a line
  after a state (`cbar ls`'s `done 1m`), `<age>`; a board row's age at its right edge and
  `cbar agent ls`'s SINCE, `<age>` too: a slow side (Rex, the 0.4 board) can reach those cases
  minutes after its agents set their states.
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
-started t-nogit in a new tab (<pane>)
+started t-nogit in a new tab (<pane>) (no worktree: ~/repo-b isn't a git repo)
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

## board-here-json (rex)

The same as board-local-json, through `cbar board --here --json`.

```diff
--- old
+++ new
@@ -2,66 +2,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "operator",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell",
-       "owner": "<client>"
-      },
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "switchboard"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ],
-      "split": {
-       "after": {
-        "block_id": "<pane>",
-        "rect": [
-         0.7,
-         0,
-         0.30000000000000004,
-         1
-        ]
-       },
-       "before": {
-        "block_id": "<pane>",
-        "rect": [
-         0,
-         0,
-         0.7,
-         1
-        ]
-       },
-       "direction": "horizontal",
-       "ratio": 0.7,
-       "split_id": "split:<uuid>"
-      }
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -98,36 +38,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-ask",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -151,36 +61,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-boom",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -204,36 +84,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-busy",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -257,36 +107,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-chat",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -310,36 +130,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-done",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
```

## board-local-json (rex)

Scoped to one session, 0.4 passed Rex's raw window record through as each group (`layers`, `focused_block_id`); the Rust board gives the same slim group as everywhere (`session_id`, `label`, `mux`, `window_id`). The panes are the same. Confirm: `--json` is documented output.

```diff
--- old
+++ new
@@ -2,66 +2,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "operator",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell",
-       "owner": "<client>"
-      },
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "switchboard"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ],
-      "split": {
-       "after": {
-        "block_id": "<pane>",
-        "rect": [
-         0.7,
-         0,
-         0.30000000000000004,
-         1
-        ]
-       },
-       "before": {
-        "block_id": "<pane>",
-        "rect": [
-         0,
-         0,
-         0.7,
-         1
-        ]
-       },
-       "direction": "horizontal",
-       "ratio": 0.7,
-       "split_id": "split:<uuid>"
-      }
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -98,36 +38,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-ask",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -151,36 +61,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-boom",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -204,36 +84,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-busy",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -257,36 +107,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-chat",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
@@ -310,36 +130,6 @@
  {
   "group": {
-   "focused_block_id": "<pane>",
    "label": "t-done",
-   "layers": [
-    {
-     "blocks": [
-      {
-       "block_id": "<pane>",
-       "creator_name": "com.superlogical.terminal",
-       "flavor": "com.superlogical.terminal.shell",
-       "host_id": "<uuid>",
-       "label": "shell"
-      }
-     ],
-     "bounds": [
-      0,
-      0,
-      1,
-      1
-     ],
-     "kind": "tiled",
-     "layer_id": "<layer>",
-     "layout": {
-      "block_id": "<pane>",
-      "rect": [
-       0,
-       0,
-       1,
-       1
-      ]
-     }
-    }
-   ],
+   "mux": "rex",
    "session_id": "<session>",
    "window_id": "<window>"
```

## board-main-json (rex)

The Rust board's `--json` gives every group its `mux`, Rex ones too (0.4 left it out for Rex and readers assumed rex). Confirm: `--json` is documented output.

```diff
--- old
+++ new
@@ -3,4 +3,5 @@
   "group": {
    "label": "central",
+   "mux": "rex",
    "session_id": "<session>"
   },
@@ -37,4 +38,5 @@
   "group": {
    "label": "central/alpha",
+   "mux": "rex",
    "session_id": "<session>"
   },
@@ -136,4 +138,5 @@
   "group": {
    "label": "central/beta",
+   "mux": "rex",
    "session_id": "<session>"
   },
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
+│  ├─ t-done                                done <age>
+│  ├─ t-ask                                 blocked <age>
+│  ├─ t-boom                                error <age>
+│  ├─ t-busy                                working <age>
+│  └─ t-chat                                idle <age>
+└─ beta        tmux      ~/repo-b  running  operator idle
+   └─ t-nogit                               done <age>
 --- exit 0
```

## ls (rex)

`cbar ls` is a tree: the exchange with its supervisor on top, projects under it, their agents under them; the last column is CLAUDE (central.md, exchanges.md).

```diff
--- old
+++ new
@@ -1,4 +1,11 @@
-NAME   TERMINAL  PATH      SESSION  OPERATOR
-alpha  rex       ~/repo-a  running  idle
-beta   rex       ~/repo-b  running  idle
+NAME           TERMINAL  PATH      SESSION  CLAUDE
+central        rex       ~         running  supervisor idle
+├─ alpha       rex       ~/repo-a  running  operator idle
+│  ├─ t-done                                done <age>
+│  ├─ t-ask                                 blocked <age>
+│  ├─ t-boom                                error <age>
+│  ├─ t-busy                                working <age>
+│  └─ t-chat                                idle <age>
+└─ beta        rex       ~/repo-b  running  operator idle
+   └─ t-nogit                               done <age>
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
+   ├─ t-ask                                blocked <age>
+   ├─ t-busy                               working <age>
+   ├─ t-chat                               done <age>
+   └─ t-path                               idle <age>
 --- exit 0
```

## ls-after-rm (rex)

The same tree as ls, after t-done and t-boom were closed and beta removed.

```diff
--- old
+++ new
@@ -1,3 +1,8 @@
-NAME   TERMINAL  PATH      SESSION  OPERATOR
-alpha  rex       ~/repo-a  running  idle
+NAME          TERMINAL  PATH      SESSION  CLAUDE
+central       rex       ~         running  supervisor idle
+└─ alpha      rex       ~/repo-a  running  operator idle
+   ├─ t-ask                                blocked <age>
+   ├─ t-busy                               working <age>
+   ├─ t-chat                               done <age>
+   └─ t-path                               idle <age>
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

## tail-chat (rex)

Any diff. As tail-chat: 0.4 typed the agent's command line into a shell, so it shows up on the agent's screen; 0.5 starts it as the pane's own command (rust.md addendum: any shell). In Rex the typed line wraps at 80 columns, and where depends on the sandbox's path, so no one diff fits.

## version

The Rust cbar knows its own version (VERSION is 0.5.2); 0.4 only read ~/.crossbar/version.

```diff
--- old
+++ new
@@ -1,2 +1,2 @@
-unknown (not installed by install)
+0.5.2 (not installed by install)
 --- exit 0
```

## watch

Two changes, both wanted. The addendum "cbar watch at startup": a watch prints what stands when it starts (blocked and error, and a done no earlier watch printed), where 0.4 printed nothing at startup. And a `closed` event carries the report's last message (`t-done: closed — finished the thing`), so whoever reads the event knows what the closed agent last said without a second look; the user asked to keep it.

```diff
--- old
+++ new
@@ -1,3 +1,7 @@
+t-ask: blocked (permission) — allow npm test?
+t-boom: error — tests failed
+t-chat: done — did: again
+t-done: done — finished the thing
 t-chat: done — did: ping
-t-done: closed
+t-done: closed — finished the thing
 --- exit watch ran
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
