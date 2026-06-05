//! Shared test utilities for unit tests.

use std::path::PathBuf;

/// Temporarily set `$HOME` to a unique temp directory and run the test closure.
///
/// On all platforms `home_dir()` checks `$HOME` first, so setting it here
/// ensures tests use the temp directory as the home regardless of platform.
/// Also sets `$USERPROFILE` on Windows for any code that bypasses `home_dir()`.
pub fn with_test_home<F: FnOnce(&PathBuf)>(test: F) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let home_str = home.to_str().unwrap();

    #[cfg(target_os = "windows")]
    {
        temp_env::with_vars(
            [("HOME", Some(home_str)), ("USERPROFILE", Some(home_str))],
            || test(&home),
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        temp_env::with_var("HOME", Some(home_str), || test(&home));
    }
}
