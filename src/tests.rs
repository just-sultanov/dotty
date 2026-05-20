//! Shared test utilities for unit tests.

use std::path::PathBuf;

/// Temporarily set `HOME` to a unique temp directory and run the test closure.
pub fn with_test_home<F: FnOnce(&PathBuf)>(test: F)
where
    F: FnOnce(&PathBuf),
{
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    temp_env::with_var("HOME", Some(home.to_str().unwrap()), || {
        test(&home);
    });
}
