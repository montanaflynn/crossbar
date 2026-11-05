//! crossbar: a team of Claude Code agents in Rex or tmux.
//!
//! - An exchange is the top level, a group of projects. It has a supervisor
//!   and an exchange switchboard, in the session named after it.
//! - A project is a folder or repo in an exchange (not an exchange of its
//!   own). It has an operator and a project switchboard, in its session
//!   `<exchange>/<project>`.
//! - An agent does one task in a tab of a project's session or of the
//!   exchange's. It has no switchboard.
//!
//! Each is named by its path, `exchange/project/agent`, and has its briefs
//! and reports. `cbar` drives them; `switchboard` shows them.

#![allow(clippy::too_many_arguments)]

pub mod board;
pub mod cli;
pub mod launch;
pub mod layout;
pub mod mux;
pub mod paths;
pub mod restart;
pub mod status;
pub mod util;
pub mod watch;
