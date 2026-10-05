use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use quarkd::config::{self, Config, EngineKind};

#[derive(Parser)]
#[command(name = "quarkd", version, about = "Quark local control plane daemon")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (the default).
    Serve(ServeArgs),
    /// Print the OpenAPI document for the /v1 API.
    Openapi,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Quark home directory [default: $QUARK_HOME or ~/.quark]
    #[arg(long)]
    home: Option<PathBuf>,
    /// Loopback address to listen on.
    #[arg(long, env = "QUARKD_LISTEN",
          default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config::DEFAULT_PORT))]
    listen: SocketAddr,
    /// Engine adapter to read workspaces with.
    #[arg(long, value_enum, default_value_t = EngineKind::Stub)]
    engine: EngineKind,
    /// Seconds between projection refreshes.
    #[arg(long, default_value_t = 5)]
    refresh_secs: u64,
    /// Seconds between reads of open pull requests from their forge.
    #[arg(long, default_value_t = 30)]
    pr_refresh_secs: u64,
    /// tmux binary for terminal sessions [default: tmux on PATH]
    #[arg(long, env = "QUARKD_TMUX")]
    tmux: Option<PathBuf>,
    /// Seconds between reads of each account's quota.
    #[arg(long, default_value_t = 300)]
    quota_refresh_secs: u64,
    /// quota-axi binary that reads account quota.
    #[arg(long, env = "QUARKD_QUOTA_AXI", default_value = "quota-axi")]
    quota_axi: PathBuf,
    /// User-level memory directory, shared by every Project [default: <home>/memory]
    #[arg(long, env = "QUARK_USER_MEMORY")]
    user_memory: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("QUARKD_LOG")
                .unwrap_or_else(|_| "quarkd=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve(cli.serve)) {
        Command::Openapi => {
            print!("{}", quarkd::api::ApiDoc::json());
            Ok(())
        }
        Command::Serve(args) => {
            let config = Config {
                home: args.home.unwrap_or_else(config::default_home),
                listen: args.listen,
                refresh_interval: Duration::from_secs(args.refresh_secs.max(1)),
                pr_refresh_interval: Duration::from_secs(args.pr_refresh_secs.max(5)),
                tmux: args.tmux,
                quota_refresh_interval: Duration::from_secs(args.quota_refresh_secs.max(30)),
                quota_axi: args.quota_axi,
                user_memory: args.user_memory,
            };
            quarkd::serve(config, args.engine).await
        }
    }
}
