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

/// Prompt the user for a yes/no confirmation.
///
/// Returns `true` if the user confirms, `false` otherwise.
pub(crate) fn prompt_confirm(prompt: &str) -> Result<bool, DottyError> {
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
pub fn prompt_input(prompt: &str) -> Result<String, DottyError> {
    let input = dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .interact_text()
        .map_err(map_dialoguer_error)?;
    Ok(input)
}

/// Prompt the user to select from a list of options.
///
/// Returns the index of the selected option.
pub fn prompt_select(prompt: &str, options: &[&str]) -> Result<usize, DottyError> {
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
pub fn prompt_machine_selection(known_machines: &[String]) -> Result<String, DottyError> {
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
