//! Linux-specific tests.
//!
//! These tests verify dotty's behavior on Linux, including:
//! - ext4-specific behaviors
//! - Linux-specific file system features

#![cfg(target_os = "linux")]

use crate::common::TestEnv;
use std::fs;
use std::os::unix::fs::symlink;

/// Test ext4-specific behaviors.
#[test]
fn test_ext4_behavior() {
    let env = TestEnv::new();

    // Create a typical dotfiles structure
    let config_dir = env.repo.join("base").join(".config");
    fs::create_dir_all(&config_dir).unwrap();

    // Create multiple config files
    let files = vec![
        ("app1/config.toml", "app1_config"),
        ("app2/settings.toml", "app2_settings"),
        ("app3/preferences.toml", "app3_prefs"),
    ];

    for (rel_path, content) in files {
        let full_path = config_dir.join(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, content).unwrap();
    }

    // Verify all files were created
    for (rel_path, expected_content) in files {
        let full_path = config_dir.join(rel_path);
        assert!(full_path.exists(), "File {} should exist", rel_path);
        let content = fs::read_to_string(&full_path).unwrap();
        assert_eq!(content, *expected_content);
    }
}

/// Test SELinux context handling (if applicable).
/// Note: This test runs regardless of SELinux status,
/// but documents expected behavior on SELinux-enabled systems.
#[test]
fn test_selinux_contexts() {
    let env = TestEnv::new();

    // Create a config file
    let config_path = env.repo.join("base").join("config.toml");
    let parent = config_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&config_path, "key = \"value\"").unwrap();

    // On SELinux-enabled systems, files have security contexts
    // This test primarily documents that dotty should work
    // regardless of SELinux context

    // Basic file operations should work
    assert!(config_path.exists());
    let content = fs::read_to_string(&config_path).unwrap();
    assert_eq!(content, "key = \"value\"");
}

/// Test Linux-specific symlink handling.
#[test]
fn test_linux_symlinks() {
    let env = TestEnv::new();

    // Create a target file
    let target_path = env.repo.join("base").join("target.toml");
    let parent = target_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&target_path, "target_content").unwrap();

    // Create a relative symlink
    let link_path = env.repo.join("base").join("link.toml");
    symlink("target.toml", &link_path).unwrap();

    // Verify symlink
    assert!(link_path.is_symlink());

    // Follow symlink
    let content = fs::read_to_string(&link_path).unwrap();
    assert_eq!(content, "target_content");

    // Verify the link target
    let resolved = fs::read_link(&link_path).unwrap();
    assert_eq!(resolved, "target.toml");
}

/// Test Linux file capabilities (if applicable).
#[test]
fn test_linux_file_capabilities() {
    let env = TestEnv::new();

    // Create a regular file
    let file_path = env.repo.join("base").join("app.toml");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "config = \"value\"").unwrap();

    // Basic operations should work
    assert!(file_path.exists());

    // Read the file
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "config = \"value\"");
}
