//! Phase 4 — `add` integration tests.

mod common;
use common::TestEnv;

// ---------------------------------------------------------------------------
// add — special characters in filenames
// ---------------------------------------------------------------------------

#[test]
fn add_file_with_spaces_in_name() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file with spaces in the path
    let target = env.create_file(".config/my app/config", "app setting");

    env.run_ok(&[
        "add",
        target.to_str().unwrap(),
        "--commit",
        "add app config",
    ]);

    let tracked = env.tracked_files();
    assert!(
        tracked.contains(&"base/home/.config/my app/config".to_string()),
        "file with spaces not tracked: {:?}",
        tracked
    );

    env.assert_symlink(&target, &env.repo.join("base/home/.config/my app/config"));
}

#[test]
fn add_file_with_unicode_name() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file with unicode in the name
    let target = env.create_file(".config/файл.txt", "unicode content");

    env.run_ok(&[
        "add",
        target.to_str().unwrap(),
        "--commit",
        "add unicode file",
    ]);

    let tracked = env.tracked_files();
    // git ls-files may quote non-ASCII paths, so check for the file name
    // appearing in some form (quoted or not)
    let has_file = tracked
        .iter()
        .any(|f| f.contains("файл.txt") || f.contains("321") || f.contains(".txt"));
    assert!(
        has_file,
        "file with unicode name not tracked: {:?}",
        tracked
    );

    assert!(target.is_symlink(), "unicode file should be symlinked");
}

#[test]
fn add_file_with_quotes_in_name() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file with a quote in the name
    let target = env.create_file(".config/it's a config", "quoted content");

    env.run_ok(&[
        "add",
        target.to_str().unwrap(),
        "--commit",
        "add quoted file",
    ]);

    let tracked = env.tracked_files();
    assert!(
        tracked.iter().any(|f| f.contains("it's a config")),
        "file with quotes not tracked: {:?}",
        tracked
    );

    assert!(target.is_symlink(), "quoted file should be symlinked");
}

#[test]
fn add_remove_file_with_special_chars_roundtrip() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Add a file with spaces
    let target = env.create_file(".config/my app/settings", "setting=value");
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add settings"]);

    assert!(target.is_symlink());

    // Remove it
    env.run_ok(&["remove", target.to_str().unwrap()]);

    // Symlink should be gone, regular file restored
    assert!(!target.is_symlink(), "symlink should be removed");
    assert!(target.is_file(), "file should be restored as regular file");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "setting=value");
}

#[test]
fn add_single_file_to_base() {
    let env = TestEnv::new();

    // Init repo
    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Create a file to add
    let target = env.create_file(".vimrc", "set nocompatible");

    // Add it (using --commit to stage+commit in one go)
    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add vimrc"]);

    // File copied into repo at base/home/.vimrc
    let tracked = env.tracked_files();
    assert!(
        tracked.contains(&"base/home/.vimrc".to_string()),
        "base/home/.vimrc not tracked: {:?}",
        tracked
    );

    // Target is now a symlink pointing to the repo file
    env.assert_symlink(&target, &env.repo.join("base/home/.vimrc"));
}

#[test]
fn add_file_to_machine_tier() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "mybox"]);
    env.git_config_identity();

    // Create machine directory first (add will prompt, so we pre-create)
    std::fs::create_dir_all(env.repo.join("mybox/home")).unwrap();

    let target = env.create_file(".gitconfig", "[user]\n  name = Test");

    env.run_ok(&[
        "add",
        target.to_str().unwrap(),
        "--machine",
        "mybox",
        "--commit",
        "add gitconfig",
    ]);

    let tracked = env.tracked_files();
    assert!(
        tracked.contains(&"mybox/home/.gitconfig".to_string()),
        "mybox/home/.gitconfig not tracked: {:?}",
        tracked
    );

    env.assert_symlink(&target, &env.repo.join("mybox/home/.gitconfig"));
}

#[test]
fn add_file_to_platform_tier() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let target = env.create_file(".config/skhd/skhdrc", "ctrl + q: kill");

    env.run_ok(&[
        "add",
        target.to_str().unwrap(),
        "--platform",
        "macos",
        "--commit",
        "add skhdrc",
    ]);

    let tracked = env.tracked_files();
    assert!(
        tracked.contains(&"macos/home/.config/skhd/skhdrc".to_string()),
        "macos/home/.config/skhd/skhdrc not tracked: {:?}",
        tracked
    );
}

#[test]
fn add_directory_recursively() {
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
    std::fs::write(
        sub.join("mappings.lua"),
        "vim.keymap.set('n', '<leader>w', '<C-w>')",
    )
    .unwrap();

    env.run_ok(&["add", dir.to_str().unwrap(), "--commit", "add nvim config"]);

    let tracked = env.tracked_files();
    assert!(tracked.contains(&"base/home/.config/nvim/init.lua".to_string()));
    assert!(tracked.contains(&"base/home/.config/nvim/lazy.lua".to_string()));
    assert!(tracked.contains(&"base/home/.config/nvim/lua/mappings.lua".to_string()));
}

#[test]
fn add_dry_run_makes_no_changes() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let target = env.create_file(".testrc", "dry run test");

    env.run_ok(&["add", target.to_str().unwrap(), "--dry-run"]);

    // No files tracked
    let tracked = env.tracked_files();
    assert!(
        tracked.is_empty(),
        "dry-run should not track files: {:?}",
        tracked
    );

    // Target is still a regular file, not a symlink
    assert!(!target.is_symlink(), "dry-run should not create symlinks");
}

#[test]
fn add_rejects_files_inside_repo() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    // Try to add a file that is inside the dotty repo itself
    let inside = env.repo.join("base/home/.inside");
    std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
    std::fs::write(&inside, "should fail").unwrap();

    env.run_err(&["add", inside.to_str().unwrap()]);
}

#[test]
fn add_nonexistent_path_fails() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    env.run_err(&["add", "/tmp/dotty_nonexistent_file_xyz123.txt"]);
}

#[test]
fn add_updates_managed_map() {
    let env = TestEnv::new();

    env.run_ok(&["init", "--machine", "testbox"]);
    env.git_config_identity();

    let target = env.create_file(".testrc", "managed map test");

    env.run_ok(&["add", target.to_str().unwrap(), "--commit", "add testrc"]);

    let config = env.read_config();
    // The managed map should contain the repo path
    assert!(
        config.contains("base/home/.testrc"),
        "managed map missing entry:\n{}",
        config
    );
}
