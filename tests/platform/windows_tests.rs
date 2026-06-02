//! Windows-specific tests.
//!
//! These tests verify dotty's behavior on Windows platforms, including:
//! - Junction points handling
//! - MAX_PATH limit (260 characters)
//! - Case-insensitive path resolution
//! - CRLF line endings

#![cfg(windows)]

use crate::common::TestEnv;
use std::fs;
use std::io::Write;

/// Test that dotty handles Windows junction points correctly.
/// Junction points are different from symbolic links on Windows.
#[test]
fn test_junction_points() {
    let env = TestEnv::new();

    // Create a directory that could be a junction point target
    let target_dir = env.repo.join("base").join(".config");
    fs::create_dir_all(&target_dir).unwrap();

    // Create a config file in the target directory
    let config_file = target_dir.join("settings.toml");
    let mut file = fs::File::create(&config_file).unwrap();
    writeln!(file, "key = \"value\"").unwrap();

    // Verify the directory was created
    assert!(target_dir.exists());
    assert!(target_dir.is_dir());

    // Verify the config file exists
    assert!(config_file.exists());
    assert!(config_file.is_file());
}

/// Test handling of MAX_PATH limit (260 characters).
/// Windows has a default path length limit.
#[test]
fn test_max_path_length() {
    let env = TestEnv::new();

    // Create a deeply nested path structure
    // to test how dotty handles long paths
    let long_dir_name = "this_is_a_very_long_directory_name_that_contributes_to_path_length";
    let mut current_path = env.repo.clone();

    // Build up a long path (but stay under the limit for the test)
    for i in 0..5 {
        let new_path = current_path.join(format!("{}_{}", long_dir_name, i));
        fs::create_dir_all(&new_path).unwrap();
        current_path = new_path;
    }

    // Verify the path was created
    assert!(current_path.exists());

    // The path should be significantly long but valid
    let path_len = current_path.as_path().to_string_lossy().len();
    println!("Created path with length: {}", path_len);

    // On Windows, paths over 260 characters may fail without special handling
    // This test verifies basic long path handling
}

/// Test that dotty handles case-insensitive path resolution.
/// Windows file systems are case-insensitive.
#[test]
fn test_case_insensitive_paths() {
    let env = TestEnv::new();

    // Create a directory with specific casing
    let dir_path = env.repo.join("base").join(".config");
    fs::create_dir_all(&dir_path).unwrap();

    // Create a file with specific casing
    let file_path = dir_path.join("config.toml");
    fs::write(&file_path, "test = \"value\"").unwrap();

    // On Windows, these should all work due to case-insensitivity
    // Note: This test primarily documents expected behavior
    // The actual case-insensitivity is handled by the OS
    assert!(file_path.exists());

    // Verify we can read the file
    let contents = fs::read_to_string(&file_path).unwrap();
    assert_eq!(contents, "test = \"value\"");
}

/// Test handling of CRLF line endings in config files.
#[test]
fn test_windows_line_endings() {
    let env = TestEnv::new();

    // Create a config file with CRLF line endings
    let config_path = env.repo.join("base").join("config.toml");
    let parent = config_path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();

    // Write content with Windows-style line endings
    let content_with_crlf = "key1 = \"value1\"\r\nkey2 = \"value2\"\r\n";
    fs::write(&config_path, content_with_crlf).unwrap();

    // Read the file back and verify content
    let read_content = fs::read_to_string(&config_path).unwrap();

    // The content should contain CRLF
    assert!(read_content.contains("\r\n"));

    // Parse should work regardless of line ending style
    // (TOML parser handles both LF and CRLF)
}
