//! macOS-specific tests.
//!
//! These tests verify dotty's behavior on macOS, including:
//! - HFS+/APFS case-insensitive behavior
//! - macOS-specific file system features

#![cfg(target_os = "macos")]

use crate::common::TestEnv;
use std::fs;

/// Test HFS+ case-insensitive behavior.
/// macOS typically uses case-insensitive file systems (Case-Insensitive HFS+ or APFS).
#[test]
fn test_hfs_case_insensitivity() {
    let env = TestEnv::new();

    // Create a file with specific casing
    let file_path = env.repo.join("base").join("Config.toml");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "key = \"value\"").unwrap();

    // On case-insensitive file systems, reading with different case should work
    // Note: This test documents expected behavior; actual result depends on
    // the underlying file system of the test environment

    // Try to read with different casing
    let alt_path = env.repo.join("base").join("config.toml");

    // On case-insensitive volumes, this will succeed
    // On case-sensitive volumes (like some CI environments), this may fail
    let exists_with_alt_case = alt_path.exists();

    // The original path should always work
    assert!(file_path.exists());

    // Document the behavior (but don't assert, as it depends on the volume)
    println!("Case-insensitive read succeeded: {}", exists_with_alt_case);
}

/// Test macOS-specific path resolution.
#[test]
fn test_macos_path_resolution() {
    let env = TestEnv::new();

    // Create a standard config structure
    let config_path = env.repo.join("base").join(".config").join("app");
    fs::create_dir_all(&config_path).unwrap();

    let config_file = config_path.join("settings.toml");
    fs::write(&config_file, "setting = \"value\"").unwrap();

    // Verify the structure was created
    assert!(config_path.is_dir());
    assert!(config_file.exists());

    // Read and verify content
    let content = fs::read_to_string(&config_file).unwrap();
    assert_eq!(content, "setting = \"value\"");
}

/// Test handling of macOS resource forks (if applicable).
#[test]
fn test_macos_metadata_handling() {
    let env = TestEnv::new();

    // Create a file that might have metadata
    let file_path = env.repo.join("base").join("document.txt");
    let parent = file_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::write(&file_path, "document content").unwrap();

    // Verify basic file operations work
    assert!(file_path.exists());

    // Read the file
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "document content");
}
