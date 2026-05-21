//! Command modules for the dotty dotfiles manager.
//!
//! Each module implements a single subcommand and follows a consistent pattern:
//!
//! 1. **Resolve inputs** — parse arguments, resolve paths, detect platform/machine
//! 2. **Build plan** — a pure function (`build_*_plan`) that constructs a `Plan`
//!    from resolved inputs without performing any side effects
//! 3. **Execute plan** — calls `plan::execute_plan()` to apply actions
//! 4. **Persist state** — write updated config, git stage/commit
//!
//! The `apply` command is the most complex, using a multi-module structure:
//! `dispatch` (entry point), `tiers` (merge/override logic),
//! `inspect` (filesystem state detection), `plan_builder` (plan construction),
//! `orphan_detection` (stale entry cleanup), and `summary` (console output).

pub mod add;
pub mod apply;
pub mod clean;
pub mod config;
pub mod init;
pub mod remove;
pub mod status;
