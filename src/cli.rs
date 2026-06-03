use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "polaris",
    version,
    about = "Sync Polaris market data snapshots"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Account(AccountCommand),
    List(ListCommand),
    Reset(ResetArgs),
    Sync(SyncArgs),
    Update(UpdateArgs),
}

#[derive(Debug, Clone, Args)]
pub struct AccountCommand {
    #[command(subcommand)]
    pub subcommand: AccountSubcommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AccountSubcommand {
    SetKey,
    Status,
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
pub struct ListCommand {
    #[command(subcommand)]
    pub subcommand: Option<ListSubcommand>,
    #[command(flatten)]
    pub remote: RemoteListArgs,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ListSubcommand {
    Local(LocalListArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SyncArgs {
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
    fn update_command_parses_with_optional_overrides() {
        let cli = Cli::try_parse_from([
            "polaris",
            "update",
            "--version",
            "v0.2.0",
            "--install-dir",
            "/tmp/polaris",
        ])
        .expect("cli");

        match cli.command {
            Some(Command::Update(args)) => {
                assert_eq!(args.version.as_deref(), Some("v0.2.0"));
                assert_eq!(
                    args.install_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/polaris"))
                );
            }
            other => panic!("expected update command, got {other:?}"),
        }
    }
}
