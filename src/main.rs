//! Dotty — a minimal dotfiles manager for multiple machines.
//!
//! Config files are organized by priority tiers (`base/`, `<platform>/`, `<machine>/`)
//! and linked to their real locations via file-level symlinks.

#![forbid(unsafe_code)]

mod backups;
mod cli;
mod commands;
mod config;
mod convention;
mod error;
mod fs_utils;
mod git;
mod log;
mod paths;
mod plan;
mod platform;
mod prompt;
mod recovery;
mod repo_state;
mod symbols;
mod symlink;

#[cfg(test)]
pub mod tests;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ConfigCommands};
use log::Verbosity;

fn main() -> Result<()> {
    let cli = Cli::parse();

    let verbosity = Verbosity::from_flags(cli.is_verbose(), cli.is_quiet());
    log::init(verbosity);

    // Check for a pending plan from a previously interrupted operation
    if !cli.skip_recovery() {
        recovery::check_pending_plan(cli.recovery_action())?;
    }

    match cli.command {
        Commands::Init { git_url, machine } => commands::init::run(git_url, machine)?,
        Commands::Config { command } => match command {
            ConfigCommands::Machine { name } => commands::config::set_machine(name)?,
        },
        Commands::Add {
            path,
            machine,
            platform,
            commit,
            dry_run,
            force,
        } => commands::add::run(path, machine, platform, commit, dry_run, force)?,
        Commands::Remove {
            path,
            machine,
            platform,
            commit,
            dry_run,
        } => commands::remove::run(path, machine, platform, commit, dry_run)?,
        Commands::Apply {
            dry_run,
            platform,
            force,
            follow_symlinks,
        } => commands::apply::run(dry_run, platform, force, follow_symlinks)?,
        Commands::Status => commands::status::run()?,
        Commands::Clean { keep, before, yes } => commands::clean::run(keep, before, yes)?,
    }

    Ok(())
}
