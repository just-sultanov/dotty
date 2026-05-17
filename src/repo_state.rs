use std::path::PathBuf;

use crate::config::Config;
use crate::convention::{read_config, resolve_repo_path, resolve_state_path};
use crate::error::DottyError;

/// Encapsulates repository state and precondition validation.
///
/// Centralizes the common setup logic shared by most commands:
/// resolving the repo and state paths, reading the config, and
/// checking whether the repository is a git repository.
pub(crate) struct RepoState {
    /// Absolute path to the dotty repository root.
    pub repo_path: PathBuf,
    /// Absolute path to the dotty state directory.
    pub state_path: PathBuf,
    /// Parsed configuration from state directory.
    pub config: Config,
    /// Whether the repository has been initialized with `git init`.
    pub is_git_repo: bool,
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
    /// Returns [`DottyError`] if the repo path or state path cannot be
    /// resolved, or if the config file cannot be read.
    pub fn new() -> Result<Self, DottyError> {
        let repo_path = resolve_repo_path()?;
        let state_path = resolve_state_path()?;
        let config = read_config(&state_path)?;
        let is_git_repo = repo_path.join(".git").exists();

        Ok(Self {
            repo_path,
            state_path,
            config,
            is_git_repo,
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

    /// Require a machine name to be configured.
    ///
    /// Returns the machine name if set in config, or an error suggesting
    /// the user configure one.
    ///
    /// # Errors
    ///
    /// Returns [`DottyError::MissingMachineName`] if no machine is set.
    #[allow(dead_code)]
    pub fn require_machine(&self) -> Result<&str, DottyError> {
        self.config
            .machine
            .as_deref()
            .ok_or(DottyError::MissingMachineName)
    }
}
