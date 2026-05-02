#![allow(dead_code)]

use dialoguer::Confirm;

use crate::error::DottyError;

/// Prompt the user for a yes/no confirmation.
///
/// Returns `true` if the user confirms, `false` otherwise.
pub fn prompt_confirm(prompt: &str) -> Result<bool, DottyError> {
    let answer = Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()?;
    Ok(answer)
}

/// Prompt the user for a text input.
///
/// Returns the entered string.
pub fn prompt_input(prompt: &str) -> Result<String, DottyError> {
    let input = dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .interact_text()?;
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
        .interact()?;
    Ok(index)
}
