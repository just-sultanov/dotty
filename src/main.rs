//! Dotty — a minimal dotfiles manager for multiple machines.
//!
//! Config files are organized by priority tiers (`base/`, `<platform>/`, `<machine>/`)
//! and linked to their real locations via file-level symlinks.

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
        check_pending_plan(cli.recovery_action())?;
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

/// Handle a stale (invalid) pending plan during recovery.
///
/// Called when `load_pending_plan` returns `PendingPlanInvalid`, indicating
/// the plan references a repository that no longer exists or is not a valid
/// git repository.
///
/// Prompts the user to discard the stale plan or ignore it (leave it on disk).
/// When `recovery_action` is provided, the choice is made automatically.
fn handle_stale_plan(
    state_path: &std::path::Path,
    reason: &str,
    recovery_action: Option<&str>,
) -> Result<()> {
    eprintln!("Pending plan is invalid: {reason}");
    let choice = match recovery_action {
        Some("discard") => 0,
        Some("ignore") => 1,
        Some(other) => {
            anyhow::bail!(
                "invalid recovery action '{}'. Must be one of: rollback, discard, ignore",
                other
            )
        }
        None => {
            let options = ["Discard", "Ignore"];
            prompt::prompt_select("What would you like to do?", &options)?
        }
    };
    match choice {
        0 => {
            // Discard the stale plan file
            if let Err(e) = plan::clear_pending_plan(state_path) {
                eprintln!("Warning: could not discard stale plan: {e}");
            } else {
                println!("Stale pending plan discarded.");
            }
        }
        1 => {
            // Ignore: leave the file as-is
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Handle a valid pending plan during recovery.
///
/// Called when `load_pending_plan` returns `Some(plan)`, indicating the
/// plan is still valid (repo exists and is a git repository).
///
/// Presents the user with rollback, discard, or abort options.
/// When `recovery_action` is provided, the choice is made automatically.
fn handle_valid_plan(
    plan: &plan::Plan,
    state_path: &std::path::Path,
    recovery_action: Option<&str>,
) -> Result<()> {
    println!(
        "Found a pending plan from a previously interrupted operation ({} actions).",
        plan.actions.len()
    );
    println!("Actions:");
    for (i, action) in plan.actions.iter().enumerate() {
        println!("  {}. {}", i + 1, action);
    }

    let options = ["Rollback", "Discard", "Abort"];
    let choice = match recovery_action {
        Some("rollback") => 0,
        Some("discard") => 1,
        Some("ignore") => 2,
        Some(other) => {
            anyhow::bail!(
                "invalid recovery action '{}'. Must be one of: rollback, discard, ignore",
                other
            )
        }
        None => prompt::prompt_select("What would you like to do?", &options)?,
    };

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
                plan::execute_plan(&rollback_plan, false, state_path)?;
                println!("Rollback complete.");
            } else {
                println!("No reversible actions to rollback. Clearing pending plan.");
            }
            plan::clear_pending_plan(state_path)?;
        }
        1 => {
            // Discard: just remove the pending plan file
            plan::clear_pending_plan(state_path)?;
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

/// Recover from a previously interrupted dotty operation.
///
/// ## Recovery Flow
///
/// 1. Load the pending plan from the state directory.
/// 2. If the plan is **stale** (repo missing or not a git repo), call
///    [`handle_stale_plan`] to offer discard or ignore.
/// 3. If the plan is **valid**, call [`handle_valid_plan`] to offer
///    rollback (execute inverse actions), discard (remove plan file),
///    or abort (exit without running the current command).
/// 4. If no pending plan exists, return immediately.
///
/// When `recovery_action` is provided (via `--recovery-action`), the
/// corresponding choice is executed automatically without prompting,
/// enabling non-interactive recovery in scripts or CI.
fn check_pending_plan(recovery_action: Option<&str>) -> Result<()> {
    let state_path = paths::resolve_state_path()?;
    let pending = match plan::load_pending_plan(&state_path) {
        Ok(p) => p,
        Err(error::DottyError::PendingPlanInvalid { reason, .. }) => {
            return handle_stale_plan(&state_path, &reason, recovery_action);
        }
        Err(e) => return Err(e.into()),
    };

    let Some(plan) = pending else {
        return Ok(()); // No pending plan
    };

    handle_valid_plan(&plan, &state_path, recovery_action)
}

// ---------------------------------------------------------------------------
// Tests for extracted recovery functions
// ---------------------------------------------------------------------------

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::path::PathBuf;

    /// Create a temp dir with a .git subdirectory (valid git repo) and return
    /// the state path inside it.
    fn setup_state_with_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        // Create .git so the repo validates
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        (dir, state)
    }

    /// Save a simple plan to the state path so it can be loaded.
    fn save_dummy_plan(state_path: &PathBuf, repo_path: &PathBuf) {
        let mut plan = plan::Plan::new(repo_path);
        plan.add(plan::Action::CreateDir {
            path: repo_path.join("test_dir"),
        });
        plan::save_pending_plan(&plan, state_path).unwrap();
    }

    // ── handle_stale_plan tests ──

    #[test]
    fn test_handle_stale_plan_discard_removes_file() {
        let (_dir, state) = setup_state_with_repo();
        // Write a plan whose repo_path points to a non-existent or invalid repo.
        // We create a plan for a repo that doesn't exist, so load_pending_plan
        // will return PendingPlanInvalid.
        let fake_repo = PathBuf::from("/tmp/does_not_exist_dotty_repo");
        let mut plan = plan::Plan::new(&fake_repo);
        plan.add(plan::Action::CreateDir {
            path: fake_repo.join("dir"),
        });
        let pending = crate::plan::PendingPlan::from_plan(&plan);
        let content = serde_json::to_string_pretty(&pending).unwrap();
        std::fs::write(state.join("pending_plan.json"), content).unwrap();

        assert!(state.join("pending_plan.json").exists());

        // Simulate what happens when load_pending_plan returns PendingPlanInvalid:
        // the reason will say the repo doesn't exist.
        let result = handle_stale_plan(
            &state,
            "repository no longer exists at /tmp/does_not_exist_dotty_repo",
            Some("discard"),
        );
        assert!(result.is_ok());
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_handle_stale_plan_ignore_keeps_file() {
        let (_dir, state) = setup_state_with_repo();
        let fake_repo = PathBuf::from("/tmp/does_not_exist_dotty_repo_2");
        let mut plan = plan::Plan::new(&fake_repo);
        plan.add(plan::Action::CreateDir {
            path: fake_repo.join("dir"),
        });
        let pending = crate::plan::PendingPlan::from_plan(&plan);
        let content = serde_json::to_string_pretty(&pending).unwrap();
        std::fs::write(state.join("pending_plan.json"), content).unwrap();

        let result = handle_stale_plan(
            &state,
            "repository no longer exists at /tmp/does_not_exist_dotty_repo_2",
            Some("ignore"),
        );
        assert!(result.is_ok());
        assert!(state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_handle_stale_plan_invalid_recovery_action() {
        let (_dir, state) = setup_state_with_repo();
        let result = handle_stale_plan(&state, "some reason", Some("rollback"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("rollback"));
        assert!(err.contains("Must be one of: rollback, discard, ignore"));
    }

    // ── handle_valid_plan tests ──

    #[test]
    fn test_handle_valid_plan_discard_removes_file() {
        let (_dir, state) = setup_state_with_repo();
        let repo = PathBuf::from(".");
        save_dummy_plan(&state, &repo);

        let plan = plan::load_pending_plan(&state).unwrap().unwrap();
        assert!(state.join("pending_plan.json").exists());

        let result = handle_valid_plan(&plan, &state, Some("discard"));
        assert!(result.is_ok());
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_handle_valid_plan_rollback_executes_inverse() {
        let (_dir, state) = setup_state_with_repo();
        let repo = PathBuf::from(".");
        save_dummy_plan(&state, &repo);

        let plan = plan::load_pending_plan(&state).unwrap().unwrap();

        let result = handle_valid_plan(&plan, &state, Some("rollback"));
        assert!(result.is_ok());
        // After rollback, the created dir should be removed
        assert!(!repo.join("test_dir").exists());
        // Pending plan should be cleared
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_handle_valid_plan_abort_returns_error() {
        let (_dir, state) = setup_state_with_repo();
        let repo = PathBuf::from(".");
        save_dummy_plan(&state, &repo);

        let plan = plan::load_pending_plan(&state).unwrap().unwrap();

        let result = handle_valid_plan(&plan, &state, Some("ignore"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Aborted"));
        assert!(err.contains("Pending plan still exists"));
    }

    #[test]
    fn test_handle_valid_plan_invalid_recovery_action() {
        let (_dir, state) = setup_state_with_repo();
        let repo = PathBuf::from(".");
        save_dummy_plan(&state, &repo);

        let plan = plan::load_pending_plan(&state).unwrap().unwrap();

        let result = handle_valid_plan(&plan, &state, Some("invalid"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid"));
        assert!(err.contains("Must be one of: rollback, discard, ignore"));
    }

    // ── Integration: check_pending_plan dispatch ──

    #[test]
    fn test_check_pending_plan_delegates_to_stale_handler() {
        // Create a state dir with a stale plan (repo doesn't exist)
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();

        let fake_repo = PathBuf::from("/tmp/nonexistent_dotty_repo");
        let mut plan = plan::Plan::new(&fake_repo);
        plan.add(plan::Action::CreateDir {
            path: fake_repo.join("dir"),
        });
        let pending = crate::plan::PendingPlan::from_plan(&plan);
        let content = serde_json::to_string_pretty(&pending).unwrap();
        std::fs::write(state.join("pending_plan.json"), content).unwrap();

        // Temporarily override DOTTY_STATE_HOME so check_pending_plan uses our state dir
        let result = temp_env::with_var("DOTTY_STATE_HOME", Some(state.to_str().unwrap()), || {
            check_pending_plan(Some("discard"))
        });
        assert!(result.is_ok());
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_check_pending_plan_delegates_to_valid_handler() {
        let (_dir, state) = setup_state_with_repo();
        let repo = PathBuf::from(".");
        save_dummy_plan(&state, &repo);

        let result = temp_env::with_var("DOTTY_STATE_HOME", Some(state.to_str().unwrap()), || {
            check_pending_plan(Some("discard"))
        });
        assert!(result.is_ok());
        assert!(!state.join("pending_plan.json").exists());
    }

    #[test]
    fn test_check_pending_plan_no_pending_plan_returns_ok() {
        let (_dir, state) = setup_state_with_repo();

        // No pending plan file exists
        let result = temp_env::with_var("DOTTY_STATE_HOME", Some(state.to_str().unwrap()), || {
            check_pending_plan(None)
        });
        assert!(result.is_ok());
    }
}
