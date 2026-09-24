mod app;
mod auth;
mod cache;
mod config;
mod headless;
mod innertube;
mod lastfm;
mod lyrics;
mod model;
mod player;
mod radio;
mod ui;
mod visualizer;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use crossterm::style::{Color, Stylize};
use std::{io::IsTerminal, time::Duration};

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
    /// Play a song, album, or playlist without starting the TUI.
    Play {
        #[command(subcommand)]
        target: PlayTarget,
        /// Leave playback running after this command exits.
        #[arg(long, global = true)]
        detach: bool,
        /// Initial playback volume, from 0 to 100.
        #[arg(long, global = true, default_value_t = 80)]
        volume: u8,
    },
    /// Control a detached headless player.
    Control {
        #[command(subcommand)]
        action: ControlCommand,
    },
    #[command(name = "__headless-resolve", hide = true)]
    HeadlessResolve {
        #[arg(long, default_value_t = 1)]
        start: usize,
    },
    /// Configure, check, or remove YouTube Music browser-session credentials.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
    Lastfm {
        #[command(subcommand)]
        action: LastFmCommand,
    },
}

#[derive(Subcommand)]
enum PlayTarget {
    Song {
        query: String,
    },
    Album {
        query: String,
    },
    Playlist {
        query: String,
    },
    /// Choose an item from a signed-in YouTube Music library category.
    Library {
        #[command(subcommand)]
        kind: LibraryTarget,
    },
}

#[derive(Subcommand)]
enum LibraryTarget {
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Subcommand)]
enum ControlCommand {
    Pause,
    Resume,
    Toggle,
    Next,
    Previous,
    Stop,
    Volume { level: u8 },
    Status,
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
#[derive(Subcommand)]
enum LastFmCommand {
    /// Authorize Dymus in a browser and save a new Last.fm session.
    Login,
    /// Privately save an API key, shared secret, and session key from Last.fm.
    Paste,
    /// Verify the saved session with Last.fm and show its account name.
    Status,
    /// Remove only Last.fm credentials, preserving YouTube Music sign-in.
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Some(Command::Search { query }) => {
            let page = innertube::InnerTube::configured()?
                .search(&query, innertube::SearchFilter::Songs)
                .await?;
            println!("{}", serde_json::to_string_pretty(&page.tracks)?);
        }
        Some(Command::Doctor) => doctor().await?,
        Some(Command::Play {
            target,
            detach,
            volume,
        }) => {
            let target = match target {
                PlayTarget::Song { query } => (headless::Target::Song, query),
                PlayTarget::Album { query } => (headless::Target::Album, query),
                PlayTarget::Playlist { query } => (headless::Target::Playlist, query),
                PlayTarget::Library { kind } => {
                    let kind = match kind {
                        LibraryTarget::Playlists => innertube::LibraryKind::Playlists,
                        LibraryTarget::Albums => innertube::LibraryKind::Albums,
                        LibraryTarget::Artists => innertube::LibraryKind::Artists,
                        LibraryTarget::Podcasts => innertube::LibraryKind::Podcasts,
                    };
                    (headless::Target::Library(kind), String::new())
                }
            };
            headless::play(target.0, &target.1, detach, volume).await?;
        }
        Some(Command::Control { action }) => {
            let action = match action {
                ControlCommand::Pause => headless::Control::Pause,
                ControlCommand::Resume => headless::Control::Resume,
                ControlCommand::Toggle => headless::Control::Toggle,
                ControlCommand::Next => headless::Control::Next,
                ControlCommand::Previous => headless::Control::Previous,
                ControlCommand::Stop => headless::Control::Stop,
                ControlCommand::Volume { level } => headless::Control::Volume(level),
                ControlCommand::Status => headless::Control::Status,
            };
            headless::control(action).await?;
        }
        Some(Command::HeadlessResolve { start }) => headless::resolve_remaining(start).await?,
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
        Some(Command::Lastfm {
            action: LastFmCommand::Login,
        }) => {
            let credentials = lastfm::authorize_interactively().await?;
            let account = lastfm::Client::new(credentials.clone())?.verify().await?;
            auth::save_lastfm(credentials)?;
            println!("Last.fm connected as {account}. Credentials saved privately.");
        }
        Some(Command::Lastfm {
            action: LastFmCommand::Paste,
        }) => {
            let credentials = auth::prompt_lastfm()?;
            let account = lastfm::Client::new(credentials.clone())?.verify().await?;
            auth::save_lastfm(credentials)?;
            println!("Last.fm connected as {account}. Credentials saved privately.");
        }
        Some(Command::Lastfm {
            action: LastFmCommand::Status,
        }) => match auth::load_lastfm()? {
            Some(credentials) => println!(
                "Last.fm connected as {}.",
                lastfm::Client::new(credentials)?.verify().await?
            ),
            None => println!("No Last.fm credentials are configured."),
        },
        Some(Command::Lastfm {
            action: LastFmCommand::Logout,
        }) => {
            if auth::logout_lastfm()? {
                println!("Last.fm credentials removed.");
            } else {
                println!("No Last.fm credentials were saved.");
            }
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
            let mut app = app::App::new().await?;
            app.start();
            let mut terminal = ratatui::init();
            app.image_picker = ratatui_image::picker::Picker::from_query_stdio_with_options(
                ratatui_image::picker::cap_parser::QueryStdioOptions {
                    timeout: Duration::from_millis(150),
                    ..Default::default()
                },
            )
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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(program)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let output = match result {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let message = format!(
                    "{program} is not available: {error}; {}",
                    dependency_help(program)
                );
                doctor_error(&message);
                bail!(message);
            }
            Err(_) => {
                let message =
                    format!("{program} did not respond within 10 seconds; reinstall or check PATH");
                doctor_error(&message);
                bail!(message);
            }
        };
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let message = format!(
                "{program} --version failed{}; {}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                },
                dependency_help(program),
            );
            doctor_error(&message);
            bail!(message);
        }
        doctor_ok(&format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
        ));
    }
    let mut pipewire_tools = true;
    for program in ["pw-dump", "pw-record"] {
        match tokio::process::Command::new(program)
            .arg("--version")
            .output()
            .await
        {
            Ok(output) if output.status.success() => doctor_ok(&format!("{program}: available")),
            _ => {
                pipewire_tools = false;
                doctor_warning(&format!(
                    "{program}: missing (audio-reactive visualizers unavailable)"
                ));
            }
        }
    }
    if pipewire_tools {
        match tokio::process::Command::new("pw-dump")
            .arg("--no-colors")
            .output()
            .await
        {
            Ok(output) if output.status.success() => doctor_ok("PipeWire session: connected"),
            _ => doctor_warning(
                "PipeWire session: unavailable (visualizers will use animation fallback)",
            ),
        }
    }
    Ok(())
}

fn doctor_ok(message: &str) {
    doctor_line("✓", Color::Green, message, Color::White);
}
fn doctor_warning(message: &str) {
    doctor_line("!", Color::Yellow, message, Color::Yellow);
}
fn doctor_error(message: &str) {
    let (summary, detail) = message.split_once(';').unwrap_or((message, ""));
    eprintln!(
        "{} {}{}",
        "✗".with(Color::Red).bold(),
        summary.with(Color::Red).bold(),
        if detail.is_empty() {
            String::new()
        } else {
            format!("; {}", detail.with(Color::Yellow))
        },
    );
}

fn doctor_line(icon: &str, icon_color: Color, message: &str, detail_color: Color) {
    let (label, detail) = message.split_once(": ").unwrap_or((message, ""));
    println!(
        "{} {}{}",
        icon.with(icon_color).bold(),
        label.with(Color::Cyan).bold(),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {}", detail.with(detail_color))
        },
    );
}

fn dependency_help(program: &str) -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let command = if os_release.contains("ID=arch") || os_release.contains("ID_LIKE=arch") {
        format!("install it with `sudo pacman -S {program}`")
    } else if os_release.contains("ID=fedora") || os_release.contains("ID_LIKE=\"fedora") {
        format!("install it with `sudo dnf install {program}`")
    } else if os_release.contains("ID=debian")
        || os_release.contains("ID=ubuntu")
        || os_release.contains("ID_LIKE=debian")
    {
        format!("install it with `sudo apt install {program}`")
    } else {
        format!("install `{program}` with your distribution's package manager")
    };
    format!("{command}, then run `dymus doctor`")
}
