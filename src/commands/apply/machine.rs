use std::path::Path;

use anyhow::Result;

use crate::config::write_config;
use crate::convention::scan_machine_directories;
use crate::prompt::prompt_machine_selection;

/// Resolve the machine name. If missing from config, prompt user to select.
pub(crate) fn resolve_machine(
    repo_path: &Path,
    config: &mut crate::config::Config,
    state_path: &Path,
    dry_run: bool,
    _platform: &Option<String>,
) -> Result<String> {
    if let Some(name) = &config.machine {
        return Ok(name.clone());
    }

    // No machine in config — scan repo for known machines
    let known = scan_machine_directories(repo_path);

    if dry_run {
        if known.is_empty() {
            anyhow::bail!(
                "No machine configured and no known machines in repo. \
                 Run `dotty init` or `dotty config machine <name>` first."
            );
        }
        anyhow::bail!(
            "No machine configured. Known machines in repo: {}. \
             Run `dotty config machine <name>` to select one.",
            known.join(", ")
        );
    }

    let name = prompt_machine_selection(&known)?;
    config.machine = Some(name.clone());
    // Guard against persisting machine name during dry-run: the config
    // should only be written when the apply is actually executed, not
    // when the user is only previewing what would happen.
    if !dry_run {
        write_config(state_path, config)?;
    }
    Ok(name)
}
