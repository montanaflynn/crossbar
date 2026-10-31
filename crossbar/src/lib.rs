//! crossbar: a team of Claude Code agents in Rex or tmux. Exchanges each
//! have a supervisor and projects; each project has an operator, and agents
//! in tabs of its session. `cbar` drives them; `switchboard` shows them.

pub mod cli;
pub mod launch;
pub mod layout;
pub mod migrate;
pub mod mux;
pub mod paths;
pub mod status;
pub mod util;
