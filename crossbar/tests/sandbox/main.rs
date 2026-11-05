//! cbar, the switchboards and claude-relay end to end, each test in a sandbox
//! of its own (common.rs): a private tmux server, or a private Rex server,
//! never the live one.
//!
//!   cargo test --test sandbox                  the tmux tests (skipped without tmux)
//!   cargo test --test sandbox -- --ignored     the Rex tests (they need Rex)
//!   cargo test --test sandbox -- --nocapture   with a line per check as it runs
//!
//! The tmux tests run in parallel, one sandbox each; the Rex tests take turns.
//! A failed test prints each check's line and lists the failed ones.

mod board;
mod common;
mod restart;
mod watch;
