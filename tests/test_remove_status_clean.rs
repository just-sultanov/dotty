//! Phase 6 — `remove` + `status` + `clean` integration tests.

mod common;
use common::TestEnv;

// ---------------------------------------------------------------------------
// remove — directory (recursive removal)
// ---------------------------------------------------------------------------

#[test]
fn remove_directory_recursively() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a directory with multiple files
    let dir = env.home.join(".config/nvim");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.lua"), "vim.g.mapleader = ' '").unwrap();
    std::fs::write(dir.join("lazy.lua"), "require('lazy').setup()").unwrap();
    let sub = dir.join("lua");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("mappings.lua"), "keymap").unwrap();

    // Add the entire directory
    env.run_ok(&["add", dir.to_str().unwrap(), "--commit", "add nvim"]);

    // Verify all files are tracked and symlinked
    let tracked = env.tracked_files();
    assert!(tracked.contains(&"base/home/.config/nvim/init.lua".to_string()));
    assert!(tracked.contains(&"base/home/.config/nvim/lazy.lua".to_string()));
    assert!(tracked.contains(&"base/home/.config/nvim/lua/mappings.lua".to_string()));

    assert!(dir.join("init.lua").is_symlink());
    assert!(dir.join("lazy.lua").is_symlink());
    assert!(dir.join("lua/mappings.lua").is_symlink());

    // Remove the entire directory
    env.run_ok(&["remove", dir.to_str().unwrap()]);

    // All symlinks should be gone
    assert!(!dir.join("init.lua").is_symlink());
    assert!(!dir.join("lazy.lua").is_symlink());
    assert!(!dir.join("lua/mappings.lua").is_symlink());

    // All files should be restored as regular files
    assert!(dir.join("init.lua").is_file());
    assert!(dir.join("lazy.lua").is_file());
    assert!(dir.join("lua/mappings.lua").is_file());

    // Content should be preserved
    assert_eq!(
        std::fs::read_to_string(dir.join("init.lua")).unwrap(),
        "vim.g.mapleader = ' '"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("lazy.lua")).unwrap(),
        "require('lazy').setup()"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("lua/mappings.lua")).unwrap(),
        "keymap"
    );

    // Repo files should be deleted
    assert!(!env.repo.join("base/home/.config/nvim/init.lua").exists());
    assert!(!env.repo.join("base/home/.config/nvim/lazy.lua").exists());
    assert!(
        !env.repo
            .join("base/home/.config/nvim/lua/mappings.lua")
            .exists()
    );
}

#[test]
fn remove_file_from_management() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Add a file
    let target = env.create_file(".vimrc", "set nocompatible");
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add vimrc"]);

    // Verify it's tracked and symlinked
    let tracked = env.tracked_files();
    assert!(tracked.contains(&"base/home/.vimrc".to_string()));
    assert!(target.is_symlink());

    // Remove it
    env.run_ok(&["remove", target.to_str().unwrap()]);

    // Symlink should be gone
    assert!(!target.is_symlink(), "symlink should be removed");

    // File should be restored as a regular file from repo
    assert!(target.is_file(), "file should be restored as regular file");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "set nocompatible"
    );
}

#[test]
fn remove_with_commit_stages_and_commits() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Add a file
    let target = env.create_file(".testrc", "test content");
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add testrc"]);

    assert!(target.is_symlink());

    // Remove with --commit
    env.run_ok(&[
        "remove",
        target.to_str().unwrap(),
        "--commit",
        "remove testrc",
    ]);

    // Symlink should be gone
    assert!(!target.is_symlink());

    // File should be restored
    assert!(target.is_file());

    // File should no longer be tracked
    let tracked = env.tracked_files();
    assert!(
        !tracked.contains(&"base/home/.testrc".to_string()),
        "file should be untracked after remove --commit: {:?}",
        tracked
    );

    // There should be a commit for the removal
    let log_output = std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log_output.stdout);
    assert!(
        log.contains("remove testrc"),
        "commit message not found in git log:\n{}",
        log
    );
}

#[test]
fn remove_dry_run_makes_no_changes() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let target = env.create_file(".vimrc", "set nocompatible");
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add vimrc"]);

    assert!(target.is_symlink());

    env.run_ok(&["remove", target.to_str().unwrap(), "--dry-run"]);

    // Symlink should still exist
    assert!(target.is_symlink(), "dry-run should not remove symlink");
}

#[test]
fn remove_unmanaged_path_fails() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file that is NOT managed by dotty
    let unmanaged = env.create_file(".unmanaged", "not tracked");

    let out = env.run_err(&["remove", unmanaged.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Path not managed by dotty"),
        "expected 'Path not managed by dotty' error:\n{}",
        stderr
    );
}

#[test]
fn remove_without_repo_fails() {
    let env = TestEnv::new();

    env.run_err(&["remove", "~/.vimrc"]);
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

#[test]
fn status_shows_machine_and_repo() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let out = env.run_ok(&["status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("testbox"),
        "machine name not shown:\n{}",
        stdout
    );
    assert!(
        stdout.contains(env.repo.to_str().unwrap()),
        "repo path not shown:\n{}",
        stdout
    );
}

#[test]
fn status_without_repo_fails() {
    let env = TestEnv::new();

    env.run_err(&["status"]);
}

#[test]
fn status_shows_broken_symlinks() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Add a file
    let target = env.create_file(".vimrc", "set nocompatible");
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add vimrc"]);

    // Verify managed map has the entry
    let config = env.read_config();
    assert!(
        config.contains("base/home/.vimrc"),
        "managed map missing entry after add:\n{}",
        config
    );

    // Now delete the repo file to create a broken symlink
    let repo_file = env.repo.join("base/home/.vimrc");
    std::fs::remove_file(&repo_file).unwrap();

    // Verify the symlink is still there but broken
    assert!(target.is_symlink(), "symlink should still exist");
    assert!(!repo_file.exists(), "repo file should be deleted");

    let out = env.run_ok(&["status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should report broken symlinks
    assert!(
        !stdout.contains("Broken:    0"),
        "broken symlinks not reported:\n{}",
        stdout
    );
}

#[test]
fn status_shows_git_dirty() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a tracked file
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

    // Modify the file to make git dirty
    std::fs::write(&repo_file, "set number\nset relativenumber").unwrap();

    let out = env.run_ok(&["status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should show dirty status (not "clean")
    assert!(
        !stdout.contains("Git:       clean"),
        "git dirty status not shown:\n{}",
        stdout
    );
}

#[test]
fn status_shows_inactive_tiers() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Pick a platform tier that is NOT active on the current OS
    let (inactive_tier, file_name, commit_msg) = if cfg!(target_os = "macos") {
        ("linux", "linux/home/.bashrc", "add linux bashrc")
    } else if cfg!(target_os = "linux") {
        (
            "macos",
            "macos/home/.bash_profile",
            "add macos bash_profile",
        )
    } else {
        // On other platforms, skip the test
        return;
    };

    let repo_file = env.repo.join(file_name);
    std::fs::create_dir_all(repo_file.parent().unwrap()).unwrap();
    std::fs::write(&repo_file, "export PATH=\"$PATH:/usr/local/bin\"").unwrap();

    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["add", file_name])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&env.repo)
        .args(["commit", "-m", commit_msg])
        .output()
        .unwrap();

    let out = env.run_ok(&["status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("Inactive:"),
        "inactive tiers not shown:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("Inactive:  0"),
        "{} tier should be reported as inactive:\n{}",
        inactive_tier,
        stdout
    );
    assert!(
        stdout.contains(inactive_tier),
        "inactive tier name not shown:\n{}",
        stdout
    );
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

#[test]
fn clean_no_backups() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let out = env.run_ok(&["clean"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("No backups"),
        "expected 'No backups' message:\n{}",
        stdout
    );
}

#[test]
fn clean_with_backups() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create some backup directories manually
    let backup_dir = env.state.join("backups");
    std::fs::create_dir_all(backup_dir.join("2024-01-01T00-00-00")).unwrap();
    std::fs::create_dir_all(backup_dir.join("2024-06-15T12-30-00")).unwrap();
    std::fs::create_dir_all(backup_dir.join("2024-12-31T23-59-59")).unwrap();

    // Verify backups exist
    assert_eq!(
        std::fs::read_dir(&backup_dir).unwrap().count(),
        3,
        "expected 3 backup dirs"
    );

    // Clean with --keep 1 (keep the most recent)
    let out = env.run_ok(&["clean", "--keep", "1", "--yes"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should have removed 2 of 2 targeted backups
    assert!(
        stdout.contains("Removed 2 of 2"),
        "expected 'Removed 2 of 2' message:\n{}",
        stdout
    );

    // Verify only the most recent backup remains
    let remaining: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0], "2024-12-31T23-59-59");
}

#[test]
fn clean_before_date() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create backup directories
    let backup_dir = env.state.join("backups");
    std::fs::create_dir_all(backup_dir.join("2024-01-01T00-00-00")).unwrap();
    std::fs::create_dir_all(backup_dir.join("2024-06-15T12-30-00")).unwrap();
    std::fs::create_dir_all(backup_dir.join("2024-12-31T23-59-59")).unwrap();

    // --before 2024-07-01 should target the first two
    let out = env.run_ok(&["clean", "--before", "2024-07-01", "--yes"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("Removed 2 of 2"),
        "expected 'Removed 2 of 2' message:\n{}",
        stdout
    );

    // Verify only the most recent backup remains
    let remaining: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0], "2024-12-31T23-59-59");
}

#[test]
fn clean_invalid_date_fails() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a backup directory so clean doesn't exit early
    let backup_dir = env.state.join("backups");
    std::fs::create_dir_all(backup_dir.join("2024-01-01T00-00-00")).unwrap();

    env.run_err(&["clean", "--before", "not-a-date"]);
}
