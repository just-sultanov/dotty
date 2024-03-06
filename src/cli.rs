pub mod commands;

use clap::{Parser, Subcommand};
use commands::init;

#[derive(Debug, Parser)]
#[command(name = "dotty")]
#[command(bin_name = "dotty")]
#[command(about = "An insanely simple dotfiles management tool", long_about = None)]
#[command(version, propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initializes the Dotty root directory.
    #[command(arg_required_else_help = true)]
    Init(init::Command),
}

pub fn dispatch() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init(cmd) => init::dispatch(cmd),
    }
}
