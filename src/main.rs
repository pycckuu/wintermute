//! Wintermute CLI entry point.
//!
//! Provides subcommands for initializing, starting, and managing
//! the Wintermute agent.

use clap::{Parser, Subcommand};
use tracing::info;

/// Wintermute — a self-coding AI agent.
#[derive(Parser)]
#[command(name = "wintermute", version, about)]
struct Cli {
    /// Subcommand to execute
    #[command(subcommand)]
    command: Command,
}

/// Available CLI subcommands.
#[derive(Subcommand)]
enum Command {
    /// First-time setup: create config files, initialize database, build sandbox
    Init,
    /// Start the agent (connects to Telegram, begins listening)
    Start,
    /// Show health status, sandbox info, and memory stats
    Status,
    /// Recreate the sandbox container and reinstall dependencies
    Reset,
    /// Trigger an immediate backup of scripts and memory
    Backup {
        /// Subcommand for backup operations
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
}

/// Backup subcommands.
#[derive(Subcommand)]
enum BackupAction {
    /// List available backups
    List,
    /// Restore a specific backup by index
    Restore {
        /// Backup index to restore
        index: u32,
    },
}

fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init => {
            info!("wintermute init: not yet implemented");
        }
        Command::Start => {
            info!("wintermute start: not yet implemented");
        }
        Command::Status => {
            info!("wintermute status: not yet implemented");
        }
        Command::Reset => {
            info!("wintermute reset: not yet implemented");
        }
        Command::Backup { action } => match action {
            None => {
                info!("wintermute backup: not yet implemented");
            }
            Some(BackupAction::List) => {
                info!("wintermute backup list: not yet implemented");
            }
            Some(BackupAction::Restore { index }) => {
                info!(index, "wintermute backup restore: not yet implemented");
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
