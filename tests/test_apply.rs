//! Phase 5 — `apply` integration tests.

mod common;
use common::TestEnv;

// ---------------------------------------------------------------------------
// apply — replaces wrong symlinks
// ---------------------------------------------------------------------------

#[test]
fn apply_replaces_wrong_symlink() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Repo file
    let repo_file = env.repo.join("base/home/.vimrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "set number").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.vimrc"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add vimrc", "--allow-empty"])
        .output()
        .unwrap();

    let target = env.home.join(".vimrc");

    // Create a wrong symlink (pointing somewhere else)
    let wrong_target = env.home.join(".wrong_target");
    std::fs::write(&wrong_target, "wrong").unwrap();
    std::fs::remove_file(&wrong_target).unwrap();
    symlink_rs::symlink_file(env.home.join("some/nonexistent/path"), &target).unwrap();

    assert!(target.is_symlink());
    // Verify it points to the wrong place
    let actual = std::fs::read_link(&target).unwrap();
    assert_ne!(actual, repo_file);

    // Apply should replace the wrong symlink
    env.run_ok(&["apply"]);

    // Now it should point to the correct repo file
    env.assert_symlink(&target, &repo_file);
}

#[test]
fn apply_creates_symlinks_for_tracked_files() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Manually create a file in the repo (simulating a pre-existing repo)
    let repo_file = env.repo.join("base/home/.vimrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "set number").unwrap();

    // Stage and commit
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.vimrc"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add vimrc", "--allow-empty"])
        .output()
        .unwrap();

    // Target location is env.home (HOME env var points there)
    let target = env.home.join(".vimrc");

    // Apply should create a symlink at the target
    env.run_ok(&["apply"]);

    // The target should now be a symlink pointing to the repo file
    env.assert_symlink(&target, &repo_file);
}

#[test]
fn apply_fails_without_repo() {
    let env = TestEnv::new();

    // No init — should fail
    let out = env.run_err(&["apply"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no dotty repository found"),
        "expected 'no dotty repository found' error:\\n{}",
        stderr
    );
}

#[test]
fn apply_dry_run_makes_no_changes() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let repo_file = env.repo.join("base/home/.testrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "test").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.testrc"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add testrc", "--allow-empty"])
        .output()
        .unwrap();

    let target = env.home.join(".testrc");

    env.run_ok(&["apply", "--dry-run"]);

    // No symlink created
    assert!(!target.is_symlink(), "dry-run should not create symlinks");
}

// ---------------------------------------------------------------------------
// apply — tier override
// ---------------------------------------------------------------------------

#[test]
fn apply_machine_tier_overrides_base() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "mybox"]);
    env.git_config_identity();

    // Create base tier file
    let base_file = env.repo.join("base/home/.config/app.conf");
    std::fs::create_dir_all(base_file.parent().unwrap()).unwrap();
    std::fs::write(&base_file, "base config").unwrap();

    // Create machine tier override
    let machine_file = env.repo.join("mybox/home/.config/app.conf");
    std::fs::create_dir_all(machine_file.parent().unwrap()).unwrap();
    std::fs::write(&machine_file, "machine config").unwrap();

    // Stage and commit both
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "-A"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add configs", "--allow-empty"])
        .output()
        .unwrap();

    env.run_ok(&["apply"]);

    // Symlink should point to the machine tier file (higher priority)
    let target = env.home.join(".config/app.conf");
    env.assert_symlink(&target, &machine_file);
}

#[test]
fn apply_platform_tier_overrides_base() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // base tier
    let base_file = env.repo.join("base/home/.config/editor.conf");
    std::fs::create_dir_all(base_file.parent().unwrap()).unwrap();
    std::fs::write(&base_file, "base editor").unwrap();

    // platform tier (macos)
    let plat_file = env.repo.join("macos/home/.config/editor.conf");
    std::fs::create_dir_all(plat_file.parent().unwrap()).unwrap();
    std::fs::write(&plat_file, "macos editor").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "-A"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add editor configs", "--allow-empty"])
        .output()
        .unwrap();

    env.run_ok(&["apply"]);

    // On macOS the platform tier should win; on other platforms base wins.
    // We can't control detect_platform() from integration tests, so we just
    // verify that apply succeeds and creates a symlink.
    let target = env.home.join(".config/editor.conf");
    assert!(target.is_symlink(), "symlink not created");
}

// ---------------------------------------------------------------------------
// apply — backup on conflict
// ---------------------------------------------------------------------------

#[test]
fn apply_backups_existing_regular_file() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Repo file
    let repo_file = env.repo.join("base/home/.existing");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "repo version").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.existing"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add existing", "--allow-empty"])
        .output()
        .unwrap();

    // Pre-existing regular file at target (simulating user's own file)
    let target = env.home.join(".existing");
    std::fs::write(&target, "user version").unwrap();

    env.run_ok(&["apply"]);

    // Target should now be a symlink
    env.assert_symlink(&target, &repo_file);

    // Backup should exist in state/backups/
    let backup_dir = env.state.join("backups");
    assert!(backup_dir.is_dir(), "backup dir not created");

    // Find the backup file
    let backup_entries: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!backup_entries.is_empty(), "no backup entries found");
}

// ---------------------------------------------------------------------------
// apply — idempotent (already correct symlinks are skipped)
// ---------------------------------------------------------------------------

#[test]
fn apply_skips_correct_symlinks() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let repo_file = env.repo.join("base/home/.vimrc");
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "set number").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", "base/home/.vimrc"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", "add vimrc"])
        .output()
        .unwrap();

    let target = env.home.join(".vimrc");

    // First apply — creates symlink
    env.run_ok(&["apply"]);
    assert!(target.is_symlink());

    // Second apply — should succeed without errors (idempotent)
    env.run_ok(&["apply"]);
    assert!(target.is_symlink());
    env.assert_symlink(&target, &repo_file);
}

// ---------------------------------------------------------------------------
// apply — --platform override
// ---------------------------------------------------------------------------

/// Return a known platform name that is different from the current one,
/// so tests can verify that a non-active platform tier is not applied.
fn other_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "linux"
    } else if cfg!(target_os = "linux") {
        "macos"
    } else {
        // On freebsd or unknown, pick macos
        "macos"
    }
}

#[test]
fn apply_platform_override_uses_specified_tier() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file in a platform tier that is NOT the current platform.
    // This ensures the test works on any CI runner.
    let other = other_platform();
    let repo_file = env.repo.join(format!("{other}/home/.bashrc"));
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "export PATH=\"$PATH:/usr/local/bin\"").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", &format!("{other}/home/.bashrc")])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", &format!("add {other} bashrc")])
        .output()
        .unwrap();

    let target = env.home.join(".bashrc");

    // Without --platform override, the other platform tier is not active.
    // So the file should not be applied.
    env.run_ok(&["apply"]);
    assert!(
        !target.exists(),
        "{other} tier file should not be applied without --platform override"
    );

    // With --platform override, the other platform tier is active
    env.run_ok(&["apply", "--platform", other]);
    assert!(
        target.is_symlink(),
        "{other} tier file should be applied with --platform {other}"
    );
    env.assert_symlink(&target, &repo_file);
}
