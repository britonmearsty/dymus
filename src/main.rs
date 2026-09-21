mod app;
mod auth;
mod innertube;
mod lyrics;
mod model;
mod player;
mod ui;
mod visualizer;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Search songs through InnerTube and print JSON, without starting the player.
    Search { query: String },
    /// Check playback and optional visualizer dependencies.
    Doctor,
    /// Configure, check, or remove YouTube Music browser-session credentials.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Paste a YouTube Music Cookie header privately and validate it before saving.
    Paste {
        /// X-Goog-AuthUser account index from the browser request (usually 0).
        #[arg(long, default_value = "0")]
        auth_user: String,
    },
    /// Validate the configured session and show its account name.
    Status,
    /// Remove Dymus's locally stored credentials.
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Some(Command::Search { query }) => {
            let tracks = innertube::InnerTube::configured()?.search(&query).await?;
            println!("{}", serde_json::to_string_pretty(&tracks)?);
        }
        Some(Command::Doctor) => doctor().await?,
        Some(Command::Auth {
            action: AuthCommand::Paste { auth_user },
        }) => {
            auth::validate_input()?;
            let cookie = auth::prompt_cookie()?;
            let credentials = auth::BrowserAuth::from_cookie(cookie, &auth_user)?;
            let account = innertube::InnerTube::new()?
                .with_auth(credentials.clone())
                .validate_session()
                .await?;
            auth::save(&credentials)?;
            println!(
                "Signed in as {account}. Credentials saved to Dymus's private configuration directory."
            );
        }
        Some(Command::Auth {
            action: AuthCommand::Status,
        }) => {
            let client = innertube::InnerTube::configured()?;
            let account = client.validate_session().await?;
            println!("Signed in as {account}.");
        }
        Some(Command::Auth {
            action: AuthCommand::Logout,
        }) => {
            if auth::logout()? {
                println!("Dymus credentials removed.");
            } else {
                println!("No Dymus credentials were saved.");
            }
        }
        None => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!(
                    "The TUI needs an interactive terminal. Use `dymus search <query>` or `dymus doctor` instead."
                );
            }
            doctor().await?;
            let mut app = app::App::new().await?;
            let mut terminal = ratatui::init();
            app.image_picker = ratatui_image::picker::Picker::from_query_stdio()
                .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());
            let result = app.run(&mut terminal).await;
            ratatui::restore();
            app.shutdown().await;
            result?;
        }
    }
    Ok(())
}

async fn doctor() -> Result<()> {
    for program in ["mpv", "yt-dlp"] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(program)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("Dependency check timed out")?
        .with_context(|| format!("Install {program} and ensure it is on PATH"))?;
        if !output.status.success() {
            bail!("{program} --version failed");
        }
        println!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
        );
    }
    let mut pipewire_tools = true;
    for program in ["pw-dump", "pw-record"] {
        match tokio::process::Command::new(program)
            .arg("--version")
            .output()
            .await
        {
            Ok(output) if output.status.success() => println!("{program}: available"),
            _ => {
                pipewire_tools = false;
                println!("{program}: missing (audio-reactive visualizers unavailable)");
            }
        }
    }
    if pipewire_tools {
        match tokio::process::Command::new("pw-dump")
            .arg("--no-colors")
            .output()
            .await
        {
            Ok(output) if output.status.success() => println!("PipeWire session: connected"),
            _ => {
                println!("PipeWire session: unavailable (visualizers will use animation fallback)")
            }
        }
    }
    Ok(())
}
