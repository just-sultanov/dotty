use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::warn;

use crate::config::Config;
use crate::convention::{
    self, KNOWN_PLATFORMS, backup_timestamp, expand_tilde, repo_to_target, target_to_repo,
    walk_dir, write_config,
};
use crate::git;
use crate::plan::{self, Action, Plan};
use crate::prompt::{prompt_confirm, prompt_select};
use crate::repo_state::RepoState;
use crate::symlink::is_symlink;

/// Run the `add` command.
pub fn run(
    path: String,
    machine: Option<String>,
    platform: Option<String>,
    commit: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let repo = RepoState::new().map_err(|e| anyhow::anyhow!("{e}"))?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Determine scope (tier directory name)
    let scope = resolve_scope(&machine, &platform)?;

    // Reject paths inside the dotty repo itself.
    // Canonicalize both paths to prevent path traversal via `..` components.
    let canonical_repo = fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.clone());
    if let Ok(canonical_target) = fs::canonicalize(&target_path)
        && canonical_target.starts_with(&canonical_repo)
    {
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
    let existing_files = if repo.is_git_repo {
        match git::git_ls_files(repo_path) {
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

    let home = convention::home_dir()?;
    let has_git = repo.is_git_repo;
    let config = repo.config;

    // Build the plan (pure function — no side effects)
    let input = AddPlanInput {
        repo_path: repo_path.clone(),
        state_path: state_path.clone(),
        home,
        scope,
        files_to_add: files_to_override,
        commit: commit.clone(),
        has_git,
    };
    let output = build_add_plan(&input, &config)?;

    // Execute the plan
    plan::execute_plan(&output.plan, dry_run, state_path)?;

    // Write updated config only after successful plan execution.
    if !dry_run && !output.plan.is_empty() {
        write_config(state_path, &output.config)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Plan building (pure — no I/O)
// ---------------------------------------------------------------------------

/// Input data for building an `add` plan.
///
/// All filesystem and user-interaction concerns are resolved before this
/// struct is created, so `build_add_plan` is a pure function suitable for
/// unit testing.
pub(crate) struct AddPlanInput {
    pub repo_path: PathBuf,
    pub state_path: PathBuf,
    pub home: PathBuf,
    pub scope: String,
    pub files_to_add: Vec<PathBuf>,
    pub commit: Option<String>,
    pub has_git: bool,
}

/// Output of `build_add_plan`.
pub(crate) struct AddPlanOutput {
    pub plan: Plan,
    pub config: Config,
}

/// Build a plan for adding files to the dotty repository.
///
/// This is a pure function: it takes all resolved input data and returns
/// a `Plan` with actions and an updated `Config`. No filesystem or git
/// operations are performed.
pub(crate) fn build_add_plan(input: &AddPlanInput, config: &Config) -> Result<AddPlanOutput> {
    let mut plan = Plan::new(&input.repo_path);
    let mut config = config.clone();

    // Backup timestamp
    let backup_timestamp = backup_timestamp();
    let backup_base = input.state_path.join("backups").join(&backup_timestamp);

    // Collect repo-relative paths for git add alongside plan building
    let mut git_add_paths: Vec<PathBuf> = Vec::new();

    for target_file in &input.files_to_add {
        // Compute repo-relative path (without scope prefix)
        let rel_path = target_to_repo(target_file)?;
        let repo_file = input.repo_path.join(&input.scope).join(&rel_path);

        // Create parent directories in repo
        if let Some(parent) = repo_file.parent() {
            plan.add(Action::CreateDir {
                path: parent.to_path_buf(),
            });
        }

        // Backup original file if it exists at target
        if target_file.exists() {
            let backup_dest = if let Ok(relative) = target_file.strip_prefix(&input.home) {
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
        if let Ok(rel) = repo_file.strip_prefix(&input.repo_path) {
            git_add_paths.push(rel.to_path_buf());
        }

        // Update managed map (normalize separators to `/` for cross-platform keys)
        let repo_rel =
            convention::normalize_path(repo_file.strip_prefix(&input.repo_path).map_err(|_| {
                anyhow::anyhow!(
                    "Repo file {} is not inside the repository at {}",
                    repo_file.display(),
                    input.repo_path.display()
                )
            })?);
        let target_rel = convention::format_target_display(target_file);
        config.managed.insert(repo_rel, target_rel);
    }

    // Git add (stage the copied files)
    if !git_add_paths.is_empty() && input.has_git {
        plan.add(Action::GitAdd {
            paths: git_add_paths,
        });
    }

    // Git commit (if --commit specified)
    if let Some(msg) = &input.commit {
        plan.add(Action::GitCommit {
            message: msg.clone(),
        });
    }

    Ok(AddPlanOutput { plan, config })
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

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
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
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let existing = vec![
                "base/home/.vimrc".into(),
                "base/home/.gitconfig".into(),
                "macbook/home/.config/nvim/plugins.lua".into(),
            ];
            let map = build_conflict_map(&existing);

            assert!(map.contains_key(&home.join(".vimrc")));
            assert!(map.contains_key(&home.join(".gitconfig")));
            assert!(map.contains_key(&home.join(".config/nvim/plugins.lua")));
        });
    }

    #[test]
    fn test_conflict_map_empty() {
        let map = build_conflict_map(&[]);
        assert!(map.is_empty());
    }

    // -- build_add_plan tests --

    #[test]
    fn test_build_add_plan_single_file() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        fs::write(&target, "set nocompatible").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                scope: "base".to_string(),
                files_to_add: vec![target.clone()],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            // CreateDir + Backup + CopyFile + CreateSymlink = 4 actions
            assert_eq!(output.plan.actions.len(), 4);
            assert!(output.config.managed.contains_key("base/home/.vimrc"));
        });
    }

    #[test]
    fn test_build_add_plan_with_git_commit() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let target = home.join(".gitconfig");
        fs::write(&target, "[user]").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                scope: "base".to_string(),
                files_to_add: vec![target.clone()],
                commit: Some("add gitconfig".to_string()),
                has_git: true,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            // CreateDir + Backup + CopyFile + CreateSymlink + GitAdd + GitCommit = 6
            assert_eq!(output.plan.actions.len(), 6);

            match &output.plan.actions.last().unwrap() {
                Action::GitCommit { message } => assert_eq!(message, "add gitconfig"),
                other => panic!("expected GitCommit, got: {other:?}"),
            }
        });
    }

    #[test]
    fn test_build_add_plan_multiple_files() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let f1 = home.join(".vimrc");
        let f2 = home.join(".gitconfig");
        fs::write(&f1, "vim").unwrap();
        fs::write(&f2, "git").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                scope: "base".to_string(),
                files_to_add: vec![f1.clone(), f2.clone()],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            // 2 files × (CreateDir + Backup + CopyFile + CreateSymlink) = 8
            assert_eq!(output.plan.actions.len(), 8);
            assert_eq!(output.config.managed.len(), 2);
        });
    }

    #[test]
    fn test_build_add_plan_nested_path() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();

        let target = home.join(".config/nvim/init.lua");
        fs::write(&target, "vim.g.mapleader = ' '").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                scope: "macbook".to_string(),
                files_to_add: vec![target.clone()],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            assert!(
                output
                    .config
                    .managed
                    .contains_key("macbook/home/.config/nvim/init.lua"),
                "expected macbook scope in managed key"
            );
        });
    }

    #[test]
    fn test_build_add_plan_no_git_skips_git_add() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        fs::write(&target, "content").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: repo.clone(),
                state_path: state.clone(),
                home: home.clone(),
                scope: "base".to_string(),
                files_to_add: vec![target.clone()],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            for action in &output.plan.actions {
                assert!(
                    !matches!(action, Action::GitAdd { .. }),
                    "should not have GitAdd when has_git is false"
                );
            }
        });
    }
}
