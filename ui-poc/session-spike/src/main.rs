//! Spike: should quarkd use herdr instead of tmux control mode as its
//! session layer? See REPORT.md for the findings and README.md to reproduce.
//!
//! ```text
//! quark-session-spike herdr  [--herdr BIN] [--run DIR] [--samples N]
//! quark-session-spike server-restart [--herdr BIN] [--run DIR]
//! quark-session-spike stub   [--port 7450] [--samples N]
//! quark-session-spike cleanup [--herdr BIN] [--run DIR]   (close the workspace, stop the server)
//! ```

mod herdr;
mod herdr_arm;
mod measure;
mod stub_arm;

use std::path::PathBuf;

use herdr::Herdr;

struct Args {
    cmd: String,
    herdr: PathBuf,
    run: PathBuf,
    port: u16,
    samples: usize,
}

fn parse() -> Args {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut a = Args {
        cmd: String::new(),
        herdr: here.join(".run/herdr"),
        run: here.join(".run"),
        port: 7450,
        samples: 200,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| die(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--herdr" => a.herdr = PathBuf::from(val()),
            "--run" => a.run = PathBuf::from(val()),
            "--port" => a.port = val().parse().unwrap_or_else(|_| die("bad --port")),
            "--samples" => a.samples = val().parse().unwrap_or_else(|_| die("bad --samples")),
            "-h" | "--help" => {
                println!("usage: quark-session-spike herdr|server-restart|stub|cleanup [options]");
                std::process::exit(0);
            }
            c if a.cmd.is_empty() && !c.starts_with('-') => a.cmd = c.to_string(),
            other => die(&format!("unknown argument {other}")),
        }
    }
    a
}

fn die(msg: &str) -> ! {
    eprintln!("quark-session-spike: {msg}");
    std::process::exit(2)
}

fn herdr_ctx(a: &Args) -> herdr::Result<(herdr_arm::Ctx, bool)> {
    // Keep the socket path short: AF_UNIX paths are limited to ~108 bytes.
    let h = Herdr {
        bin: a.herdr.clone(),
        config_home: a.run.join("config"),
    };
    let cfg = h.config_home.join("herdr");
    std::fs::create_dir_all(&cfg).map_err(|e| e.to_string())?;
    let cfg_file = cfg.join("config.toml");
    if !cfg_file.exists() {
        // No background calls to herdr.dev (version check, remote agent-detection manifests).
        std::fs::write(
            &cfg_file,
            "[update]\nversion_check = false\nmanifest_check = false\n",
        )
        .map_err(|e| e.to_string())?;
    }
    h.ensure_server()?;
    let scripts = herdr_arm::install_scripts(&a.run)?;
    let (panes, reused) = herdr_arm::find_or_create(&h, &scripts)?;
    Ok((herdr_arm::Ctx { h, scripts, panes }, reused))
}

fn main() {
    let a = parse();
    let r = match a.cmd.as_str() {
        "herdr" => herdr_ctx(&a).and_then(|(ctx, reused)| herdr_arm::run(&ctx, reused, a.samples)),
        "server-restart" => herdr_ctx(&a).and_then(|(ctx, _)| herdr_arm::server_restart(&ctx)),
        "stub" => stub_arm::run(a.port, a.samples),
        "cleanup" => {
            // Close the spike workspace and stop the isolated server.
            let h = Herdr {
                bin: a.herdr.clone(),
                config_home: a.run.join("config"),
            };
            if h.server_running() {
                let _ = herdr_arm::close_workspace(&h);
                h.stop_server()
            } else {
                Ok(())
            }
        }
        _ => die("command must be herdr, server-restart, stub or cleanup"),
    };
    if let Err(e) = r {
        eprintln!("quark-session-spike: {e}");
        std::process::exit(1);
    }
}
