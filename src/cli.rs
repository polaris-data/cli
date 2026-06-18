use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "polaris",
    version,
    about = "Download Polaris market data snapshots"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Account,
    Catalog(RemoteListArgs),
    Key,
    Login,
    List(LocalListArgs),
    Download(DownloadArgs),
    Reset(ResetArgs),
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
    use clap::Parser;

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
            "v0.4.0",
            "--install-dir",
            "/tmp/polaris",
        ])
        .expect("cli");

        match cli.command {
            Some(Command::Update(args)) => {
                assert_eq!(args.version.as_deref(), Some("v0.4.0"));
                assert_eq!(
                    args.install_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/polaris"))
                );
            }
            other => panic!("expected update command, got {other:?}"),
        }
    }
}
