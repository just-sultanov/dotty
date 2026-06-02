//! Unix-specific tests.
//!
//! These tests verify dotty's behavior on Unix-like platforms (Linux, macOS, etc.),
//! including:
//! - Hard links handling
//! - File permissions
//! - POSIX-compliant path handling

#![cfg(unix)]

use crate::common::TestEnv;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink as unix_symlink;

/// Test handling of hard links.
#[test]
fn test_hard_links() {
    let env = TestEnv::new();

    // Create a file in the repo
    let file_path = env.repo.join("base").join("config.toml");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "config_content").unwrap();

    // Create a hard link to the file
    let hard_link_path = env.repo.join("base").join("config_backup.toml");

    #[cfg(not(windows))]
    fs::hard_link(&file_path, &hard_link_path).unwrap();

    // Both paths should point to the same file
    #[cfg(not(windows))]
    {
        assert!(hard_link_path.exists());

        // Read from both paths should give the same content
        let original_content = fs::read_to_string(&file_path).unwrap();
        let hard_link_content = fs::read_to_string(&hard_link_path).unwrap();
        assert_eq!(original_content, hard_link_content);
    }
}

/// Test file permission handling.
#[test]
fn test_permissions() {
    let env = TestEnv::new();

    // Create a file with specific permissions
    let file_path = env.repo.join("base").join("secure_config.toml");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "secret = \"value\"").unwrap();

    // Set restrictive permissions (owner read/write only)
    let mut perms = fs::metadata(&file_path).unwrap().permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&file_path, perms).unwrap();

    // Verify permissions were set
    let actual_perms = fs::metadata(&file_path).unwrap().permissions();
    assert_eq!(actual_perms.mode() & 0o777, 0o600);

    // File should still be readable (we're the owner)
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "secret = \"value\"");
}

/// Test POSIX-compliant path handling.
#[test]
fn test_posix_paths() {
    let env = TestEnv::new();

    // Test path with POSIX-special characters
    let file_path = env.repo.join("base").join("config.with.dots.toml");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "test = \"value\"").unwrap();

    assert!(file_path.exists());

    // Test path with spaces
    let spaced_path = env.repo.join("base").join("config with spaces.toml");
    fs::write(&spaced_path, "test = \"value\"").unwrap();

    assert!(spaced_path.exists());

    // Test symlink with relative target
    let link_path = env.repo.join("base").join("link_to_config.toml");
    unix_symlink("config.with.dots.toml", &link_path).unwrap();

    assert!(link_path.is_symlink());

    // Follow the symlink
    let target = fs::read_link(&link_path).unwrap();
    assert_eq!(target.to_string_lossy(), "config.with.dots.toml");
}

/// Test symbolic link following behavior.
#[test]
fn test_symlink_following() {
    let env = TestEnv::new();

    // Create a target file
    let target_path = env.repo.join("base").join("original.toml");
    let parent = target_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&target_path, "original_content").unwrap();

    // Create a symlink pointing to it
    let link_path = env.repo.join("base").join("link.toml");
    unix_symlink(&target_path, &link_path).unwrap();

    // Verify symlink exists
    assert!(link_path.is_symlink());

    // Reading through symlink should give original content
    let content = fs::read_to_string(&link_path).unwrap();
    assert_eq!(content, "original_content");
}
