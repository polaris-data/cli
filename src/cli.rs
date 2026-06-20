use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "polaris",
    version,
    about = "Download Polaris market data snapshots",
    after_help = "Running `polaris` with no command opens the interactive dataset browser TUI."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the current Polaris auth state and account details.
    Account,
    /// List remote datasets available from Polaris.
    Catalog(RemoteListArgs),
    /// Send product feedback to the Polaris team.
    Feedback(FeedbackArgs),
    /// Store a Polaris API key from a secure prompt.
    Key,
    /// Sign in through the browser and store the returned API key.
    Login,
    /// List local snapshots under the configured root.
    List(LocalListArgs),
    /// Download missing snapshots for a dataset and time range.
    Download(DownloadArgs),
    /// Remove all local dataset state managed by Polaris.
    Reset(ResetArgs),
    /// Download and install the latest Polaris CLI release.
    Update(UpdateArgs),
}

#[derive(Debug, Clone, Args)]
pub struct DatasetArgs {
    #[arg(long)]
    pub exchange: String,
    #[arg(long)]
    pub asset: String,
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RemoteListArgs {
    #[arg(long)]
    pub exchange: Option<String>,
    #[arg(long)]
    pub asset: Option<String>,
    #[arg(long)]
    pub search: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct FeedbackArgs {
    pub message: String,
}

#[derive(Debug, Clone, Args)]
pub struct LocalListArgs {
    #[arg(long)]
    pub exchange: Option<String>,
    #[arg(long)]
    pub asset: Option<String>,
    #[arg(long)]
    pub date: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub dataset: DatasetArgs,
    #[arg(long)]
    pub concurrency: Option<usize>,
}

#[derive(Debug, Clone, Args)]
pub struct ResetArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct UpdateArgs {
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub install_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command};

    #[test]
    fn account_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "account"]).expect("cli");
        assert!(matches!(cli.command, Some(Command::Account)));
    }

    #[test]
    fn login_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "login"]).expect("cli");
        assert!(matches!(cli.command, Some(Command::Login)));
    }

    #[test]
    fn key_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "key"]).expect("cli");
        assert!(matches!(cli.command, Some(Command::Key)));
    }

    #[test]
    fn feedback_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "feedback", "can you add this?"]).expect("cli");
        match cli.command {
            Some(Command::Feedback(args)) => assert_eq!(args.message, "can you add this?"),
            other => panic!("expected feedback command, got {other:?}"),
        }
    }

    #[test]
    fn catalog_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "catalog"]).expect("cli");
        assert!(matches!(cli.command, Some(Command::Catalog(_))));
    }

    #[test]
    fn list_command_parses() {
        let cli = Cli::try_parse_from(["polaris", "list"]).expect("cli");
        assert!(matches!(cli.command, Some(Command::List(_))));
    }

    #[test]
    fn download_command_parses() {
        let cli = Cli::try_parse_from([
            "polaris",
            "download",
            "--exchange",
            "aster",
            "--asset",
            "BTCUSDT",
            "--from",
            "2026-06-01T00:00:00Z",
            "--to",
            "2026-06-02T00:00:00Z",
        ])
        .expect("cli");
        assert!(matches!(cli.command, Some(Command::Download(_))));
    }

    #[test]
    fn update_command_parses_with_optional_overrides() {
        let cli = Cli::try_parse_from([
            "polaris",
            "update",
            "--version",
            "v0.4.2",
            "--install-dir",
            "/tmp/polaris",
        ])
        .expect("cli");

        match cli.command {
            Some(Command::Update(args)) => {
                assert_eq!(args.version.as_deref(), Some("v0.4.2"));
                assert_eq!(
                    args.install_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/polaris"))
                );
            }
            other => panic!("expected update command, got {other:?}"),
        }
    }

    #[test]
    fn top_level_help_lists_command_descriptions() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();

        assert!(
            help.contains("Running `polaris` with no command opens the interactive dataset browser TUI.")
        );
        assert!(help.contains("account   Print the current Polaris auth state and account details"));
        assert!(help.contains("catalog   List remote datasets available from Polaris"));
        assert!(help.contains("feedback  Send product feedback to the Polaris team"));
        assert!(help.contains("key       Store a Polaris API key from a secure prompt"));
        assert!(help.contains("login     Sign in through the browser and store the returned API key"));
        assert!(help.contains("list      List local snapshots under the configured root"));
        assert!(help.contains("download  Download missing snapshots for a dataset and time range"));
        assert!(help.contains("reset     Remove all local dataset state managed by Polaris"));
        assert!(help.contains("update    Download and install the latest Polaris CLI release"));
    }
}
