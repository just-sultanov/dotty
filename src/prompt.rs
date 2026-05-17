use std::io::IsTerminal;

use dialoguer::Confirm;

use crate::error::DottyError;

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
fn is_interactive() -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_interactive_returns_bool() {
        // In test environment (non-TTY), should return false
        assert!(!is_interactive());
    }

    #[test]
    fn test_require_interactive_fails_in_non_tty() {
        // In test environment (non-TTY), should return NotInteractive error
        let result = require_interactive();
        assert!(matches!(result, Err(DottyError::NotInteractive { .. })));
    }

    #[test]
    fn test_require_interactive_error_message() {
        let result = require_interactive();
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not running in an interactive terminal"));
        assert!(msg.contains("--dry-run"));
    }
}
