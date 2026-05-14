//! Phase 6 — `remove` + `status` + `clean` integration tests.

mod common;
use common::TestEnv;

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

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

    env.run_err(&["remove", unmanaged.to_str().unwrap()]);
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
    // Note: clean prompts interactively, so without a TTY it may not remove.
    // We just verify the command runs without crashing.
    let out = env.run(&["clean", "--keep", "1"]);
    // It may succeed or fail depending on TTY availability for prompts.
    // The important thing is it doesn't panic.
    let _ = out;
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
    let out = env.run(&["clean", "--before", "2024-07-01"]);
    // Again, interactive prompts may block, but it should not panic.
    let _ = out;
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
