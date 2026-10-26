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
    /// Report what went wrong lately: logged warnings and errors, failed
    /// engine reads, supervisor and dispatch trouble, quiet workers. Reads
    /// the home directly, so it works with the daemon down.
    Diagnose {
        /// Quark home directory [default: $QUARK_HOME or ~/.quark]
        #[arg(long)]
        home: Option<PathBuf>,
        /// Hours back to look.
        #[arg(long, default_value_t = 24)]
        hours: u32,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Bring projects over from another tool, through the running daemon.
    Import {
        #[command(subcommand)]
        source: ImportCommand,
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

#[derive(Subcommand)]
enum ImportCommand {
    /// A firstmate home: a Project per firstmate project, with its dispatch
    /// rules, backlog, holds, quota settings and memory. The home is only
    /// read. Importing again finishes what an earlier import left pending.
    Firstmate {
        /// The firstmate home (FM_HOME, usually the firstmate clone).
        path: PathBuf,
        /// Only this firstmate project; repeat for more [default: all]
        #[arg(long = "project", value_name = "NAME")]
        projects: Vec<String>,
        /// Route backlog items whose id starts with PREFIX to PROJECT when
        /// nothing in them says; repeat for more.
        #[arg(long = "route", value_name = "PREFIX=PROJECT", value_parser = parse_route)]
        routes: Vec<(String, String)>,
        /// Also import the Done entries of data/done-archive.md.
        #[arg(long)]
        include_archive: bool,
        /// Show what would be imported; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Seconds to wait for each new Project to be set up [default: 120]
        #[arg(long)]
        wait_secs: Option<u64>,
        /// The daemon's address.
        #[arg(long, env = "QUARKD_LISTEN",
              default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config::DEFAULT_PORT))]
        daemon: SocketAddr,
        /// Print the daemon's report as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn parse_route(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((prefix, project)) if !prefix.is_empty() && !project.is_empty() => {
            Ok((prefix.to_string(), project.to_string()))
        }
        _ => Err("expected PREFIX=PROJECT".into()),
    }
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
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve(cli.serve));
    // The daemon also logs to <home>/logs, so a daemon the app launched
    // (whose stderr goes nowhere) still leaves a log to diagnose from.
    let log_dir = match &command {
        Command::Serve(args) => Some(
            args.home
                .clone()
                .unwrap_or_else(config::default_home)
                .join("logs"),
        ),
        _ => None,
    };
    let _log_guard = init_tracing(log_dir.as_deref());

    match command {
        Command::Ptyd { socket } => {
            quark_sessions::pty::run_daemon(&socket).await?;
            Ok(())
        }
        Command::Quota { home, provider } => {
            let home = home.unwrap_or_else(config::default_home);
            quota(&home, provider).await
        }
        Command::Diagnose { home, hours, json } => {
            let home = home.unwrap_or_else(config::default_home);
            let now = time::OffsetDateTime::now_utc();
            let report =
                quarkd::diagnose::diagnose(&home, now - time::Duration::hours(hours.into()), now);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", quarkd::diagnose::render(&report));
            }
            Ok(())
        }
        Command::Import { source } => import(source).await,
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

/// Logs to stderr, and with `log_dir` also as JSON lines to a file there
/// that rolls daily, keeping a week. The returned guard flushes the file on
/// drop.
fn init_tracing(
    log_dir: Option<&std::path::Path>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("QUARKD_LOG")
            .unwrap_or_else(|_| "quarkd=info,tower_http=info".into())
    };
    let stderr = tracing_subscriber::fmt::layer().with_filter(filter());
    let file = log_dir.and_then(|dir| {
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("quarkd")
            .filename_suffix("jsonl")
            .max_log_files(7)
            .build(dir)
            .map_err(|e| eprintln!("quarkd: not logging to {}: {e}", dir.display()))
            .ok()?;
        Some(tracing_appender::non_blocking(appender))
    });
    let (file_layer, guard) = match file {
        Some((writer, guard)) => (
            Some(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(writer)
                    .with_filter(filter()),
            ),
            Some(guard),
        ),
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();
    guard
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

async fn import(source: ImportCommand) -> anyhow::Result<()> {
    use quark_systems::{FirstmateImportReport, ImportFirstmate, ImportOutcome};
    let ImportCommand::Firstmate {
        path,
        projects,
        routes,
        include_archive,
        dry_run,
        wait_secs,
        daemon,
        json,
    } = source;
    let input = ImportFirstmate {
        home: std::path::absolute(&path)?.to_string_lossy().into_owned(),
        dry_run,
        projects,
        include_archive,
        routes: routes.into_iter().collect(),
        wait_secs,
    };
    let (status, body) = post(
        daemon,
        "/v1/imports/firstmate",
        &serde_json::to_vec(&input)?,
    )
    .await
    .map_err(|e| anyhow::anyhow!("reaching quarkd at {daemon}: {e}; is it running?"))?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
        anyhow::bail!("{message}");
    }
    if json {
        println!("{}", String::from_utf8_lossy(&body));
        return Ok(());
    }
    let report: FirstmateImportReport = serde_json::from_slice(&body)?;
    let outcome = |o: ImportOutcome| match o {
        ImportOutcome::Imported if report.dry_run => "would import",
        ImportOutcome::Imported => "imported",
        ImportOutcome::Present => "already there",
        ImportOutcome::Pending => "pending",
        ImportOutcome::NotImported => "not imported",
    };
    for p in &report.projects {
        println!(
            "{} ({}, {}): {} issues, {} decisions{}",
            p.name,
            p.project,
            outcome(p.outcome),
            p.tasks.imported,
            p.holds.imported,
            p.detail
                .as_deref()
                .map(|d| format!(": {d}"))
                .unwrap_or_default()
        );
    }
    println!();
    for i in report.inventory.iter().filter(|i| i.count > 0) {
        println!(
            "{:<13} {} ({}): {}",
            outcome(i.outcome),
            i.source,
            i.count,
            i.detail
        );
    }
    if !report.unrouted.is_empty() {
        println!("\nNot routed to a project (use --route PREFIX=PROJECT):");
        for u in &report.unrouted {
            println!("  {u}");
        }
    }
    if !report.notes.is_empty() {
        println!();
        for n in &report.notes {
            println!("note: {n}");
        }
    }
    if report.dry_run {
        println!("\nDry run: nothing was written.");
    } else if report
        .projects
        .iter()
        .any(|p| p.outcome == ImportOutcome::Pending)
    {
        println!("\nSome Projects are still being set up; run the import again to finish them.");
    }
    Ok(())
}

/// One JSON POST to the daemon over loopback HTTP/1.1. Returns the status
/// and body.
async fn post(addr: SocketAddr, path: &str, body: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(bad)?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    let mut rest = raw[split + 4..].to_vec();
    if head.lines().any(|l| {
        l.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    }) {
        rest = dechunk(&rest).ok_or_else(bad)?;
    }
    Ok((status, rest))
}

fn dechunk(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = raw.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&raw[..line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        raw = &raw[line + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(raw.get(..size)?);
        raw = raw.get(size + 2..)?;
    }
}
