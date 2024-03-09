use clap::Args;

use crate::config;
use crate::fs;
use crate::git;

/// Initializes the Dotty home directory.
/// Default: `$HOME/.dotty`
#[derive(Debug, Args)]
pub struct Command {
    /// Git URL of the remote repository with dotfiles
    #[arg(value_name = "remote", required = false, long)]
    remote: Option<String>,
    /// Overwrite an existing root directory (default: `false`)
    #[arg(value_name = "overwrite", required = false, long)]
    overwrite: bool,
}

/// Initializes the Dotty home directory
pub fn dispatch(cmd: Command) {
    // dotty home
    let home = config::dotty_home().unwrap();

    // cleanup
    if cmd.overwrite {
        println!("Overwriting...");
        fs::remove_dir_all(home.clone());
    }

    // init
    println!("Initializing...");
    match cmd.remote {
        // use the specified remote git repository
        Some(remote) => git::clone(remote, home),
        // use an empty git repository
        None => git::init(home),
    };
}
