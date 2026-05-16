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
mod symbols;
mod symlink;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, ConfigCommands};
use log::Verbosity;

fn main() -> Result<()> {
    let cli = Cli::parse();

    let verbosity = Verbosity::from_flags(cli.is_verbose(), cli.is_quiet());
    log::init(verbosity);

    // Check for a pending plan from a previously interrupted operation
    if !cli.skip_recovery()
        && let Err(e) = check_pending_plan()
    {
        eprintln!("Warning: pending plan check failed: {e}");
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
        } => commands::add::run(path, machine, platform, commit, dry_run)?,
        Commands::Remove {
            path,
            machine,
            commit,
            dry_run,
        } => commands::remove::run(path, machine, commit, dry_run)?,
        Commands::Apply { dry_run, platform } => commands::apply::run(dry_run, platform)?,
        Commands::Status => commands::status::run()?,
        Commands::Clean { keep, before, yes } => commands::clean::run(keep, before, yes)?,
    }

    Ok(())
}

/// Check for a pending plan from a previously interrupted operation.
///
/// If found, offers the user to rollback, discard, or abort the current command.
fn check_pending_plan() -> Result<()> {
    let state_path = paths::resolve_state_path()?;
    let pending = plan::load_pending_plan(&state_path)?;

    let Some(plan) = pending else {
        return Ok(()); // No pending plan
    };

    println!(
        "Found a pending plan from a previously interrupted operation ({} actions).",
        plan.actions.len()
    );
    println!("Actions:");
    for (i, action) in plan.actions.iter().enumerate() {
        println!("  {}. {}", i + 1, action);
    }

    let options = ["Rollback", "Discard", "Abort"];
    let choice = prompt::prompt_select("What would you like to do?", &options)?;

    match choice {
        0 => {
            // Rollback: execute inverse actions
            println!("Rolling back pending plan...");
            // Build rollback actions in reverse
            let mut rollback_plan = plan::Plan::new(&plan.repo_path);
            for action in plan.actions.iter().rev() {
                if let Some(rollback_action) = action.rollback() {
                    rollback_plan.add(rollback_action);
                }
            }
            if !rollback_plan.is_empty() {
                plan::execute_plan(&rollback_plan, false, &state_path)?;
                println!("Rollback complete.");
            } else {
                println!("No reversible actions to rollback. Clearing pending plan.");
            }
            plan::clear_pending_plan(&state_path)?;
        }
        1 => {
            // Discard: just remove the pending plan file
            plan::clear_pending_plan(&state_path)?;
            println!("Pending plan discarded.");
        }
        2 => {
            // Abort: exit without running the current command
            anyhow::bail!(
                "Aborted. Pending plan still exists at {}.",
                state_path.display()
            );
        }
        _ => unreachable!(),
    }

    Ok(())
}
