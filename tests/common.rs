#![allow(missing_docs)]
//! Shared helpers for integration tests.
//!
//! Each test gets its own isolated temp directory for both the repo
//! (`DOTTY_HOME`) and the state (`DOTTY_STATE_HOME`).
//!
//! Test output is automatically captured to reduce noise during test runs.
//! Use `TestOutput::stdout_contains()` and `stderr_contains()` to assert on output.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Captured output from a test command.
///
/// Provides convenient assertion methods for checking stdout/stderr content.
#[derive(Debug)]
#[allow(dead_code)]
pub struct TestOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

#[allow(dead_code)]
impl TestOutput {
    /// Check if the command succeeded (exit code 0).
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Assert that stdout contains the given needle.
    ///
    /// Returns self for fluent chaining.
    pub fn stdout_contains(&self, needle: &str) -> &Self {
        assert!(
            self.stdout.contains(needle),
            "stdout doesn't contain '{}': {}",
            needle,
            self.stdout
        );
        self
    }

    /// Assert that stderr contains the given needle.
    ///
    /// Returns self for fluent chaining.
    pub fn stderr_contains(&self, needle: &str) -> &Self {
        assert!(
            self.stderr.contains(needle),
            "stderr doesn't contain '{}': {}",
            needle,
            self.stderr
        );
        self
    }

    /// Assert that stdout contains the given needle (alias for stdout_contains).
    pub fn contains(&self, needle: &str) -> &Self {
        self.stdout_contains(needle)
    }

    /// Get stdout as a lossy string slice.
    pub fn stdout_lossy(&self) -> std::string::String {
        self.stdout.clone()
    }

    /// Get stderr as a lossy string slice.
    pub fn stderr_lossy(&self) -> std::string::String {
        self.stderr.clone()
    }
}

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
/// Test helper methods — some may not be used in every test file.
/// Rust compiles each test as a separate crate, so dead-code detection
/// doesn't work across test files. This is a common pattern for test helpers.
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
    ///
    /// Output is captured to reduce noise during test runs.
    pub fn run(&self, args: &[&str]) -> TestOutput {
        let mut child = Command::new(Self::bin())
            .env("DOTTY_HOME", &self.repo)
            .env("DOTTY_STATE_HOME", &self.state)
            .env("HOME", &self.home)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn dotty");

        let mut stdout = String::new();
        let mut stderr = String::new();

        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();

        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();

        let status = child.wait().unwrap();

        TestOutput {
            stdout,
            stderr,
            status,
        }
    }

    /// Run `dotty` and assert it succeeded (exit code 0).
    pub fn run_ok(&self, args: &[&str]) -> TestOutput {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "dotty {:?} failed (exit {})\nstdout: {}\nstderr: {}",
            args,
            out.status.code().unwrap_or(-1),
            out.stdout,
            out.stderr,
        );
        out
    }

    /// Run `dotty` and assert it failed (non-zero exit code).
    pub fn run_err(&self, args: &[&str]) -> TestOutput {
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
