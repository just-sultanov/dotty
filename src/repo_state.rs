use std::path::PathBuf;

use crate::config::{Config, read_config};
use crate::error::DottyError;
use crate::paths::resolve_dotty_home;

/// Encapsulates repository state and precondition validation.
///
/// This struct is `Send + Sync` and can be safely shared across threads.
/// All fields are either `PathBuf`, `bool`, or `Config`, which are
/// thread-safe types. This enables potential parallelization of repository
/// operations in the future.
///
/// Centralizes the common setup logic shared by most commands:
/// resolving the repo and state paths, reading the config, and
/// checking whether the repository is a git repository.
#[derive(Clone)]
pub(crate) struct RepoState {
    /// Absolute path to the dotty repository root (under `$DOTTY_HOME`).
    pub repo_path: PathBuf,
    /// Absolute path to the dotty state directory (`$DOTTY_HOME/state`).
    pub state_path: PathBuf,
    /// Absolute path to the dotty config directory (`$DOTTY_HOME/config`).
    pub config_path: PathBuf,
    /// Absolute path to the dotty backups directory (`$DOTTY_HOME/backups`).
    pub backups_path: PathBuf,
    /// Parsed configuration from config directory.
    pub config: Config,
    /// Whether the repository has been initialized with `git init`.
    pub is_git_repo: bool,
    /// Whether the git identity (user.name / user.email) has been validated.
    ///
    /// Cached to avoid spawning two `git config` subprocesses on every
    /// commit operation. Set to `true` after the first successful
    /// `git config user.name` and `git config user.email` check.
    /// Reset via [`reset_git_identity`](Self::reset_git_identity) if
    /// the user manually changes git config during a session.
    pub git_identity_valid: bool,
}

impl RepoState {
    /// Create a new `RepoState` by resolving paths and reading config.
    ///
    /// This is the basic constructor used by all commands. It does **not**
    /// require the repository to be a git repository — use [`require_git()`]
    /// for that check.
    ///
    /// # Errors
    ///
    /// Returns [`DottyError`] if the dotty home cannot be resolved,
    /// or if the config file cannot be read.
    pub fn new() -> Result<Self, DottyError> {
        let dotty_home = resolve_dotty_home()?;
        let state_path = dotty_home.join("state");
        let config_path = dotty_home.join("config");
        let config = read_config(&config_path)?;
        let repo_name = config.repo_name.as_deref().unwrap_or("dotfiles");
        let repo_path = dotty_home.join(repo_name);
        let backups_path = dotty_home.join("backups");
        let is_git_repo = repo_path.join(".git").exists();

        Ok(Self {
            repo_path,
            state_path,
            config_path,
            backups_path,
            config,
            is_git_repo,
            git_identity_valid: false,
        })
    }

    /// Require the repository to be a git repository.
    ///
    /// Returns a reference to `self` if `.git` exists, or an error
    /// instructing the user to run `dotty init` first.
    ///
    /// # Errors
    ///
    /// Returns [`DottyError::MissingGitRepository`] if the repo is not
    /// a git repository.
    pub fn require_git(&self) -> Result<&Self, DottyError> {
        if self.is_git_repo {
            Ok(self)
        } else {
            Err(DottyError::MissingGitRepository {
                path: self.repo_path.clone(),
            })
        }
    }

    /// Validate git identity (user.name and user.email) and cache the result.
    ///
    /// On the first call, spawns two `git config` subprocesses to check
    /// that both `user.name` and `user.email` are set and non-empty.
    /// The result is cached in `git_identity_valid` so subsequent calls
    /// skip the subprocess overhead (~10-20ms per commit).
    ///
    /// If `git_identity_valid` is already `true`, this returns `Ok(())`
    /// immediately without spawning any subprocesses.
    ///
    /// # Errors
    ///
    /// Returns [`DottyError::Git`] with exit code 127 if the identity
    /// is not configured, including actionable guidance.
    pub fn validate_git_identity(&mut self) -> Result<(), DottyError> {
        if self.git_identity_valid {
            return Ok(());
        }

        let name = crate::git::git_run(&self.repo_path, &["config", "user.name"]);
        let email = crate::git::git_run(&self.repo_path, &["config", "user.email"]);

        match (name, email) {
            (Ok(n), Ok(e)) if !n.trim().is_empty() && !e.trim().is_empty() => {
                self.git_identity_valid = true;
                Ok(())
            }
            _ => Err(DottyError::Git {
                exit_code: 127,
                stderr: "Git identity is not configured. Run `git config user.name 'Your Name'` and `git config user.email 'you@example.com'`".into(),
            }),
        }
    }

    /// Reset the cached git identity validation.
    ///
    /// Call this if the user has manually changed git config during a
    /// session and you want the next [`validate_git_identity`](Self::validate_git_identity)
    /// call to re-check.
    #[cfg(test)]
    pub fn reset_git_identity(&mut self) {
        self.git_identity_valid = false;
    }

    /// Create a minimal `RepoState` for git operations that don't need config.
    ///
    /// Uses an empty config and assumes the repo is a valid git repository.
    /// Useful for crash recovery where config is not needed.
    pub fn new_for_git(
        repo_path: PathBuf,
        state_path: PathBuf,
        config_path: PathBuf,
        backups_path: PathBuf,
    ) -> Self {
        Self {
            repo_path,
            state_path,
            config_path,
            backups_path,
            config: Config::new(),
            is_git_repo: true,
            git_identity_valid: false,
        }
    }

    /// Convenience constructor for tests — creates state/config/backups as
    /// subdirectories of `repo_path`.
    #[cfg(test)]
    pub fn new_for_git_test(repo_path: PathBuf, state_path: PathBuf) -> Self {
        let config_path = repo_path.join("config");
        let backups_path = repo_path.join("backups");
        Self::new_for_git(repo_path, state_path, config_path, backups_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `RepoState` is `Send`.
    #[test]
    fn test_repstate_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RepoState>();
    }

    /// Verify that `RepoState` is `Sync`.
    #[test]
    fn test_repstate_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<RepoState>();
    }
}
