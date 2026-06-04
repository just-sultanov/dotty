use crate::error::DottyError;
use crate::prompt;

use tracing::warn;

use crate::config::write_config;
use crate::git;
use crate::paths::home_dir;
use crate::plan;
use crate::platform::detect_platform;
use crate::repo_state::RepoState;

use super::machine::resolve_machine;
use super::managed::rebuild_managed_map;
use super::plan_builder::{ApplyPlanInput, build_apply_plan};
use super::summary::print_per_file_summary;
use super::tiers::{build_override_map, merge_tiers};

/// Run the `apply` command.
///
/// This is a thin dispatcher that delegates to focused sub-modules:
/// 1. Resolve machine name (prompt if missing)
/// 2. Collect tracked files from git
/// 3. Classify and merge tiers by priority
/// 4. Build override map
/// 5. Build apply plan (pure function)
/// 6. Execute plan
/// 7. Print summary
/// 8. Rebuild managed map in config
pub fn run(
    dry_run: bool,
    platform_override: Option<String>,
    force: bool,
    follow_symlinks: bool,
) -> Result<(), DottyError> {
    let mut repo = RepoState::new()?;
    repo.require_git()?;

    let repo_path = repo.repo_path.clone();
    let state_path = repo.state_path.clone();

    // Read config (machine + managed map)
    let mut config = repo.config.clone();

    // 1. Detect platform and resolve machine
    let platform = platform_override.or_else(detect_platform);
    let machine_name = resolve_machine(&repo_path, &mut config, &state_path, dry_run, &platform)?;

    // 2. Collect all tracked files from git
    let tracked_files: Vec<String> = git::TrackedFiles::new(&repo_path)?.collect();

    // 3. Classify files by tier and merge by priority
    let merged = merge_tiers(&tracked_files, &machine_name, &platform);

    // 4. Build override map: target_path → lower tier that was overridden
    let override_map = build_override_map(&tracked_files, &Some(machine_name.clone()), &platform);

    // 4b. Rebuild managed map from tracked files so orphan detection has
    //     a complete view of currently tracked files (prevents false
    //     orphans on first apply when config.managed is empty/stale).
    let new_managed = rebuild_managed_map(&tracked_files);
    config.managed = new_managed;

    // 5. Build the plan (pure function — no git/config I/O)
    let input = ApplyPlanInput {
        repo_path: repo_path.clone(),
        state_path: state_path.clone(),
        home: home_dir()?,
        merged,
        override_map,
        config: config.clone(),
        force,
        follow_symlinks,
    };
    let output = build_apply_plan(&input);

    // 5b. Orphan confirmation: prompt user before removing orphans.
    //     If --force is set, skip prompting and proceed with removal.
    //     If not interactive (CI, pipes), skip orphan removal silently.
    let mut plan = output.plan;
    if !output.orphans.is_empty() && !force {
        let confirmed = prompt::prompt_orphan_removal(&output.orphans)?;
        if confirmed {
            for action in output.orphan_removal_actions {
                plan.add(action);
            }
        } else {
            warn!(
                "orphan removal skipped by user ({} orphan(s) not removed)",
                output.orphans.len()
            );
        }
    } else if !output.orphans.is_empty() && force {
        // --force: remove orphans without prompting
        for action in output.orphan_removal_actions {
            plan.add(action);
        }
    }

    // 6. Execute plan
    let mode = if dry_run {
        plan::ExecuteMode::DryRun
    } else {
        plan::ExecuteMode::Normal
    };
    plan::execute_plan(&plan, mode, &mut repo)?;

    // 7. Print per-file summary
    print_per_file_summary(&output.file_results, &output.orphans, dry_run);

    // 8. Write updated config (managed map was rebuilt in step 4b).
    //
    // Config write failure is non-fatal: the apply itself succeeded.
    // We print to stderr with details and a recommendation so the user
    // knows the managed map may be stale (orphan detection will be
    // incorrect on the next apply until the config is fixed).
    if !dry_run && let Err(e) = write_config(&state_path, &config) {
        warn!("failed to write config: {e}. Your managed map may be out of sync.");
    }

    Ok(())
}
