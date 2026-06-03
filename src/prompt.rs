use std::io::IsTerminal;

use dialoguer::Confirm;

use crate::error::DottyError;
use tracing::warn;

/// Map a `dialoguer::Error` to `DottyError`, converting cancellation
/// into a domain-specific `Cancelled` variant.
///
/// Note: dialoguer 0.12 represents cancellation as io::ErrorKind::Other
/// with message "user aborted". There is no public ErrorKind enum exposed.
fn map_dialoguer_error(e: dialoguer::Error) -> DottyError {
    if e.to_string().contains("aborted") {
        DottyError::Cancelled
    } else {
        DottyError::Prompt(e)
    }
}

/// Check if we are running in an interactive terminal.
///
/// Returns `false` in CI, pipes, or any non-TTY environment where
/// interactive prompts would hang or behave unpredictably.
///
/// This function is `pub(crate)` so command modules (e.g., `add.rs`)
/// can guard interactive prompts with an early-return for non-interactive
/// contexts, avoiding hangs in CI or scripted workflows.
pub(crate) fn is_interactive() -> bool {
    // In CI environments, never prompt — avoids hangs.
    if std::env::var_os("CI").is_some() {
        return false;
    }
    std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

/// Ensure we are in an interactive terminal, returning a helpful error if not.
fn require_interactive() -> Result<(), DottyError> {
    if !is_interactive() {
        return Err(DottyError::NotInteractive {
            hint: "Use --dry-run or run in an interactive terminal".into(),
        });
    }
    Ok(())
}

/// Prompt the user for a yes/no confirmation.
///
/// Returns `true` if the user confirms, `false` otherwise.
/// Returns `DottyError::NotInteractive` when not running in a TTY.
pub(crate) fn prompt_confirm(prompt: &str) -> Result<bool, DottyError> {
    require_interactive()?;
    let answer = Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()
        .map_err(map_dialoguer_error)?;
    Ok(answer)
}

/// Prompt the user for a text input.
///
/// Returns the entered string.
/// Returns `DottyError::NotInteractive` when not running in a TTY.
pub(crate) fn prompt_input(prompt: &str) -> Result<String, DottyError> {
    require_interactive()?;
    let input = dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .interact_text()
        .map_err(map_dialoguer_error)?;
    Ok(input)
}

/// Prompt the user to select from a list of options.
///
/// Returns the index of the selected option.
/// Returns `DottyError::NotInteractive` when not running in a TTY.
pub(crate) fn prompt_select(prompt: &str, options: &[&str]) -> Result<usize, DottyError> {
    require_interactive()?;
    let index = dialoguer::Select::new()
        .with_prompt(prompt)
        .items(options)
        .default(0)
        .interact()
        .map_err(map_dialoguer_error)?;
    Ok(index)
}

/// Prompt the user to select a machine from known machines or enter a new name.
///
/// Returns the selected or entered machine name.
/// Returns `DottyError::NotInteractive` when not running in a TTY.
pub(crate) fn prompt_machine_selection(known_machines: &[String]) -> Result<String, DottyError> {
    if known_machines.is_empty() {
        return prompt_input("What is this machine called? (e.g. macbook, ubuntu-work)");
    }

    let mut options: Vec<String> = known_machines.to_vec();
    options.push("(new)".to_string());

    let selected = prompt_select(
        "Which machine is this?",
        &options.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )?;

    if selected == options.len() - 1 {
        prompt_input("Enter a new machine name:")
    } else {
        Ok(options[selected].clone())
    }
}

/// Prompt the user to confirm orphan removal.
///
/// Lists each orphan's repo_relative_path and target path, then asks for confirmation.
///
/// Returns:
/// - `true` if the user confirms (proceed with removal)
/// - `false` if the user declines (skip removal)
/// - `Ok(false)` if not in an interactive context (graceful skip, no error)
/// - `Err(DottyError::Cancelled)` if the prompt is cancelled
///
/// In non-interactive environments (CI, pipes), this returns `Ok(false)`
/// instead of erroring, silently skipping orphan removal to avoid hangs.
pub(crate) fn prompt_orphan_removal(orphans: &[(String, String)]) -> Result<bool, DottyError> {
    if !is_interactive() {
        warn!(
            "non-interactive context: skipping orphan removal for {} orphan(s)",
            orphans.len()
        );
        for (repo_relative_path, target) in orphans {
            warn!("orphan: {} → {}", repo_relative_path, target);
        }
        return Ok(false);
    }

    // Build a human-readable list of orphans
    let mut list =
        String::from("The following files have no tracked source and will be removed:\n");
    for (repo_relative_path, target) in orphans {
        list.push_str(&format!("  - {} → {}\n", repo_relative_path, target));
    }

    let prompt = format!("{}\nRemove these {} orphan(s)?", list, orphans.len());

    let confirmed = prompt_confirm(&prompt)?;
    Ok(confirmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that prompt_orphan_removal returns Ok(false) in non-interactive mode.
    #[test]
    fn test_prompt_orphan_removal_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let orphans = vec![
                ("base/home/.old".to_string(), "/home/user/.old".to_string()),
                (
                    "base/home/.backup".to_string(),
                    "/home/user/.backup".to_string(),
                ),
            ];
            let result = prompt_orphan_removal(&orphans);
            assert!(result.is_ok(), "should not error in non-interactive mode");
            assert_eq!(
                result.unwrap(),
                false,
                "should return false (skip) in non-interactive mode"
            );
        });
    }

    /// Test that prompt_orphan_removal returns Ok(false) with empty orphans list.
    #[test]
    fn test_prompt_orphan_removal_empty() {
        temp_env::with_var("CI", Some("1"), || {
            let orphans: Vec<(String, String)> = vec![];
            let result = prompt_orphan_removal(&orphans);
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), false);
        });
    }

    /// Test that is_interactive returns false when CI is set.
    #[test]
    fn test_is_interactive_ci() {
        temp_env::with_var("CI", Some("1"), || {
            assert!(!is_interactive(), "should not be interactive when CI=1");
        });
    }

    /// Test that is_interactive returns false when CI is set to empty string.
    #[test]
    fn test_is_interactive_ci_empty() {
        temp_env::with_var("CI", Some(""), || {
            assert!(
                !is_interactive(),
                "should not be interactive when CI is set"
            );
        });
    }

    /// Test map_dialoguer_error: aborted error maps to Cancelled.
    #[test]
    fn test_map_dialoguer_error_aborted() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "user aborted");
        let err = dialoguer::Error::from(io_err);
        let mapped = map_dialoguer_error(err);
        assert!(matches!(mapped, DottyError::Cancelled));
    }

    /// Test map_dialoguer_error: non-aborted error maps to Prompt.
    #[test]
    fn test_map_dialoguer_error_other() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe");
        let err = dialoguer::Error::from(io_err);
        let mapped = map_dialoguer_error(err);
        assert!(matches!(mapped, DottyError::Prompt(_)));
    }

    /// Test require_interactive returns NotInteractive in CI.
    #[test]
    fn test_require_interactive_not_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let result = require_interactive();
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }

    /// Test prompt_confirm returns NotInteractive in CI.
    #[test]
    fn test_prompt_confirm_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let result = prompt_confirm("test prompt");
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }

    /// Test prompt_input returns NotInteractive in CI.
    #[test]
    fn test_prompt_input_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let result: Result<String, DottyError> = prompt_input("test input");
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }

    /// Test prompt_select returns NotInteractive in CI.
    #[test]
    fn test_prompt_select_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let result = prompt_select("test select", &["a", "b"]);
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }

    /// Test prompt_machine_selection returns NotInteractive in CI.
    #[test]
    fn test_prompt_machine_selection_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let result = prompt_machine_selection(&[]);
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }

    /// Test prompt_machine_selection with known machines in CI returns NotInteractive.
    #[test]
    fn test_prompt_machine_selection_known_machines_non_interactive() {
        temp_env::with_var("CI", Some("1"), || {
            let known = vec!["macbook".to_string(), "server".to_string()];
            let result = prompt_machine_selection(&known);
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DottyError::NotInteractive { .. }
            ));
        });
    }
}
