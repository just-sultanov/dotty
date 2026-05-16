//! Shared helpers for integration tests.
//!
//! Each test gets its own isolated temp directory for both the repo
//! (`DOTTY_HOME`) and the state (`DOTTY_STATE_HOME`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A handle that owns a set of temp directories (repo + state + home) and
/// cleans them up on drop.
///
/// `home` is a separate directory that simulates the user's home directory.
/// It lives *outside* the repo so that the `add` command's self-reference
/// check does not trigger.
pub struct TestEnv {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub home: PathBuf,
    _repo_dir: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    _home_dir: tempfile::TempDir,
}

#[allow(dead_code)]
impl TestEnv {
    /// Create a fresh set of temp directories.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the binary path (built by `cargo test`).
    fn bin() -> &'static str {
        env!("CARGO_BIN_EXE_dotty")
    }

    /// Configure git identity in the repo so that `git commit` works.
    pub fn git_config_identity(&self) {
        Command::new("git")
            .current_dir(&self.repo)
            .args(["config", "user.name", "Test"])
            .output()
            .ok();
        Command::new("git")
            .current_dir(&self.repo)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .ok();
    }

    /// Run `dotty` with the isolated environment.
    ///
    /// `HOME` is set to the test `home` directory so that `repo_to_target()`
    /// maps `base/home/...` → `<test-home>/...` instead of the real `~`.
    pub fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(Self::bin())
            .env("DOTTY_HOME", &self.repo)
            .env("DOTTY_STATE_HOME", &self.state)
            .env("HOME", &self.home)
            .args(args)
            .output()
            .expect("failed to spawn dotty")
    }

    /// Run `dotty` and assert it succeeded (exit code 0).
    pub fn run_ok(&self, args: &[&str]) -> std::process::Output {
        let out = self.run(args);
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            panic!(
                "dotty {:?} failed (exit {})\nstdout: {}\nstderr: {}",
                args, out.status, stdout, stderr,
            );
        }
        out
    }

    /// Run `dotty` and assert it failed (non-zero exit code).
    pub fn run_err(&self, args: &[&str]) -> std::process::Output {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "expected failure but dotty {:?} succeeded",
            args
        );
        out
    }

    /// Convenience: create a file inside the simulated home directory.
    pub fn create_file(&self, rel_path: &str, content: &str) -> PathBuf {
        let full = self.home.join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        full
    }

    /// Create a file at an arbitrary path.
    pub fn create_file_at(&self, path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Read config.toml from the state directory.
    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.state.join("config.toml")).unwrap_or_default()
    }

    /// List tracked files in the repo.
    pub fn tracked_files(&self) -> Vec<String> {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(["ls-files"])
            .output()
            .expect("git ls-files");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    /// Check if a path is a symlink pointing to the expected target.
    pub fn assert_symlink(&self, link: &Path, expected_target: &Path) {
        assert!(link.is_symlink(), "{} is not a symlink", link.display());
        let actual = std::fs::read_link(link).expect("read_link");
        assert_eq!(
            actual,
            expected_target,
            "symlink {} points to {} but expected {}",
            link.display(),
            actual.display(),
            expected_target.display()
        );
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        let repo_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        Self {
            repo: repo_dir.path().to_path_buf(),
            state: state_dir.path().to_path_buf(),
            home: home_dir.path().to_path_buf(),
            _repo_dir: repo_dir,
            _state_dir: state_dir,
            _home_dir: home_dir,
        }
    }
}
