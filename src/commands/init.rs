use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::convention::{
    read_config, resolve_repo_path, resolve_state_path, scan_machine_directories,
    validate_machine_name, write_config,
};
use crate::git::{git_clone, git_init};
use crate::prompt::prompt_machine_selection;

/// Bootstrap a new dotty repository or clone an existing one.
///
/// - Without `git_url`: creates a fresh repo (`git init`), sets up `base/home/`.
/// - With `git_url`: clones the repo, then sets up machine config.
///
/// In both cases, the machine name is either taken from the `machine` parameter
/// or prompted interactively.
pub fn run(git_url: Option<String>, machine: Option<String>) -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    let machine_name = if let Some(url) = &git_url {
        // Clone mode: clone repo, then resolve machine name
        clone_repo(url, &repo_path)?;
        if let Some(name) = machine {
            validate_machine_name(&name)?;
            name
        } else {
            prompt_machine_from_repo(&repo_path)?
        }
    } else if let Some(name) = machine {
        // Fresh repo with explicit machine name (no prompt)
        create_fresh_repo(&repo_path)?;
        validate_machine_name(&name)?;
        name
    } else {
        // Fresh repo: prompt for machine name
        create_fresh_repo(&repo_path)?;
        prompt_machine_name()?
    };

    // Save machine name to config
    ensure_state_dir(&state_path)?;
    let mut config = read_config(&state_path)?;
    config.set_machine(machine_name.clone());
    write_config(&state_path, &config)?;

    println!("Machine set to: {}", machine_name);
    println!("Repo: {}", repo_path.display());
    Ok(())
}

/// Create a fresh repository: `git init` + `base/home/`.
fn create_fresh_repo(repo_path: &Path) -> Result<()> {
    // If repo already exists and is a git repo, inform user
    if repo_path.exists() && repo_path.join(".git").exists() {
        println!("Repo already exists at {}", repo_path.display());
        return Ok(());
    }

    fs::create_dir_all(repo_path)?;
    git_init(repo_path)?;

    // Create base/home/ directory
    let base_home = repo_path.join("base").join("home");
    fs::create_dir_all(&base_home)?;

    println!("Created fresh repo at {}", repo_path.display());
    Ok(())
}

/// Clone a repository into the resolved path.
fn clone_repo(url: &str, repo_path: &Path) -> Result<()> {
    // Pre-check: if directory exists and is not empty, abort
    if repo_path.exists() {
        let mut entries = fs::read_dir(repo_path)?;
        if entries.next().is_some() {
            anyhow::bail!(
                "Directory {} already exists and is not empty. \
                 Remove it or choose a different path via $DOTTY_HOME.",
                repo_path.display()
            );
        }
    }

    git_clone(url, repo_path)?;
    println!("Cloned repo into {}", repo_path.display());
    Ok(())
}

/// Ensure the state directory exists.
fn ensure_state_dir(state_path: &Path) -> Result<()> {
    fs::create_dir_all(state_path)?;
    Ok(())
}

/// Prompt the user for a machine name (fresh repo mode).
fn prompt_machine_name() -> Result<String> {
    let name =
        crate::prompt::prompt_input("What is this machine called? (e.g. macbook, ubuntu-work)")?;
    validate_machine_name(&name)?;
    Ok(name)
}

/// Scan the repo for known machine directories and prompt the user to select one.
///
/// A "machine directory" is a top-level directory that:
/// - Contains a `home/` subdirectory
/// - Is not `base/`
/// - Is not a known platform (`macos/`, `linux/`, `freebsd/`)
///
/// If no known machines are found, falls back to prompting for a new name.
fn prompt_machine_from_repo(repo_path: &Path) -> Result<String> {
    let known_machines = scan_machine_directories(repo_path);
    let name = prompt_machine_selection(&known_machines)?;
    validate_machine_name(&name)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    // Tests for scan_machine_directories and validate_machine_name live in convention.rs.
    // Integration tests for init live in tests/test_init.rs.
}
