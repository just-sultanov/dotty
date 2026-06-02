//! Pending plan persistence for crash recovery.
//!
//! Contains [`save_pending_plan`], [`load_pending_plan`], and [`clear_pending_plan`]
//! which manage a JSON plan file on disk. If the process is killed during plan
//! execution, the pending plan file remains and can be used to recover on the
//! next run.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::err_msg;

use super::{Action, Plan};

/// Filename for the pending plan file inside the state directory.
const PENDING_PLAN_FILE: &str = "pending_plan.json";

/// A pending plan saved to disk for recovery after interrupted operations.
///
/// Serialized as JSON with the same schema as an `Action` enum (externally tagged)
/// because `Action` derives `Serialize + Deserialize`. PathBuf fields are serialized
/// as strings, so the format is identical to the previous `SerializableAction`-based
/// approach — existing plan files on disk remain compatible.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PendingPlan {
    /// Path to the dotty repository.
    repo_path: String,
    /// Actions that were planned but may not have completed.
    actions: Vec<Action>,
}

impl PendingPlan {
    /// Convert a `Plan` into a `PendingPlan` for serialization.
    pub(crate) fn from_plan(plan: &Plan) -> Self {
        Self {
            repo_path: plan.repo_path.to_string_lossy().to_string(),
            actions: plan.actions.clone(),
        }
    }

    /// Convert back to an executable `Plan`.
    fn to_plan(&self) -> Plan {
        Plan {
            repo_path: PathBuf::from(&self.repo_path),
            actions: self.actions.clone(),
        }
    }
}

/// Path to the pending plan file inside the state directory.
fn pending_plan_path(state_path: &Path) -> PathBuf {
    state_path.join(PENDING_PLAN_FILE)
}

/// Save a plan to disk before execution.
///
/// If the process is killed (SIGKILL, crash) during execution, the pending
/// plan file remains and can be used for recovery on the next run.
pub(crate) fn save_pending_plan(
    plan: &Plan,
    state_path: &Path,
) -> Result<(), crate::error::DottyError> {
    fs::create_dir_all(state_path)?;
    let pending = PendingPlan::from_plan(plan);
    let content = serde_json::to_string_pretty(&pending)?;
    // Atomic write: write to a temp file first, then rename into place.
    // If the process is killed mid-write, only the temp file is corrupted,
    // and the existing pending_plan.json (if any) remains intact.
    // `fs::rename` is atomic on POSIX when source and dest are on the same filesystem.
    let tmp_path = state_path.join("pending_plan.json.tmp");
    fs::write(&tmp_path, &content)?;
    fs::rename(&tmp_path, pending_plan_path(state_path))?;
    debug!(
        "saved pending plan to {}",
        state_path.join(PENDING_PLAN_FILE).display()
    );
    Ok(())
}

/// Load a pending plan from disk, if one exists.
///
/// Returns `Ok(None)` if no pending plan file exists.
///
/// **Integrity validation:** After deserializing the plan, checks that the
/// repository path still exists and is a valid git repository. This prevents
/// confusing errors during recovery when the repo has been moved or deleted.
pub(crate) fn load_pending_plan(
    state_path: &Path,
) -> Result<Option<Plan>, crate::error::DottyError> {
    let path = pending_plan_path(state_path);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let pending: PendingPlan = serde_json::from_str(&content)?;

    // Validate that the repository still exists and is a valid git repo.
    let repo_path = PathBuf::from(&pending.repo_path);
    if !repo_path.is_dir() {
        return Err(crate::error::DottyError::PendingPlanInvalid {
            reason: err_msg!("repository no longer exists at {}", repo_path.display()),
            source: None,
        });
    }
    if !repo_path.join(".git").exists() {
        return Err(crate::error::DottyError::PendingPlanInvalid {
            reason: err_msg!("path is not a git repository: {}", repo_path.display()),
            source: None,
        });
    }

    // Verify git can read the repo (catches corrupted .git dirs missing HEAD, broken refs, etc.).
    // `git rev-parse --git-dir` is extremely fast (<10ms) and safe — it only reads metadata.
    let output = crate::git::git_run_raw(&repo_path, &["rev-parse", "--git-dir"]).map_err(|e| {
        crate::error::DottyError::PendingPlanInvalid {
            reason: err_msg!(
                "git command failed for pending plan repo at {}",
                repo_path.display()
            ),
            source: Some(Box::new(e)),
        }
    })?;
    if !output.status.success() {
        return Err(crate::error::DottyError::PendingPlanInvalid {
            reason: err_msg!("repository at {} is corrupted", repo_path.display()),
            source: None,
        });
    }

    debug!("loaded pending plan from {}", path.display());
    Ok(Some(pending.to_plan()))
}

/// Remove the pending plan file (called after successful execution).
pub(crate) fn clear_pending_plan(state_path: &Path) -> Result<(), crate::error::DottyError> {
    let path = pending_plan_path(state_path);
    if path.exists() {
        fs::remove_file(&path)?;
        debug!("cleared pending plan at {}", path.display());
    }
    Ok(())
}
