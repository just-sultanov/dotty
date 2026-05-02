mod cli;
mod commands;
mod convention;
mod git;
mod prompt;
mod symlink;

use clap::Parser;
use cli::{Cli, Commands, ConfigCommands};

fn main() {
    let cli = Cli::parse();

    let exit_code = match cli.command {
        Commands::Init { git_url, machine } => commands::init::run(git_url, machine),
        Commands::Config { command } => match command {
            ConfigCommands::Machine { name } => commands::config::set_machine(name),
        },
        Commands::Add {
            path,
            machine,
            platform,
            commit,
            dry_run,
        } => commands::add::run(path, machine, platform, commit, dry_run),
        Commands::Remove {
            path,
            machine,
            dry_run,
        } => commands::remove::run(path, machine, dry_run),
        Commands::Apply { dry_run } => commands::apply::run(dry_run),
        Commands::Status => commands::status::run(),
        Commands::Clean { keep, before } => commands::clean::run(keep, before),
    };

    std::process::exit(exit_code);
}
