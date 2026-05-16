//! Snapshot tests for `status` and `apply --dry-run` output.
//!
//! Uses `insta` to capture and compare formatted output. Dynamic paths
//! are replaced with placeholders so snapshots remain stable across machines.

mod common;
use common::TestEnv;
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run `dotty` with Unicode terminal forced (for stable symbols).
fn run_unicode(env: &TestEnv, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_dotty"))
        .env("DOTTY_HOME", &env.repo)
        .env("DOTTY_STATE_HOME", &env.state)
        .env("HOME", &env.home)
        .env("TERM", "xterm-256color")
        .args(args)
        .output()
        .expect("failed to spawn dotty");

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "dotty {:?} failed (exit {})\nstderr: {}",
            args, out.status, stderr
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Setup a repo with one tracked file (base/home/.vimrc).
fn setup_repo_with_file(env: &TestEnv) {
    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let repo_file = env.repo.join("base/home/.vimrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "set number").unwrap();

    Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.vimrc"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add vimrc"])
        .output()
        .unwrap();
}

/// Replace dynamic paths in output with stable placeholders.
/// Replaces longer paths first to avoid partial matches when one path
/// is a substring of another (e.g., temp dirs like `dotty_integ_123_6`
/// and `dotty_integ_123_60`).
fn normalize_paths(output: &str, env: &TestEnv) -> String {
    let mut replacements: Vec<(String, &str)> = vec![
        (env.repo.to_string_lossy().into_owned(), "[REPO]"),
        (env.home.to_string_lossy().into_owned(), "[HOME]"),
        (env.state.to_string_lossy().into_owned(), "[STATE]"),
    ];
    // Sort by path length descending so longer (more specific) paths
    // are replaced before shorter ones that might be their prefix.
    replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut result = output.to_string();
    for (path, placeholder) in replacements {
        result = result.replace(&path, placeholder);
    }
    result
}

/// Capture output, normalize paths, and assert snapshot.
fn snapshot_output(env: &TestEnv, name: &str, args: &[&str]) {
    let output = run_unicode(env, args);
    let normalized = normalize_paths(&output, env);
    insta::assert_snapshot!(name, normalized);
}

// ---------------------------------------------------------------------------
// status — clean repo
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_clean() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    snapshot_output(&env, "status_clean", &["status"]);
}

// ---------------------------------------------------------------------------
// status — after apply (symlinks created)
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_after_apply() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Apply creates symlinks
    run_unicode(&env, &["apply"]);

    snapshot_output(&env, "status_after_apply", &["status"]);
}

// ---------------------------------------------------------------------------
// status — broken symlink (repo file deleted)
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_broken_symlink() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Apply to create symlink
    run_unicode(&env, &["apply"]);

    // Delete the repo file to create a broken symlink
    std::fs::remove_file(env.repo.join("base/home/.vimrc")).unwrap();

    snapshot_output(&env, "status_broken_symlink", &["status"]);
}

// ---------------------------------------------------------------------------
// status — git dirty
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_git_dirty() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Modify a tracked file to make git dirty
    std::fs::write(
        env.repo.join("base/home/.vimrc"),
        "set number\nset relativenumber",
    )
    .unwrap();

    snapshot_output(&env, "status_git_dirty", &["status"]);
}

// ---------------------------------------------------------------------------
// status — inactive tiers
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_inactive_tiers() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Add a file in an inactive platform tier
    let inactive = if cfg!(target_os = "macos") {
        "linux"
    } else {
        "macos"
    };
    let inactive_file = env.repo.join(format!("{inactive}/home/.bashrc"));
    std::fs::create_dir_all(inactive_file.parent().unwrap()).unwrap();
    std::fs::write(&inactive_file, "export PATH=\"$PATH:/usr/local/bin\"").unwrap();

    Command::new("git")
        .current_dir(&env.repo)
        .args(["add", &format!("{inactive}/home/.bashrc")])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", &format!("add {inactive} bashrc")])
        .output()
        .unwrap();

    snapshot_output(&env, "status_inactive_tiers", &["status"]);
}

// ---------------------------------------------------------------------------
// status — tier conflicts (override)
// ---------------------------------------------------------------------------

#[test]
fn snapshot_status_tier_conflicts() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Add a machine-tier override for the same target
    let machine_override = env.repo.join("testbox/home/.vimrc");
    std::fs::create_dir_all(machine_override.parent().unwrap()).unwrap();
    std::fs::write(&machine_override, "set number\nset relativenumber").unwrap();

    Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "testbox/home/.vimrc"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add machine vimrc override"])
        .output()
        .unwrap();

    snapshot_output(&env, "status_tier_conflicts", &["status"]);
}

// ---------------------------------------------------------------------------
// apply --dry-run — snapshot
// ---------------------------------------------------------------------------

#[test]
fn snapshot_apply_dry_run_new_files() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    snapshot_output(&env, "apply_dry_run_new_files", &["apply", "--dry-run"]);
}

#[test]
fn snapshot_apply_dry_run_with_overrides() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // Add a machine-tier override for the same target
    let machine_override = env.repo.join("testbox/home/.vimrc");
    std::fs::create_dir_all(machine_override.parent().unwrap()).unwrap();
    std::fs::write(&machine_override, "set number\nset relativenumber").unwrap();

    Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "testbox/home/.vimrc"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add machine vimrc override"])
        .output()
        .unwrap();

    snapshot_output(
        &env,
        "apply_dry_run_with_overrides",
        &["apply", "--dry-run"],
    );
}

// ---------------------------------------------------------------------------
// apply — idempotent (second run, all skipped)
// ---------------------------------------------------------------------------

#[test]
fn snapshot_apply_second_run_all_skipped() {
    let env = TestEnv::new();
    setup_repo_with_file(&env);

    // First apply — creates symlinks
    run_unicode(&env, &["apply"]);

    // Second apply — all files should be skipped (unchanged)
    snapshot_output(&env, "apply_second_run_all_skipped", &["apply"]);
}
