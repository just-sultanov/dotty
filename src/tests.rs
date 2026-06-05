//! Shared test utilities for unit tests.

use std::path::PathBuf;

/// Temporarily set the platform-specific home env var to a unique temp directory and run the test closure.
///
/// On Unix uses `HOME`; on Windows uses `USERPROFILE`.
pub fn with_test_home<F: FnOnce(&PathBuf)>(test: F) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    #[cfg(target_os = "windows")]
    let var = "USERPROFILE";
    #[cfg(not(target_os = "windows"))]
    let var = "HOME";

    temp_env::with_var(var, Some(home.to_str().unwrap()), || {
        test(&home);
    });
}
