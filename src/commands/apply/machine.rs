use std::path::Path;

use anyhow::Result;

use crate::convention::{scan_machine_directories, write_config};
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
    write_config(state_path, config)?;
    Ok(name)
}
