use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::warn;

use crate::convention::{
    self, KNOWN_PLATFORMS, backup_timestamp, expand_tilde, read_config, repo_to_target,
    resolve_repo_path, resolve_state_path, target_to_repo, walk_dir, write_config,
};
use crate::git;
use crate::plan::{self, Action, Plan};
use crate::prompt::{prompt_confirm, prompt_select};
use crate::symlink::is_symlink;

/// Run the `add` command.
pub fn run(
    path: String,
    machine: Option<String>,
    platform: Option<String>,
    commit: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let repo_path = resolve_repo_path()?;
    let state_path = resolve_state_path()?;

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Determine scope (tier directory name)
    let scope = resolve_scope(&machine, &platform)?;

    // Reject paths inside the dotty repo itself
    if target_path.starts_with(&repo_path) {
        anyhow::bail!("Cannot add files from inside the dotty repository.");
    }

    // Warn about non-standard config paths (only for base tier)
    if scope == "base" {
        warn_non_xdg(&target_path)?;
    }

    // Validate platform if specified
    if let Some(plat) = &platform
        && !KNOWN_PLATFORMS.contains(&plat.as_str())
    {
        let ok = prompt_confirm(&format!(
            "Platform '{}' is not recognized. Valid: macos, linux, freebsd. Continue?",
            plat
        ))?;
        if !ok {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Validate / create machine directory if --machine is used
    if machine.is_some() {
        let machine_dir = repo_path.join(&scope);
        if !machine_dir.exists() {
            let ok = prompt_confirm(&format!(
                "Machine '{}' not found in repo. Create directory?",
                scope
            ))?;
            if !ok {
                println!("Aborted.");
                return Ok(());
            }
        }
    }

    // Collect all files to add (recursively for directories)
    let files_to_add = collect_files(&target_path)?;
    if files_to_add.is_empty() {
        anyhow::bail!("No files found at path: {}", target_path.display());
    }

    // Build conflict map from existing tracked files
    let existing_files = if repo_path.join(".git").exists() {
        match git::git_ls_files(&repo_path) {
            Ok(files) => files,
            Err(e) => {
                warn!("failed to list tracked files: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let conflict_map = build_conflict_map(&existing_files);

    // Resolve conflicts interactively
    let files_to_override = resolve_conflicts(&files_to_add, &conflict_map)?;

    // Build the plan
    let mut plan = Plan::new("add", &repo_path);

    // Capture current branch
    if repo_path.join(".git").exists()
        && let Ok(branch) = git::git_current_branch(&repo_path)
    {
        plan.branch = branch;
    }

    // Read current config (to update managed map)
    let mut config = read_config(&state_path)?;

    // Backup timestamp
    let backup_timestamp = backup_timestamp();
    let backup_base = state_path.join("backups").join(&backup_timestamp);

    let home = convention::home_dir()?;

    // Collect repo-relative paths for git add alongside plan building
    let mut git_add_paths: Vec<PathBuf> = Vec::new();

    for target_file in &files_to_override {
        // Compute repo-relative path (without scope prefix)
        let rel_path = target_to_repo(target_file)?;
        let repo_file = repo_path.join(&scope).join(&rel_path);

        // Check if file already exists in this tier
        if repo_file.exists() {
            let ok = prompt_confirm(&format!(
                "File already exists in repo at {}/{}. Override?",
                scope,
                rel_path.display()
            ))?;
            if !ok {
                continue;
            }
        }

        // Create parent directories in repo
        if let Some(parent) = repo_file.parent() {
            plan.add(Action::CreateDir {
                path: parent.to_path_buf(),
            });
        }

        // Backup original file if it exists at target
        if target_file.exists() {
            let backup_dest = if let Ok(relative) = target_file.strip_prefix(&home) {
                backup_base.join(relative)
            } else {
                backup_base.join(target_file.file_name().unwrap_or_default())
            };
            plan.add(Action::Backup {
                source: target_file.clone(),
                dest: backup_dest,
            });
        }

        // Copy file to repo (dereference symlinks)
        plan.add(Action::CopyFile {
            source: target_file.clone(),
            dest: repo_file.clone(),
        });

        // Create symlink at target location pointing to repo file
        plan.add(Action::CreateSymlink {
            target: repo_file.clone(),
            link: target_file.clone(),
        });

        // Track path for git add
        if let Ok(rel) = repo_file.strip_prefix(&repo_path) {
            git_add_paths.push(rel.to_path_buf());
        }

        // Update managed map
        let repo_rel = repo_file
            .strip_prefix(&repo_path)
            .map_err(|_| {
                anyhow::anyhow!(
                    "Repo file {} is not inside the repository at {}",
                    repo_file.display(),
                    repo_path.display()
                )
            })?
            .to_string_lossy()
            .to_string();
        let target_rel = target_file
            .strip_prefix(&home)
            .map(|p| format!("~/{p}", p = p.display()))
            .unwrap_or_else(|_| target_file.to_string_lossy().to_string());
        config.managed.insert(repo_rel, target_rel);
    }

    // Git add (stage the copied files)
    if !git_add_paths.is_empty() && repo_path.join(".git").exists() {
        plan.add(Action::GitAdd {
            paths: git_add_paths,
        });
    }

    // Git commit (if --commit specified)
    if let Some(msg) = &commit {
        plan.add(Action::GitCommit {
            message: msg.clone(),
        });
    }

    // Execute the plan
    plan::execute_plan(&plan, dry_run)?;

    // Write updated config only after successful plan execution.
    if !dry_run && !plan.is_empty() {
        write_config(&state_path, &config)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the scope (tier directory name) from --machine / --platform flags.
///
/// Priority: --machine > --platform > "base" (default).
fn resolve_scope(machine: &Option<String>, platform: &Option<String>) -> Result<String> {
    if let Some(name) = machine {
        Ok(name.clone())
    } else if let Some(name) = platform {
        Ok(name.clone())
    } else {
        Ok("base".to_string())
    }
}

/// Warn if the path doesn't look like a standard config location.
///
/// A path is considered "standard" if it's under `~/.config/`, `~/.local/`,
/// `~/.ssh/`, or is a dotfile (starts with `.` but not `..`).
///
/// Also warns if the path is under a sensitive system directory
/// (`/etc/`, `/usr/`, `/sys/`, `/proc/`).
fn warn_non_xdg(target_path: &Path) -> Result<()> {
    let home = convention::home_dir()?;
    let relative = target_path.strip_prefix(&home).unwrap_or(target_path);
    let rel_str = relative.to_string_lossy();

    let is_standard = rel_str.starts_with(".config/")
        || rel_str.starts_with(".local/")
        || rel_str.starts_with(".ssh/")
        || (rel_str.starts_with('.') && !rel_str.starts_with("..")); // dotfile, not `..`

    if !is_standard {
        println!(
            "Warning: '{}' doesn't look like a standard config location.",
            target_path.display()
        );
        let ok = prompt_confirm(
            "Add to a specific machine or platform instead? (no = continue to base)",
        )?;
        if ok {
            anyhow::bail!(
                "Aborted. Re-run with --machine <name> or --platform <name> to target a specific tier."
            );
        }
    }

    // Warn on sensitive system paths
    let sensitive_prefixes = ["/etc", "/usr", "/sys", "/proc"];
    let path_str = target_path.to_string_lossy();
    if sensitive_prefixes
        .iter()
        .any(|&prefix| path_str == prefix || path_str.starts_with(&format!("{}/", prefix)))
    {
        println!(
            "Warning: '{}' is under a sensitive system directory.",
            target_path.display()
        );
        let ok = prompt_confirm("Proceed anyway?")?;
        if !ok {
            anyhow::bail!("Aborted.");
        }
    }

    Ok(())
}

/// Collect all files under the given path.
fn collect_files(target_path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if target_path.is_file() || is_symlink(target_path) {
        files.push(target_path.to_path_buf());
    } else if target_path.is_dir() {
        walk_dir(target_path, &mut files)?;
    } else {
        anyhow::bail!("Path does not exist: {}", target_path.display());
    }

    Ok(files)
}

/// Build a map from target path → list of repo-relative paths that manage it.
fn build_conflict_map(existing_files: &[String]) -> HashMap<PathBuf, Vec<String>> {
    let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for repo_rel in existing_files {
        let repo_path = PathBuf::from(repo_rel);
        if let Ok(target) = repo_to_target(&repo_path) {
            map.entry(target).or_default().push(repo_rel.clone());
        }
    }

    map
}

/// Resolve conflicts for the files being added.
///
/// Returns the subset of files that should proceed (after user confirmation).
fn resolve_conflicts(
    files_to_add: &[PathBuf],
    conflict_map: &HashMap<PathBuf, Vec<String>>,
) -> Result<Vec<PathBuf>> {
    let mut conflicting: Vec<(&PathBuf, &Vec<String>)> = Vec::new();

    for file in files_to_add {
        if let Some(existing) = conflict_map.get(file)
            && !existing.is_empty()
        {
            conflicting.push((file, existing));
        }
    }

    if conflicting.is_empty() {
        // No conflicts — all files can be added
        return Ok(files_to_add.to_vec());
    }

    // Show conflict summary
    println!("\nConflicts detected:");
    for (target, repos) in &conflicting {
        println!("  {} is already managed via:", target.display());
        for repo in *repos {
            println!("    {}", repo);
        }
    }

    let options = vec!["Ask per-file", "Override all", "Cancel"];
    let choice = prompt_select("How to resolve?", &options)?;

    let mut result = Vec::new();

    match choice {
        0 => {
            // Ask per-file for conflicting ones
            for (target, _repos) in &conflicting {
                let ok = prompt_confirm(&format!(
                    "Override {} (already managed by another tier)?",
                    target.display()
                ))?;
                if ok {
                    result.push((*target).to_path_buf());
                }
            }
            // Always include non-conflicting files
            for file in files_to_add {
                if !conflict_map.contains_key(file) {
                    result.push(file.clone());
                }
            }
        }
        1 => {
            // Override all
            result = files_to_add.to_vec();
        }
        2 => {
            println!("Aborted.");
            return Ok(Vec::new());
        }
        _ => unreachable!(),
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home() {
        let path = convention::expand_tilde("~/.vimrc").unwrap();
        assert!(path.to_string_lossy().ends_with(".vimrc"));
    }

    #[test]
    fn test_expand_tilde_nested() {
        let path = convention::expand_tilde("~/.config/nvim/init.lua").unwrap();
        assert!(path.to_string_lossy().ends_with(".config/nvim/init.lua"));
    }

    #[test]
    fn test_expand_tilde_absolute() {
        let path = convention::expand_tilde("/opt/nvim/appimage").unwrap();
        assert_eq!(path, PathBuf::from("/opt/nvim/appimage"));
    }

    #[test]
    fn test_resolve_scope_machine() {
        let scope = resolve_scope(&Some("macbook".into()), &None).unwrap();
        assert_eq!(scope, "macbook");
    }

    #[test]
    fn test_resolve_scope_platform() {
        let scope = resolve_scope(&None, &Some("macos".into())).unwrap();
        assert_eq!(scope, "macos");
    }

    #[test]
    fn test_resolve_scope_default() {
        let scope = resolve_scope(&None, &None).unwrap();
        assert_eq!(scope, "base");
    }

    #[test]
    fn test_resolve_scope_machine_over_platform() {
        let scope = resolve_scope(&Some("macbook".into()), &Some("macos".into())).unwrap();
        assert_eq!(scope, "macbook");
    }

    #[test]
    fn test_build_conflict_map() {
        let existing = vec![
            "base/home/.vimrc".into(),
            "base/home/.gitconfig".into(),
            "macbook/home/.config/nvim/plugins.lua".into(),
        ];
        let map = build_conflict_map(&existing);

        let home = convention::home_dir().unwrap();
        assert!(map.contains_key(&home.join(".vimrc")));
        assert!(map.contains_key(&home.join(".gitconfig")));
        assert!(map.contains_key(&home.join(".config/nvim/plugins.lua")));
    }

    #[test]
    fn test_conflict_map_empty() {
        let map = build_conflict_map(&[]);
        assert!(map.is_empty());
    }
}
