use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;

/// Verbosity level for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// Suppress non-essential output (ERROR only).
    Quiet,
    /// Default informational output.
    #[default]
    Normal,
    /// Verbose debug output.
    Verbose,
}

impl Verbosity {
    /// Resolve verbosity from CLI flags.
    ///
    /// `--quiet` takes precedence over `--verbose` if both are set.
    pub fn from_flags(verbose: bool, quiet: bool) -> Self {
        if quiet {
            Self::Quiet
        } else if verbose {
            Self::Verbose
        } else {
            Self::Normal
        }
    }

    /// Return the corresponding tracing log level string.
    ///
    /// `Quiet` uses `off` because application errors are already reported
    /// through `anyhow` in `main()`, so tracing logs are purely diagnostic.
    fn log_level(&self) -> &'static str {
        match self {
            Self::Quiet => "off",
            Self::Normal => "info",
            Self::Verbose => "debug",
        }
    }
}

/// Initialize the tracing subscriber.
///
/// Uses the provided verbosity level to set the log filter.
/// `RUST_LOG` environment variable overrides the default if set.
/// All logs go to stderr so they don't mix with user-facing stdout output.
pub fn init(verbosity: Verbosity) {
    let default_level = verbosity.log_level();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .without_time(),
        )
        .with(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new(default_level))
                .unwrap(),
        )
        .init();
}
