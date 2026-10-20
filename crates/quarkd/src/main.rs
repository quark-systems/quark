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
    /// Read IBM Bob's or Kiro CLI's quota the way dispatch does, or anchor
    /// Bob's estimate with the "Monthly Usage" figure its /status shows.
    Quota {
        /// Quark home directory [default: $QUARK_HOME or ~/.quark]
        #[arg(long)]
        home: Option<PathBuf>,
        #[command(subcommand)]
        provider: QuotaCommand,
    },
    /// Run the PTY supervisor the native engine's workers live in (what
    /// `quark-ptyd` runs). quarkd starts it itself; it outlives quarkd.
    #[command(hide = true)]
    Ptyd {
        #[arg(long)]
        socket: PathBuf,
    },
}

#[derive(Subcommand)]
enum QuotaCommand {
    /// IBM Bob's monthly Bobcoins.
    Bob {
        /// Print the reading as JSON (quota-axi's schema 5).
        #[arg(long)]
        json: bool,
        /// Record this many used coins, from Bob's /status, as the anchor.
        #[arg(long, value_name = "COINS")]
        anchor: Option<f64>,
        /// With --anchor: the "Monthly Budget" /status shows.
        #[arg(long, requires = "anchor")]
        budget: Option<f64>,
    },
    /// Kiro CLI's plan credits.
    Kiro {
        /// Print the reading as JSON (quota-axi's schema 5).
        #[arg(long)]
        json: bool,
    },
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
        Command::Ptyd { socket } => {
            quark_sessions::pty::run_daemon(&socket).await?;
            Ok(())
        }
        Command::Quota { home, provider } => {
            let home = home.unwrap_or_else(config::default_home);
            quota(&home, provider).await
        }
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

async fn quota(home: &std::path::Path, provider: QuotaCommand) -> anyhow::Result<()> {
    use quark_dispatch::local::{bob, kiro, LocalReading};
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    match provider {
        QuotaCommand::Bob {
            json,
            anchor,
            budget,
        } => {
            let reader = bob::BobUsage::for_home(home, user_home.as_deref());
            if let Some(used) = anchor {
                let line = reader
                    .record_anchor(used, budget, "manual", time::OffsetDateTime::now_utc())
                    .map_err(anyhow::Error::msg)?;
                println!("{line}");
                return Ok(());
            }
            let v = reader.read().await;
            if json {
                println!("{v}");
            } else {
                println!("{}", bob::status_line(&v));
            }
        }
        QuotaCommand::Kiro { json } => {
            let v = kiro::KiroUsage::for_home(home).read().await;
            if json {
                println!("{v}");
            } else {
                println!("{}", kiro::status_line(&v));
            }
        }
    }
    Ok(())
}
