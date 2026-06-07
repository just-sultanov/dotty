#![allow(missing_docs)]
//! Shared helpers for integration tests.
//!
//! Each test gets its own isolated temp directory for the dotty root
//! (`DOTTY_HOME`), with subdirectories for the repo (`dotfiles/`),
//! state (`state/`), config (`config/`), and backups (`backups/`).

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

/// A handle that owns a set of temp directories (dotty root + home) and
/// cleans them up on drop.
///
/// The dotty root directory (`DOTTY_HOME`) contains:
/// - `dotfiles/`  — the actual git repository (exposed as `repo`)
/// - `state/`     — pending plan files (exposed as `state`)
/// - `config/`    — configuration
/// - `backups/`   — backup storage
///
/// `home` is a separate directory that simulates the user's home directory.
/// It lives *outside* the repo so that the `add` command's self-reference
/// check does not trigger.
pub struct TestEnv {
    /// The repo directory under DOTTY_HOME (e.g. `<root>/dotfiles`).
    pub repo: PathBuf,
    /// The state directory under DOTTY_HOME (e.g. `<root>/state`).
    #[allow(dead_code)]
    pub state: PathBuf,
    /// Simulated home directory (outside the repo).
    pub home: PathBuf,
    _root_dir: tempfile::TempDir,
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
        let dotty_home = self._root_dir.path();
        let mut child = Command::new(Self::bin())
            .env("DOTTY_HOME", dotty_home)
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

    /// Read config.toml from the config directory under DOTTY_HOME.
    pub fn read_config(&self) -> String {
        let config_path = self._root_dir.path().join("config").join("config.toml");
        std::fs::read_to_string(config_path).unwrap_or_default()
    }

    /// Return the absolute path to the config.toml file.
    pub fn config_file(&self) -> PathBuf {
        self._root_dir.path().join("config").join("config.toml")
    }

    /// Return the absolute path to the backups directory.
    pub fn backups_dir(&self) -> PathBuf {
        self._root_dir.path().join("backups")
    }

    /// List tracked files in the repo.
    pub fn tracked_files(&self) -> Vec<String> {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(["-c", "core.quotepath=false", "ls-files"])
            .output()
            .expect("git ls-files");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    /// Run `git log --oneline -1 --format=%s` in the test repo.
    /// Returns the full commit subject line (trimmed), or panics on failure.
    pub fn git_log(&self) -> String {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(["log", "--oneline", "-1", "--format=%s"])
            .output()
            .expect("git log");
        assert!(
            out.status.success(),
            "git log failed (exit {})\nstderr: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
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
        let root_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        // repo and state dirs are created by `dotty init`, we just point to
        // where they'll be created.
        let repo = root_dir.path().join("dotfiles");
        let state = root_dir.path().join("state");
        Self {
            repo,
            state,
            home: home_dir.path().to_path_buf(),
            _root_dir: root_dir,
            _home_dir: home_dir,
        }
    }
}
