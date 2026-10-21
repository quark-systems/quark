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
    /// Seed a new Project from another tool's local data, through the
    /// running daemon.
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
    /// A firstmate install: its projects, backlog, briefs, dispatch rules,
    /// preferences and learnings. The firstmate home is only read.
    Firstmate {
        /// The firstmate home (FM_HOME, usually the firstmate clone).
        path: PathBuf,
        /// The new Project's name [default: from the firstmate projects]
        #[arg(long)]
        name: Option<String>,
        /// The coordinator's harness id [default: the firstmate crew harness]
        #[arg(long)]
        harness: Option<String>,
        /// Show what would be imported; create nothing.
        #[arg(long)]
        dry_run: bool,
        /// The daemon's address.
        #[arg(long, env = "QUARKD_LISTEN",
              default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config::DEFAULT_PORT))]
        daemon: SocketAddr,
        /// Print the daemon's answer as JSON.
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
    use quark_systems::{FirstmateImport, ImportFirstmate, ImportOutcome};
    let ImportCommand::Firstmate {
        path,
        name,
        harness,
        dry_run,
        daemon,
        json,
    } = source;
    let path = std::path::absolute(&path)?;
    let input = ImportFirstmate {
        path: path.to_string_lossy().into_owned(),
        name,
        harness,
        dry_run,
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
    let answer: FirstmateImport = serde_json::from_slice(&body)?;
    let plan = &answer.plan;
    let p = &plan.project;
    println!("Project: {}", p.name);
    for repo in &p.repos {
        println!(
            "  repo {}: {}",
            repo.name.as_deref().unwrap_or("?"),
            repo.url
        );
    }
    if let Some(agent) = &p.agent_config {
        println!(
            "  coordinator: {}{}",
            agent.harness,
            agent
                .model
                .as_deref()
                .map(|m| format!(" {m}"))
                .unwrap_or_default()
        );
    }
    println!();
    for item in &plan.items {
        let mark = match item.outcome {
            ImportOutcome::Imported => "imported",
            ImportOutcome::Skipped => "skipped ",
        };
        println!("{mark}  {} ({}): {}", item.what, item.count, item.note);
    }
    if !plan.warnings.is_empty() {
        println!();
        for w in &plan.warnings {
            println!("note: {w}");
        }
    }
    println!();
    match answer.project {
        Some(project) => println!(
            "Created {} ({}). It is provisioning; the import lands once it is ready, \
             and library/firstmate/IMPORT.md in its Project repo records what was imported.",
            project.name, project.id
        ),
        None => println!("Dry run: nothing was created."),
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
