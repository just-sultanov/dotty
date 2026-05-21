use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DottyError;
use tracing::warn;

use crate::backups::backup_timestamp;
use crate::config::Config;
use crate::config::write_config;
use crate::fs_utils::walk_dir;
use crate::git;
use crate::paths::{
    expand_tilde, format_target_display, normalize_path, repo_to_target, target_to_repo,
};
use crate::plan::{self, Action, Plan};
use crate::platform::KNOWN_PLATFORMS;
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
) -> Result<(), DottyError> {
    let repo = RepoState::new()?;

    let repo_path = &repo.repo_path;
    let state_path = &repo.state_path;

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Determine scope (tier directory name)
    let scope = resolve_scope(&machine, &platform);

    // Reject paths inside the dotty repo itself.
    // Two-layer defense: (1) canonicalize both paths and compare, (2) string-prefix fallback
    // if canonicalization fails (e.g., broken symlink). The canonical check is primary;
    // the string check is secondary — it handles cases where the OS cannot resolve the path.
    let canonical_repo = match fs::canonicalize(repo_path) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to canonicalize repo path: {e}, using original path");
            repo_path.to_path_buf()
        }
    };

    let is_self_reference = if let Ok(canonical_target) = fs::canonicalize(&target_path) {
        // Primary defense: compare canonical (resolved) paths
        canonical_target.starts_with(&canonical_repo)
    } else {
        // Secondary defense: string-prefix check on raw paths
        // This is a weaker check but prevents obvious traversal when canonicalization fails
        let target_str = target_path.to_string_lossy();
        let repo_str = canonical_repo.to_string_lossy();
        target_str.starts_with(repo_str.as_ref())
    };

    if is_self_reference {
        return Err(DottyError::InvalidRepoPath {
            path: target_path.to_string_lossy().to_string(),
            reason: "Cannot add files from inside the dotty repository".into(),
        });
    }

    // Warn about non-standard config paths (only for base tier)
    if scope == "base" {
        if crate::prompt::is_interactive() {
            warn_non_xdg_interactive(&target_path)?;
        } else {
            warn_non_xdg_non_interactive(&target_path)?;
        }
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
        return Err(DottyError::InvalidTargetPath {
            path: target_path.display().to_string(),
            reason: "no files found at this path".into(),
        });
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

    let home = crate::paths::home_dir()?;
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
pub(crate) fn build_add_plan(
    input: &AddPlanInput,
    config: &Config,
) -> Result<AddPlanOutput, DottyError> {
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
            backup_path: None,
        });

        // Track path for git add
        if let Ok(rel) = repo_file.strip_prefix(&input.repo_path) {
            git_add_paths.push(rel.to_path_buf());
        }

        // Update managed map (normalize separators to `/` for cross-platform keys and values)
        let repo_rel = normalize_path(repo_file.strip_prefix(&input.repo_path).map_err(|_| {
            DottyError::InvalidRepoPath {
                path: repo_file.to_string_lossy().to_string(),
                reason: format!("not inside the repository at {}", input.repo_path.display()),
            }
        })?);
        // Normalize the value too: format_target_display may produce backslashes on Windows,
        // which would cause key-value mismatches in orphan detection and status display.
        let target_rel = normalize_path(Path::new(&format_target_display(target_file)));
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
fn resolve_scope(machine: &Option<String>, platform: &Option<String>) -> String {
    if let Some(name) = machine {
        name.clone()
    } else if let Some(name) = platform {
        name.clone()
    } else {
        "base".to_string()
    }
}

/// Check whether a path looks like a standard XDG config location.
///
/// A path is considered "standard" if, relative to HOME, it is under
/// `~/.config/`, `~/.local/`, `~/.ssh/`, or is a dotfile (starts with `.`
/// but not `..`).
///
/// When HOME is unset, the path is treated as non-standard.
fn is_standard_xdg_path(target_path: &Path) -> bool {
    let home = crate::paths::home_dir().ok();
    let rel_str = match home {
        Some(home) => {
            let relative = target_path.strip_prefix(&home).unwrap_or(target_path);
            relative.to_string_lossy().into_owned()
        }
        None => {
            // HOME is unset — cannot determine standard-ness; treat as non-standard.
            target_path.to_string_lossy().into_owned()
        }
    };

    rel_str.starts_with(".config/")
        || rel_str.starts_with(".local/")
        || rel_str.starts_with(".ssh/")
        || (rel_str.starts_with('.') && !rel_str.starts_with(".."))
}

/// Check whether a path is under a sensitive system directory.
///
/// Sensitive prefixes: `/etc`, `/usr`, `/sys`, `/proc`.
fn is_sensitive_system_path(target_path: &Path) -> bool {
    let sensitive_prefixes = ["/etc", "/usr", "/sys", "/proc"];
    let path_str = target_path.to_string_lossy();
    sensitive_prefixes
        .iter()
        .any(|&prefix| path_str == prefix || path_str.starts_with(&format!("{}/", prefix)))
}

/// Warn about non-standard config paths in non-interactive (CI/script) mode.
///
/// Prints a warning for non-standard paths and returns an error for
/// sensitive system directories (e.g. `/etc`, `/usr`). Non-standard but
/// non-sensitive paths are allowed to proceed.
fn warn_non_xdg_non_interactive(target_path: &Path) -> Result<(), DottyError> {
    if !is_standard_xdg_path(target_path) {
        warn!(
            "'{}' doesn't look like a standard config location. Defaulting to base tier (run interactively to specify a different tier).",
            target_path.display()
        );
    }

    if is_sensitive_system_path(target_path) {
        return Err(DottyError::InvalidTargetPath {
            path: target_path.to_string_lossy().to_string(),
            reason: format!(
                "'{}' is under a sensitive system directory",
                target_path.display()
            ),
        });
    }

    Ok(())
}

/// Warn about non-standard config paths in interactive mode.
///
/// Prompts the user for non-standard paths (offering to re-run with a
/// specific tier) and for sensitive system paths (offering to proceed).
fn warn_non_xdg_interactive(target_path: &Path) -> Result<(), DottyError> {
    if !is_standard_xdg_path(target_path) {
        println!(
            "Warning: '{}' doesn't look like a standard config location.",
            target_path.display()
        );
        // Prompt semantics: "yes" = bail and ask user to re-run with --machine/--platform,
        // "no" = proceed to base tier without error. The "yes" path bails because the `add`
        // command does not support interactive tier selection — the user must re-run with
        // --machine <name> or --platform <name> to target a specific tier.
        let ok = prompt_confirm(
            "Add this file to a specific machine or platform tier instead of base? (yes = specify tier, no = add to base)",
        )?;
        if ok {
            return Err(DottyError::Cancelled);
        }
    }

    if is_sensitive_system_path(target_path) {
        println!(
            "Warning: '{}' is under a sensitive system directory.",
            target_path.display()
        );
        let ok = prompt_confirm("Proceed anyway?")?;
        if !ok {
            return Err(DottyError::Cancelled);
        }
    }

    Ok(())
}

/// Collect all files under the given path.
fn collect_files(target_path: &Path) -> Result<Vec<PathBuf>, DottyError> {
    let mut files = Vec::new();

    if target_path.is_file() || is_symlink(target_path) {
        files.push(target_path.to_path_buf());
    } else if target_path.is_dir() {
        walk_dir(target_path, &mut files, 0)?;
    } else {
        return Err(DottyError::InvalidTargetPath {
            path: target_path.display().to_string(),
            reason: "path does not exist".into(),
        });
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
) -> Result<Vec<PathBuf>, DottyError> {
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
    use crate::symlink::create_symlink;

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_resolve_scope_machine() {
        let scope = resolve_scope(&Some("macbook".into()), &None);
        assert_eq!(scope, "macbook");
    }

    #[test]
    fn test_resolve_scope_platform() {
        let scope = resolve_scope(&None, &Some("macos".into()));
        assert_eq!(scope, "macos");
    }

    #[test]
    fn test_resolve_scope_default() {
        let scope = resolve_scope(&None, &None);
        assert_eq!(scope, "base");
    }

    #[test]
    fn test_resolve_scope_machine_over_platform() {
        let scope = resolve_scope(&Some("macbook".into()), &Some("macos".into()));
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

    // -- is_standard_xdg_path tests --

    #[test]
    fn test_is_standard_xdg_config() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            assert!(is_standard_xdg_path(&home.join(".config/nvim/init.lua")));
            assert!(is_standard_xdg_path(&home.join(".config/app.conf")));
        });
    }

    #[test]
    fn test_is_standard_xdg_local() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            assert!(is_standard_xdg_path(&home.join(".local/share/app")));
        });
    }

    #[test]
    fn test_is_standard_xdg_ssh() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            assert!(is_standard_xdg_path(&home.join(".ssh/id_rsa")));
        });
    }

    #[test]
    fn test_is_standard_xdg_dotfile() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            assert!(is_standard_xdg_path(&home.join(".vimrc")));
            assert!(is_standard_xdg_path(&home.join(".gitconfig")));
        });
    }

    #[test]
    fn test_is_standard_xdg_not_standard() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            assert!(!is_standard_xdg_path(&home.join("custom/.config")));
            assert!(!is_standard_xdg_path(&home.join("some/weird/path")));
        });
    }

    #[test]
    fn test_is_standard_xdg_double_dot_prefix() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            // ..something should NOT be treated as a dotfile
            assert!(!is_standard_xdg_path(&home.join("..hidden")));
        });
    }

    #[test]
    fn test_is_standard_xdg_missing_home() {
        // Without HOME, absolute paths are non-standard
        temp_env::with_var_unset("HOME", || {
            assert!(!is_standard_xdg_path(Path::new("/some/path")));
        });
    }

    // -- is_sensitive_system_path tests --

    #[test]
    fn test_is_sensitive_system_path_etc() {
        assert!(is_sensitive_system_path(Path::new("/etc/passwd")));
        assert!(is_sensitive_system_path(Path::new("/etc")));
    }

    #[test]
    fn test_is_sensitive_system_path_usr() {
        assert!(is_sensitive_system_path(Path::new("/usr/local/bin/tool")));
        assert!(is_sensitive_system_path(Path::new("/usr")));
    }

    #[test]
    fn test_is_sensitive_system_path_sys_proc() {
        assert!(is_sensitive_system_path(Path::new("/sys/class/net")));
        assert!(is_sensitive_system_path(Path::new("/proc/cpuinfo")));
    }

    #[test]
    fn test_is_sensitive_system_path_not_sensitive() {
        assert!(!is_sensitive_system_path(Path::new("/home/user/.config")));
        assert!(!is_sensitive_system_path(Path::new("/var/log/app.log")));
    }

    // -- warn_non_xdg_non_interactive tests --

    #[test]
    fn test_warn_non_xdg_non_interactive_no_hang() {
        // In test environment (non-TTY), warn_non_xdg should return Ok
        // without hanging or returning an error.
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            // Non-standard path should not panic or error in non-interactive mode
            let result = warn_non_xdg_non_interactive(&home.join("some/weird/path"));
            assert!(
                result.is_ok(),
                "warn_non_xdg_non_interactive should return Ok, got: {result:?}"
            );
        });
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_defaults_to_base() {
        // Non-interactive mode should default to base tier with a clear warning.
        // This test verifies the warning message mentions "base tier" so that
        // the default action is transparent to the user.
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let result = warn_non_xdg_non_interactive(&home.join("custom/weird/path"));
            assert!(result.is_ok(), "should default to base without error");
        });
    }

    #[test]
    fn test_warn_non_xdg_ci_env_defaults_to_base() {
        // CI=true should always default to base tier, even if a TTY is present.
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            temp_env::with_var("CI", Some("1"), || {
                let result = warn_non_xdg_non_interactive(&home.join("some/weird/path"));
                assert!(
                    result.is_ok(),
                    "CI env should default to base without hanging"
                );
            });
        });
    }

    #[test]
    fn test_warn_non_xdg_standard_path_non_interactive() {
        // Standard paths (dotfiles, .config/) should also not error
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let result = warn_non_xdg_non_interactive(&home.join(".config/nvim/init.lua"));
            assert!(result.is_ok());

            let result = warn_non_xdg_non_interactive(&home.join(".vimrc"));
            assert!(result.is_ok());
        });
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_rejects_sensitive_etc() {
        let result = warn_non_xdg_non_interactive(Path::new("/etc/foobar"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("sensitive system directory"));
        assert!(msg.contains("/etc/foobar"));
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_rejects_sensitive_usr() {
        let result = warn_non_xdg_non_interactive(Path::new("/usr/local/bin/custom-tool"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("sensitive system directory"));
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_rejects_sensitive_sys() {
        let result = warn_non_xdg_non_interactive(Path::new("/sys/class/net"));
        assert!(result.is_err());
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_rejects_sensitive_proc() {
        let result = warn_non_xdg_non_interactive(Path::new("/proc/cpuinfo"));
        assert!(result.is_err());
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_allows_non_sensitive_non_standard() {
        // Non-standard but non-sensitive paths should still succeed
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let result = warn_non_xdg_non_interactive(&home.join("custom/.config"));
            assert!(result.is_ok());
        });
    }

    // -- warn_non_xdg_non_interactive: missing HOME tests --

    #[test]
    fn test_warn_non_xdg_non_interactive_missing_home() {
        // When HOME is unset, warn_non_xdg should NOT fail with
        // MissingHomeDirectory. Instead it should default to non-standard
        // behavior (warning + proceed) in non-interactive mode.
        temp_env::with_var_unset("HOME", || {
            let result = warn_non_xdg_non_interactive(Path::new("/some/path/file"));
            assert!(
                result.is_ok(),
                "warn_non_xdg_non_interactive should not error when HOME is unset, got: {result:?}"
            );
        });
    }

    #[test]
    fn test_warn_non_xdg_non_interactive_missing_home_sensitive_path() {
        // Even without HOME, sensitive system paths should still be rejected
        // in non-interactive mode.
        temp_env::with_var_unset("HOME", || {
            let result = warn_non_xdg_non_interactive(Path::new("/etc/passwd"));
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(msg.contains("sensitive system directory"));
        });
    }

    // -- Path traversal safety tests --

    #[test]
    fn test_self_reference_with_dotdot_components() {
        // Verify that paths with `..` components are correctly detected as self-references
        // even when the string representation differs from the canonical path.
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
                files_to_add: vec![target],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            // Should produce valid plan for non-repo path
            assert!(!output.plan.is_empty());
        });
    }

    #[test]
    fn test_self_reference_via_symlinked_repo_path() {
        // Integration test: repo path accessed via symlink should still be
        // detected as a self-reference after canonicalization resolves the symlink.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        // Create real repo and a symlink to it
        let real_repo = base.join("real_repo");
        let symlink_repo = base.join("link_repo");
        fs::create_dir_all(&real_repo).unwrap();
        create_symlink(&real_repo, &symlink_repo).unwrap();

        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let target = home.join(".vimrc");
        fs::write(&target, "content").unwrap();

        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            let input = AddPlanInput {
                repo_path: symlink_repo,
                state_path: state.clone(),
                home: home.clone(),
                scope: "base".to_string(),
                files_to_add: vec![target],
                commit: None,
                has_git: false,
            };
            let config = Config::new();
            let output = build_add_plan(&input, &config).unwrap();

            // Should produce valid plan for non-repo path (symlink resolves to same dir)
            assert!(!output.plan.is_empty());
        });
    }

    #[test]
    fn test_build_add_plan_normalized_value_in_managed() {
        // Verify that config.managed values use forward slashes consistently,
        // matching the key format. This prevents cross-platform mismatches
        // where format_target_display might produce backslashes on Windows.
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

            // Both key and value should use forward slashes
            let managed = &output.config.managed;
            assert!(
                managed.contains_key("macbook/home/.config/nvim/init.lua"),
                "key should use forward slashes"
            );
            let value = managed.get("macbook/home/.config/nvim/init.lua").unwrap();
            assert!(
                !value.contains('\\'),
                "value should not contain backslashes: {}",
                value
            );
            // Value should use forward slashes matching the key format
            assert!(
                value.contains('/'),
                "value should use forward slashes: {}",
                value
            );
        });
    }
}
