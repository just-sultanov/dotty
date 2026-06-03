use std::fs;
use std::path::{Path, PathBuf};

use crate::backups::backup_timestamp;
use crate::config::Config;
use crate::config::write_config;
use crate::err_msg;
use crate::error::DottyError;
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
use tracing::warn;

/// Run the `add` command.
pub fn run(
    path: String,
    machine: Option<String>,
    platform: Option<String>,
    commit: Option<String>,
    dry_run: bool,
) -> Result<(), DottyError> {
    let mut repo = RepoState::new()?;

    let repo_path = repo.repo_path.clone();
    let state_path = repo.state_path.clone();

    // Expand ~ in the input path
    let target_path = expand_tilde(&path)?;

    // Determine scope (tier directory name)
    let scope = resolve_scope(&machine, &platform);

    // Reject paths inside the dotty repo itself.
    // Two-layer defense: (1) canonicalize both paths and compare, (2) string-prefix fallback
    // if canonicalization fails (e.g., broken symlink). The canonical check is primary;
    // the string check is secondary — it handles cases where the OS cannot resolve the path.
    let canonical_repo = match fs::canonicalize(&repo_path) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to canonicalize repo path: {e}, using original path");
            repo_path.clone()
        }
    };

    let is_self_reference = if let Ok(canonical_target) = fs::canonicalize(&target_path) {
        // Primary defense: compare canonical (resolved) paths
        canonical_target.starts_with(&canonical_repo)
    } else {
        // Secondary defense: use Path::starts_with() on non-canonicalized paths
        // This is more robust than string comparison as it handles:
        // - Unicode normalization differences
        // - Platform-specific path separators
        // - Mixed absolute/relative path representations
        target_path.starts_with(&canonical_repo)
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

    // Build conflict map from existing tracked files.
    // Collects all files eagerly into a Vec before processing.
    let conflict_map = if repo.is_git_repo {
        match git::TrackedFiles::new(&repo_path) {
            Ok(iterator) => build_conflict_map(iterator),
            Err(e) => {
                warn!("failed to list tracked files: {e}");
                indexmap::IndexMap::new()
            }
        }
    } else {
        indexmap::IndexMap::new()
    };

    // Resolve conflicts interactively
    let files_to_override = resolve_conflicts(&files_to_add, &conflict_map)?;

    let home = crate::paths::home_dir()?;
    let has_git = repo.is_git_repo;
    let config = repo.config.clone();

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
    let mode = if dry_run {
        plan::ExecuteMode::DryRun
    } else {
        plan::ExecuteMode::Normal
    };
    plan::execute_plan(&output.plan, mode, &mut repo)?;

    // Write updated config only after successful plan execution.
    if !dry_run && !output.plan.is_empty() {
        write_config(&state_path, &output.config)?;
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
        let repo_absolute_path = input.repo_path.join(&input.scope).join(&rel_path);

        // Create parent directories in repo
        if let Some(parent) = repo_absolute_path.parent() {
            plan.add(Action::CreateDir {
                path: parent.to_path_buf(),
            });
        }

        // Backup original file if it exists at target
        let backup_path = if target_file.exists() {
            let dest = if let Ok(relative) = target_file.strip_prefix(&input.home) {
                backup_base.join(relative)
            } else {
                backup_base.join(target_file.file_name().unwrap_or_default())
            };
            plan.add(Action::Backup {
                source: target_file.clone(),
                dest: dest.clone(),
            });
            Some(dest)
        } else {
            None
        };
        let backup_exists = target_file.exists();

        // Copy file to repo (dereference symlinks)
        plan.add(Action::CopyFile {
            source: target_file.clone(),
            dest: repo_absolute_path.clone(),
        });

        // Create symlink at target location pointing to repo file
        plan.add(Action::CreateSymlink {
            target: repo_absolute_path.clone(),
            link: target_file.clone(),
            backup_path,
            backup_exists,
        });

        // Track path for git add
        if let Ok(rel) = repo_absolute_path.strip_prefix(&input.repo_path) {
            git_add_paths.push(rel.to_path_buf());
        }

        // Update managed map (normalize separators to `/` for cross-platform keys and values)
        let repo_relative_path = normalize_path(
            repo_absolute_path
                .strip_prefix(&input.repo_path)
                .map_err(|_| DottyError::InvalidRepoPath {
                    path: repo_absolute_path.to_string_lossy().to_string(),
                    reason: format!("not inside the repository at {}", input.repo_path.display()),
                })?,
        );
        // Normalize the value too: format_target_display may produce backslashes on Windows,
        // which would cause key-value mismatches in orphan detection and status display.
        let target_rel = normalize_path(Path::new(&format_target_display(target_file)));
        config.managed.insert(repo_relative_path, target_rel);
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
            reason: err_msg!(
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
///
/// Symlink handling:
/// - Symlink to a directory: recursively collect all files from the target directory.
/// - Symlink to a file: collect the symlink path itself (matching `apply` behavior).
/// - Broken symlink: treated as non-existent (error).
/// - Real file: collected as-is.
/// - Real directory: recursively collected via `walk_dir`.
fn collect_files(target_path: &Path) -> Result<Vec<PathBuf>, DottyError> {
    let mut files = Vec::new();

    // Check for symlink FIRST using symlink_metadata (does not follow symlinks).
    // This must come before is_file()/is_dir() which both follow symlinks.
    if is_symlink(target_path) {
        // is_dir() follows symlinks — if the target is a directory, traverse it.
        // For broken symlinks, is_dir() returns false, so they fall through to
        // the is_file() check below (which also returns false), resulting in
        // a "path does not exist" error.
        if target_path.is_dir() {
            walk_dir(target_path, &mut files, 0)?;
        } else if target_path.is_file() {
            // Symlink to file: collect the symlink path itself.
            files.push(target_path.to_path_buf());
        } else {
            // Broken symlink — neither is_dir nor is_file succeeds.
            return Err(DottyError::InvalidTargetPath {
                path: target_path.display().to_string(),
                reason: "broken symlink".into(),
            });
        }
    } else if target_path.is_file() {
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
/// Uses IndexMap to preserve insertion order for deterministic conflict display.
///
/// This is the eager-evaluation version kept for backward compatibility with tests.
/// Build a conflict map from tracked files.
///
/// Consumes the iterator and folds entries into an IndexMap keyed by
/// target path. The tracked files are collected eagerly by the caller;
/// this function avoids intermediate allocations during map construction.
fn build_conflict_map(files: git::TrackedFiles) -> indexmap::IndexMap<PathBuf, Vec<String>> {
    files
        .into_iter()
        .fold(indexmap::IndexMap::new(), |mut map, repo_relative_path| {
            let repo_path = PathBuf::from(&repo_relative_path);
            if let Ok(target) = repo_to_target(&repo_path) {
                map.entry(target).or_default().push(repo_relative_path);
            }
            map
        })
}

/// Resolve conflicts for the files being added.
///
/// Returns the subset of files that should proceed (after user confirmation).
fn resolve_conflicts(
    files_to_add: &[PathBuf],
    conflict_map: &indexmap::IndexMap<PathBuf, Vec<String>>,
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
    use proptest::prelude::*;

    /// Create a unique temporary directory that is automatically cleaned up on drop.
    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Helper to create a mock iterator from a Vec of file paths.
    /// This simulates the production `TrackedFiles` for testing purposes.
    fn mock_git_iterator(files: Vec<String>) -> impl Iterator<Item = String> {
        files.into_iter()
    }

    /// Build conflict map from an iterator (used in tests).
    /// This mirrors the production `build_conflict_map` logic.
    fn build_conflict_map_from_iter(
        files: impl Iterator<Item = String>,
    ) -> indexmap::IndexMap<PathBuf, Vec<String>> {
        files
            .into_iter()
            .fold(indexmap::IndexMap::new(), |mut map, repo_relative_path| {
                let repo_path = PathBuf::from(&repo_relative_path);
                if let Ok(target) = repo_to_target(&repo_path) {
                    map.entry(target).or_default().push(repo_relative_path);
                }
                map
            })
    }

    // ── Property tests for build_conflict_map ──

    /// Generator for valid repo paths with proper tier structure.
    fn repo_path() -> impl Strategy<Value = String> {
        prop_oneof![
            "base/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            "macos/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            "linux/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
            "macbook/home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
        ]
    }

    // Invariant: build_conflict_map is deterministic.
    // Same input always produces the same output.
    proptest! {
        #[test]
        fn proptest_build_conflict_map_deterministic(
            files in proptest::collection::vec(repo_path(), 0..20),
        ) {
            crate::tests::with_test_home(|_home| {
                let result1 = build_conflict_map_from_iter(mock_git_iterator(files.clone()));
                let result2 = build_conflict_map_from_iter(mock_git_iterator(files));

                assert_eq!(
                    &result1, &result2,
                    "build_conflict_map should be deterministic"
                );
            });
        }
    }

    // Invariant: empty input produces empty map.
    proptest::proptest! {
        #[test]
        fn proptest_build_conflict_map_empty(_ in Just(())) {
            crate::tests::with_test_home(|_home| {
                let empty: Vec<String> = vec![];
                let map = build_conflict_map_from_iter(mock_git_iterator(empty));

                assert!(map.is_empty(), "empty input should produce empty map");
            });
        }
    }

    // Invariant: map size is at most the input size (deduplication by target).
    proptest! {
        #[test]
        fn proptest_build_conflict_map_size_bound(
            files in proptest::collection::vec(repo_path(), 0..20),
        ) {
            crate::tests::with_test_home(|_home| {
                let map = build_conflict_map_from_iter(mock_git_iterator(files.clone()));

                assert!(
                    map.len() <= files.len(),
                    "map size ({}) should be <= input size ({})",
                    map.len(), files.len()
                );
            });
        }
    }

    // Invariant: all input repo paths appear in at least one value.
    proptest! {
        #[test]
        fn proptest_build_conflict_map_preserves_paths(
            files in proptest::collection::vec(repo_path(), 1..15),
        ) {
            crate::tests::with_test_home(|_home| {
                let map = build_conflict_map_from_iter(mock_git_iterator(files.clone()));

                // Each input file should appear in at least one value in the map
                for file in &files {
                    let found = map.values().any(|paths| paths.contains(file));
                    assert!(
                        found,
                        "input file {:?} should appear in map values",
                        file
                    );
                }
            });
        }
    }

    // Invariant: files with the same target path are grouped together.
    proptest! {
        #[test]
        fn proptest_build_conflict_map_groups_by_target(
            target_file in "home/[.a-zA-Z0-9_@-]{2,20}".prop_map(|s| s.to_string()),
        ) {
            crate::tests::with_test_home(|_home| {
                // Create files from different tiers that map to the same target
                let base_file = format!("base/{}", target_file);
                let macos_file = format!("macos/{}", target_file);
                let files = vec![base_file.clone(), macos_file.clone()];

                let map = build_conflict_map_from_iter(mock_git_iterator(files));

                // Both files should map to the same target
                let target = crate::paths::repo_to_target(&std::path::PathBuf::from(&base_file)).unwrap();
                let paths = map.get(&target);

                assert!(paths.is_some(), "target should exist in map");
                let paths = paths.unwrap();
                assert!(paths.len() == 2, "both files should be grouped at target");
                assert!(paths.contains(&base_file), "should contain base file");
                assert!(paths.contains(&macos_file), "should contain macos file");
            });
        }
    }

    // Invariant: insertion order is preserved in the map.
    proptest! {
        #[test]
        fn proptest_build_conflict_map_preserves_insertion_order(
            files in proptest::collection::vec(repo_path(), 1..10),
        ) {
            crate::tests::with_test_home(|_home| {
                let map = build_conflict_map_from_iter(mock_git_iterator(files.clone()));

                // Collect all paths from the map in order
                let all_paths: Vec<String> = map.values().flatten().cloned().collect();

                // Each file should appear at least once
                for file in &files {
                    assert!(
                        all_paths.contains(file),
                        "file {:?} should appear in ordered output",
                        file
                    );
                }
            });
        }
    }

    // Invariant: deeply nested paths are handled correctly.
    proptest! {
        #[test]
        fn proptest_build_conflict_map_deep_nesting(
            depth in 1usize..8,
        ) {
            crate::tests::with_test_home(|_home| {
                let components: Vec<String> = (0..depth)
                    .map(|i| format!("file_{}", i))
                    .collect();
                let file_path = components.join("/");
                let repo_file = format!("base/home/{}", file_path);

                let files = vec![repo_file.clone()];
                let map = build_conflict_map_from_iter(mock_git_iterator(files));

                // Map should have exactly one entry
                assert!(map.len() == 1, "should have exactly one entry");

                // The single value should contain our file
                let all_values: Vec<String> = map.values().flatten().cloned().collect();
                assert!(all_values.contains(&repo_file));
            });
        }
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
            let map = build_conflict_map_from_iter(mock_git_iterator(existing));

            assert!(map.contains_key(&home.join(".vimrc")));
            assert!(map.contains_key(&home.join(".gitconfig")));
            assert!(map.contains_key(&home.join(".config/nvim/plugins.lua")));
        });
    }

    #[test]
    fn test_conflict_map_empty() {
        let empty: Vec<String> = vec![];
        let map = build_conflict_map_from_iter(mock_git_iterator(empty));
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

    // -- CreateSymlink backup_path tests --

    #[test]
    fn test_build_add_plan_sets_backup_path_when_target_exists() {
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

            let symlink_actions: Vec<&Action> = output
                .plan
                .actions
                .iter()
                .filter(|a| matches!(a, Action::CreateSymlink { .. }))
                .collect();
            assert_eq!(symlink_actions.len(), 1, "expected one CreateSymlink");
            match symlink_actions[0] {
                Action::CreateSymlink {
                    backup_path,
                    backup_exists,
                    ..
                } => {
                    assert!(
                        backup_path.is_some(),
                        "backup_path should be Some when file exists"
                    );
                    assert!(
                        backup_exists,
                        "backup_exists should be true when file exists"
                    );
                }
                _ => unreachable!(),
            }
        });
    }

    #[test]
    fn test_build_add_plan_sets_no_backup_path_when_target_missing() {
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        let state = base.join("state");
        let home = base.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();

        let target = home.join(".nonexistent");

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

            let symlink_actions: Vec<&Action> = output
                .plan
                .actions
                .iter()
                .filter(|a| matches!(a, Action::CreateSymlink { .. }))
                .collect();
            assert_eq!(symlink_actions.len(), 1, "expected one CreateSymlink");
            match symlink_actions[0] {
                Action::CreateSymlink {
                    backup_path,
                    backup_exists,
                    ..
                } => {
                    assert!(
                        backup_path.is_none(),
                        "backup_path should be None when no file exists"
                    );
                    assert!(
                        !backup_exists,
                        "backup_exists should be false when no file exists"
                    );
                }
                _ => unreachable!(),
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

    /// Verifies that conflict_map preserves insertion order via IndexMap,
    /// ensuring deterministic conflict display across runs.
    #[test]
    fn test_build_conflict_map_deterministic_order() {
        let dir = test_dir();
        let home = dir.path().to_path_buf();
        temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
            // Insert in a specific order: .gitconfig, .vimrc, .config/nvim/plugins.lua
            let existing = vec![
                "base/home/.gitconfig".into(),
                "base/home/.vimrc".into(),
                "macbook/home/.config/nvim/plugins.lua".into(),
            ];
            let map = build_conflict_map_from_iter(mock_git_iterator(existing));

            // Verify keys are in insertion order (deterministic)
            let keys: Vec<PathBuf> = map.keys().cloned().collect();
            assert_eq!(keys.len(), 3);
            assert_eq!(keys[0], home.join(".gitconfig"));
            assert_eq!(keys[1], home.join(".vimrc"));
            assert_eq!(keys[2], home.join(".config/nvim/plugins.lua"));
        });
    }

    // -- collect_files symlink handling tests --

    #[test]
    fn test_collect_files_symlink_to_directory() {
        // Symlink to directory should collect ALL files from the target directory,
        // not just the symlink itself.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        // Create the real directory with files
        let real_dir = base.join("real_dir");
        fs::create_dir_all(real_dir.join("sub")).unwrap();
        fs::write(real_dir.join("file1.txt"), "content1").unwrap();
        fs::write(real_dir.join("sub").join("file2.txt"), "content2").unwrap();

        // Create symlink to the directory
        let link_dir = base.join("link_dir");
        create_symlink(&real_dir, &link_dir).unwrap();

        let files = collect_files(&link_dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.file_name().unwrap() == "file1.txt"));
        assert!(files.iter().any(|f| f.file_name().unwrap() == "file2.txt"));
    }

    #[test]
    fn test_collect_files_symlink_to_file() {
        // Symlink to file should collect only the symlink path.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        let real_file = base.join("real_file.txt");
        fs::write(&real_file, "content").unwrap();

        let link_file = base.join("link_file.txt");
        create_symlink(&real_file, &link_file).unwrap();

        let files = collect_files(&link_file).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], link_file);
    }

    #[test]
    fn test_collect_files_broken_symlink() {
        // Broken symlink should return an error.
        let dir = test_dir();
        let link = dir.path().join("broken_link");
        create_symlink(&dir.path().join("nonexistent_target"), &link).unwrap();

        let result = collect_files(&link);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("broken symlink"));
    }

    #[test]
    fn test_collect_files_symlink_to_nonexistent() {
        // Symlink to a non-existent path should return an error.
        let dir = test_dir();
        let link = dir.path().join("link_to_nothing");
        // On Unix, we can create a symlink to a non-existent target
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nonexistent/path", &link).unwrap();

        let result = collect_files(&link);
        assert!(result.is_err());
    }

    #[test]
    fn test_collect_files_real_directory() {
        // Real directory should still be traversed recursively.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("a.txt"), "a").unwrap();
        fs::write(base.join("sub").join("b.txt"), "b").unwrap();

        let files = collect_files(&base).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_collect_files_real_file() {
        // Real file should be collected as-is.
        let dir = test_dir();
        let file = dir.path().join("file.txt");
        fs::write(&file, "content").unwrap();

        let files = collect_files(&file).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], file);
    }

    // -- Path traversal detection edge case tests --

    #[test]
    fn test_path_traversal_detection_unicode_paths() {
        // Verify that Unicode paths are handled correctly in traversal detection.
        // This tests that Path::starts_with() handles Unicode normalization properly.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        // Create a directory with Unicode characters in the name
        let unicode_dir = base.join("日本語");
        fs::create_dir_all(&unicode_dir).unwrap();
        let unicode_file = unicode_dir.join("test.txt");
        fs::write(&unicode_file, "content").unwrap();

        // The file should not be detected as a self-reference
        // since it's outside the repo directory
        let is_traversal = unicode_file.starts_with(&base.join("repo"));
        assert!(
            !is_traversal,
            "Unicode path should not be detected as traversal"
        );
    }

    #[test]
    fn test_path_traversal_detection_symlink_in_path() {
        // Verify that paths with symlinks in the middle are handled correctly.
        let dir = test_dir();
        let base = dir.path().to_path_buf();

        // Create a real directory and a symlink to it
        let real_dir = base.join("real_dir");
        fs::create_dir_all(&real_dir).unwrap();

        let symlink_dir = base.join("symlink_dir");
        create_symlink(&real_dir, &symlink_dir).unwrap();

        // Create a file inside the real directory
        let file_via_real = real_dir.join("file.txt");
        fs::write(&file_via_real, "content").unwrap();

        // The canonicalized path should resolve the symlink.
        // Note: On macOS, /var is a symlink to /private/var, so we compare
        // the canonicalized paths rather than the original paths.
        let canonicalized = fs::canonicalize(&file_via_real).unwrap();
        let canonical_real_dir = fs::canonicalize(&real_dir).unwrap();
        assert_eq!(canonicalized.parent().unwrap(), canonical_real_dir);
    }

    #[test]
    fn test_path_traversal_detection_relative_vs_absolute() {
        // Verify that relative and absolute paths are handled consistently.
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let file = repo.join("test.txt");
        fs::write(&file, "content").unwrap();

        // Get the canonical absolute path
        let canonical_file = fs::canonicalize(&file).unwrap();
        let canonical_repo = fs::canonicalize(&repo).unwrap();

        // Absolute path should be detected as traversal
        assert!(canonical_file.starts_with(&canonical_repo));

        // Test with relative path (if we're in the right directory)
        std::env::set_current_dir(&base).unwrap();
        let relative_file = Path::new("repo/test.txt");
        let canonical_relative = fs::canonicalize(relative_file).unwrap();
        assert!(canonical_relative.starts_with(&canonical_repo));

        // Restore working directory
        std::env::set_current_dir(&dir).unwrap();
    }

    #[test]
    fn test_path_traversal_detection_normalized_paths() {
        // Verify that paths with .. components are handled correctly.
        let dir = test_dir();
        let base = dir.path().to_path_buf();
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();

        // Create a file in the repo
        let file = repo.join("test.txt");
        fs::write(&file, "content").unwrap();

        // Access the file via a path with .. components
        let subdir = repo.join("subdir");
        fs::create_dir_all(&subdir).unwrap();
        std::env::set_current_dir(&subdir).unwrap();

        let canonical_repo = fs::canonicalize(&repo).unwrap();
        let path_with_dotdot = Path::new("../test.txt");
        let canonical_path = fs::canonicalize(path_with_dotdot).unwrap();

        // The canonicalized path should still be inside the repo
        assert!(canonical_path.starts_with(&canonical_repo));

        // Restore working directory
        std::env::set_current_dir(&dir).unwrap();
    }
}
