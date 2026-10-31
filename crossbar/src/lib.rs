//! crossbar: a team of Claude Code agents in Rex or tmux. Exchanges each
//! have a supervisor and projects; each project has an operator, and agents
//! in tabs of its session. `cbar` drives them; `switchboard` shows them.

pub mod layout;
pub mod mux;
pub mod status;
pub mod util;
