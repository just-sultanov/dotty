use clap::{ArgAction, Parser, Subcommand};

/// A minimal dotfiles manager for multiple machines.
#[derive(Parser, Debug)]
#[command(
    name = "dotty",
    version = env!("DOTTY_VERSION"),
    disable_version_flag = true,
    about = "A minimal dotfiles manager for multiple machines",
    before_help = concat!(
        "dotty v",
        env!("DOTTY_VERSION"),
        "@",
        env!("DOTTY_GIT_SHA"),
        " (",
        env!("DOTTY_BUILT_AT"),
        ")"
    ),
    help_template = "{before-help}{about}\n\n{usage-heading}\n  {usage}\n\n{all-args}{after-help}",
)]
pub struct Cli {
    /// Print version information
    #[arg(long, action = ArgAction::Version)]
    version: (),

    /// Verbose output (show debug logs)
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Quiet output (suppress non-essential logs)
    #[arg(long, short, global = true)]
    quiet: bool,

    /// Skip pending-plan recovery prompt (proceed with current command)
    #[arg(long, global = true)]
    recover: bool,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Return true if verbose mode is enabled.
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }

    /// Return true if quiet mode is enabled.
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// Return true if pending-plan recovery should be skipped.
    pub fn skip_recovery(&self) -> bool {
        self.recover
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Bootstrap a new repository or clone an existing one
    #[command(
        after_help = "Examples:\n  # Create a fresh repository in the current directory\n  dotty init\n\n  # Clone an existing dotty repository\n  dotty init git@github.com:user/dotty.git\n\n  # Clone and set machine name in one step\n  dotty init git@github.com:user/dotty.git --machine macbook"
    )]
    Init {
        /// Git URL to clone (optional — omit for fresh repo)
        git_url: Option<String>,

        /// Machine name (optional — will prompt if omitted)
        #[arg(long)]
        machine: Option<String>,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Add a file or directory to the repository
    #[command(
        after_help = "Examples:\n  # Add a config file to the base tier\n  dotty add ~/.vimrc\n\n  # Add to a machine-specific tier\n  dotty add ~/.vimrc --machine macbook\n\n  # Add to a platform-specific tier\n  dotty add ~/.bashrc --platform linux\n\n  # Add and commit in one step\n  dotty add ~/.config/alacritty --commit \"add alacritty config\""
    )]
    Add {
        /// Path to add (file or directory)
        path: String,

        /// Add to a specific machine tier
        #[arg(long)]
        machine: Option<String>,

        /// Add to a specific platform tier
        #[arg(long)]
        platform: Option<String>,

        /// Commit after adding (with message)
        #[arg(long)]
        commit: Option<String>,

        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a file or directory from the repository
    #[command(
        after_help = "Examples:\n  # Remove a tracked file (restores original if backed up)\n  dotty remove ~/.old-config\n\n  # Remove from a specific machine tier\n  dotty remove ~/.vimrc --machine macbook\n\n  # Remove and commit in one step\n  dotty remove ~/.old-config --commit \"remove old config\""
    )]
    Remove {
        /// Path to remove
        path: String,

        /// Limit search to a specific machine tier
        #[arg(long)]
        machine: Option<String>,

        /// Commit after removing (with message)
        #[arg(long)]
        commit: Option<String>,

        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Create symlinks for all tracked files
    #[command(
        after_help = "Examples:\n  # Apply all tracked files (create/update symlinks)\n  dotty apply\n\n  # Preview changes without applying\n  dotty apply --dry-run\n\n  # Override platform detection\n  dotty apply --platform linux"
    )]
    Apply {
        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,

        /// Override auto-detected platform (e.g. linux, macos, windows)
        #[arg(long)]
        platform: Option<String>,
    },

    /// Show repository status
    Status,

    /// Remove old backups from state directory
    Clean {
        /// Number of recent backups to keep
        #[arg(long)]
        keep: Option<usize>,

        /// Remove backups older than this date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,

        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Set the current machine name
    Machine {
        /// Machine name
        name: String,
    },
}
