use anyhow::Result;
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
pub fn run(dry_run: bool, platform_override: Option<String>, force: bool) -> Result<()> {
    let repo = RepoState::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    repo.require_git().map_err(|e| anyhow::anyhow!("{e}"))?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Read config (machine + managed map)
    let mut config = repo.config;

    // 1. Detect platform and resolve machine
    let platform = platform_override.or_else(detect_platform);
    let machine_name = resolve_machine(repo_path, &mut config, state_path, dry_run, &platform)?;

    // 2. Collect all tracked files from git
    let tracked_files = git::git_ls_files(repo_path)?;

    // 3. Classify files by tier and merge by priority
    let merged = merge_tiers(&tracked_files, &machine_name, &platform);

    // 4. Build override map: target_path → lower tier that was overridden
    let override_map = build_override_map(&tracked_files, &Some(machine_name.clone()), &platform);

    // 5. Build the plan (pure function — no git/config I/O)
    let input = ApplyPlanInput {
        repo_path: repo_path.clone(),
        state_path: state_path.clone(),
        home: home_dir()?,
        merged,
        override_map,
        config: config.clone(),
        force,
    };
    let output = build_apply_plan(&input)?;

    // 6. Execute plan
    plan::execute_plan(&output.plan, dry_run, state_path)?;

    // 7. Print per-file summary
    print_per_file_summary(&output.file_results, &output.orphans, dry_run);

    // 8. Rebuild managed map from tracked files
    if !dry_run {
        let new_managed = rebuild_managed_map(&tracked_files);
        config.managed = new_managed;
        if let Err(e) = write_config(state_path, &config) {
            warn!("failed to write config: {e}");
        }
    }

    Ok(())
}
