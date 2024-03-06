use clap::Args;

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

pub fn dispatch(cmd: Command) {
    println!("{:?}", cmd)
}
