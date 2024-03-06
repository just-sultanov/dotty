use clap::Args;
use std::path::PathBuf;

use crate::config;
use crate::fs;
use crate::git;

#[derive(Debug, Args)]
pub struct Command {
    /// Root directory (default: `$HOME/.dotty`)
    #[arg(value_name = "root", required = false, long)]
    root: Option<String>,
    /// Git URL of the remote repository with dotfiles
    #[arg(value_name = "remote", required = false, long)]
    remote: Option<String>,
    /// Overwrite an existing root directory (default: `false`)
    #[arg(value_name = "overwrite", required = false, long)]
    overwrite: bool,
}

/// Initializes the Dotty root directory
pub fn dispatch(cmd: Command) {
    // calculate root
    let root = match cmd.root {
        // use the specified path to the root directory
        Some(s) => PathBuf::from(s),
        // use the default root directory
        None => config::root_path().unwrap(),
    };

    // cleanup
    if cmd.overwrite {
        println!("Overwriting...");
        fs::remove_dir_all(root.clone());
    }

    println!("Initializing...");
    // init
    match cmd.remote {
        // use the specified remote git repository
        Some(remote) => git::clone(remote, root),
        // use an empty git repository
        None => git::init(root),
    };
}
