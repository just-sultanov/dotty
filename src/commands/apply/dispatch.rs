use crate::error::DottyError;

use crate::config::write_config;
use crate::git;
use crate::paths::home_dir;
use crate::plan;
use crate::platform::detect_platform;
use crate::repo_state::RepoState;

use super::machine::resolve_machine;
use super::managed::{add_dir_entry, rebuild_managed_map};
use super::plan_builder::{ApplyPlanInput, build_apply_plan};
use super::summary::print_per_file_summary;
use super::tiers::{build_override_map, compute_dir_owners, merge_tiers};

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
    let config_path = repo.config_path.clone();
    let backups_path = repo.backups_path.clone();

    // Read config (machine + managed map)
    let mut config = repo.config.clone();

    // 1. Detect platform and resolve machine
    let platform = platform_override.or_else(detect_platform);
    let machine_name = resolve_machine(
        &repo_path,
        &mut config,
        &repo.config_path,
        dry_run,
        &platform,
    )?;

    // 2. Collect all tracked files from git
    let tracked_files: Vec<String> = git::TrackedFiles::new(&repo_path)?.collect();

    // 3. Classify files by tier and merge by priority
    let merged = merge_tiers(&tracked_files, &machine_name, &platform);

    // 4. Build override map: target_path → lower tier that was overridden
    let override_map = build_override_map(&tracked_files, &Some(machine_name.clone()), &platform);

    // 4b. Compute dir-owners: directories fully owned by one tier
    let home = home_dir()?;
    let dir_owners = compute_dir_owners(&merged, &home);

    // 4c. Rebuild managed map from tracked files so orphan detection has
    //     a complete view of currently tracked files (prevents false
    //     orphans on first apply when config.managed is empty/stale).
    let new_managed = rebuild_managed_map(&tracked_files);
    config.managed = new_managed;

    // 5. Build the plan (pure function — no git/config I/O)
    let input = ApplyPlanInput {
        repo_path: repo_path.clone(),
        backups_path: backups_path.clone(),
        home,
        merged,
        override_map,
        dir_owners,
        config: config.clone(),
        force,
        follow_symlinks,
    };
    let output = build_apply_plan(&input)?;

    // 6. Execute plan
    let mode = if dry_run {
        plan::ExecuteMode::DryRun
    } else {
        plan::ExecuteMode::Normal
    };
    plan::execute_plan(&output.plan, mode, &mut repo)?;

    // 7. Print per-file summary
    print_per_file_summary(
        &output.file_results,
        &output.dir_results,
        &output.orphans,
        dry_run,
        !output.plan.actions.is_empty(),
    );

    // 7b. Add dir-entries to config.managed for applied dir-symlinks.
    if !dry_run {
        for dr in &output.dir_results {
            if dr.applied
                && let Some(owner) = input.dir_owners.get(&dr.target_dir)
            {
                add_dir_entry(&mut config.managed, &owner.repo_dir, &dr.target_dir);
            }
        }
    }

    // 8. Write updated config (managed map was rebuilt in step 4b).
    if !dry_run {
        write_config(&config_path, &config)?;
    }

    Ok(())
}
