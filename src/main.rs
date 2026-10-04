mod app;
mod auth;
mod cache;
mod config;
mod download;
mod download_library;
mod headless;
mod headless_service;
mod headless_ui;
mod innertube;
mod lastfm;
mod local;
mod lyrics;
mod manual;
mod model;
mod mpris;
mod player;
mod radio;
mod ui;
mod visualizer;
mod youtube;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use config::Config;
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
    /// Search all YouTube, choose a result, then play it as audio or video.
    Search {
        query: String,
        /// Print results as JSON without prompting or playing.
        #[arg(long)]
        json: bool,
        /// Override headless_search_results (1 to 50).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=50))]
        limit: Option<u8>,
        /// Play the selected result as video without the mode prompt.
        #[arg(long, conflicts_with = "json")]
        video: bool,
        #[arg(long)]
        detach: bool,
        #[arg(long, default_value_t = 80)]
        volume: u8,
    },
    /// Check playback, download, and optional visualizer dependencies.
    Doctor,
    /// Print the bundled man page or install it for your user.
    Man {
        #[arg(long)]
        install: bool,
        /// Man root directory; the page is placed under man1/dymus.1.
        #[arg(long, requires = "install")]
        directory: Option<std::path::PathBuf>,
    },
    /// Play a song, album, or playlist without starting the TUI.
    Play {
        #[command(subcommand)]
        target: PlayTarget,
        /// Play video for every track (streaming uses the configured height cap).
        #[arg(long, global = true)]
        video: bool,
        /// Leave playback running after this command exits.
        #[arg(long, global = true)]
        detach: bool,
        /// Initial playback volume, from 0 to 100.
        #[arg(long, global = true, default_value_t = 80)]
        volume: u8,
    },
    /// Download audio or video without starting the TUI.
    Download {
        #[command(subcommand)]
        target: DownloadTarget,
        /// Download video for every selected track instead of audio.
        #[arg(long, global = true)]
        video: bool,
        /// Override the configured destination directory.
        #[arg(long, global = true)]
        path: Option<std::path::PathBuf>,
    },
    /// List local collections and tracks, optionally as JSON.
    Local {
        query: Option<String>,
        #[arg(long)]
        path: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
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
        #[arg(long)]
        session: Option<String>,
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
    /// Play downloaded or imported local media, directories, or M3U playlists.
    Local {
        query: Option<String>,
        #[arg(long)]
        path: Option<std::path::PathBuf>,
        #[arg(long)]
        id: Option<String>,
        /// Queue all matching local tracks without prompting.
        #[arg(long)]
        all: bool,
    },
    /// Choose an item from a signed-in YouTube Music library category.
    Library {
        #[command(subcommand)]
        kind: LibraryTarget,
    },
}

#[derive(Subcommand)]
enum DownloadTarget {
    /// Find a song or download a direct YouTube URL as audio.
    Song { query: String },
    /// Find a video or download a direct YouTube URL.
    Video { query: String },
    /// Choose an album or download its direct playlist URL.
    Album { query: String },
    /// Choose a playlist or download its direct URL.
    Playlist { query: String },
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
    /// Import YouTube Music cookies from a browser supported by yt-dlp.
    Browser {
        /// Browser to read (defaults to Firefox).
        #[arg(long, default_value = "firefox", value_parser = ["brave", "chrome", "chromium", "edge", "firefox", "opera", "safari", "vivaldi", "whale"])]
        browser: String,
        /// Browser profile name or path (defaults to the active profile).
        #[arg(long)]
        profile: Option<String>,
        /// Firefox Multi-Account Containers container name (Firefox only).
        #[arg(long)]
        container: Option<String>,
        /// Chromium cookie decryption keyring (basictext, gnomekeyring, kwallet, kwallet5, or kwallet6).
        #[arg(long)]
        keyring: Option<String>,
        /// X-Goog-AuthUser account index (usually 0).
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

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let feedback = matches!(
        &cli.command,
        Some(
            Command::Search { json: false, .. }
                | Command::Play { .. }
                | Command::Control { .. }
                | Command::Download { .. }
                | Command::Local { json: false, .. }
        )
    );
    match execute(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) if error.is::<headless_ui::Cancelled>() => {
            println!("Cancelled.");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            if feedback {
                let _ = headless_ui::report_error(&error);
            } else {
                eprintln!("Error: {error:#}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<()> {
    // The background worker handles asynchronous I/O and needs no CPU-sized pool.
    let mut runtime = if matches!(
        &cli.command,
        Some(Command::HeadlessResolve { .. } | Command::Download { .. })
    ) {
        tokio::runtime::Builder::new_current_thread()
    } else {
        tokio::runtime::Builder::new_multi_thread()
    };
    runtime.enable_all().build()?.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Search {
            query,
            json,
            video,
            limit,
            detach,
            volume,
        }) => {
            if json {
                let limit = limit
                    .map(usize::from)
                    .unwrap_or(config::Config::load()?.headless_search_results);
                let tracks = youtube::search(&query, limit).await?;
                println!("{}", serde_json::to_string_pretty(&tracks)?);
            } else {
                headless::search(&query, detach, volume, limit.map(usize::from), video).await?;
            }
        }
        Some(Command::Doctor) => doctor().await?,
        Some(Command::Man { install, directory }) => manual::run(install, directory.as_deref())?,
        Some(Command::Download {
            target,
            video,
            path,
        }) => {
            let (target, query, video) = match target {
                DownloadTarget::Song { query } => (headless::Target::Song, query, video),
                DownloadTarget::Video { query } => (headless::Target::Song, query, true),
                DownloadTarget::Album { query } => (headless::Target::Album, query, video),
                DownloadTarget::Playlist { query } => (headless::Target::Playlist, query, video),
            };
            download::run(target, &query, video, path.as_deref()).await?;
        }
        Some(Command::Local { query, path, json }) => {
            let library = local::load(Config::load()?, path)
                .await?
                .filtered(query.as_deref().unwrap_or(""));
            if json {
                println!("{}", serde_json::to_string_pretty(&library)?);
            } else {
                if library.collections.is_empty() {
                    headless_ui::print_empty(
                        "No local media found",
                        if query
                            .as_deref()
                            .is_some_and(|query| !query.trim().is_empty())
                        {
                            "No local tracks or collections match your search."
                        } else {
                            "No downloaded or imported media is available."
                        },
                        "Try a shorter query, use `dymus local --path /path/to/music`, or download a track with `dymus download song`.",
                    )?;
                }
                for collection in &library.collections {
                    println!(
                        "{} · {} · {} track(s) · {}\n  id: {}",
                        collection.title,
                        collection.kind,
                        collection.tracks.len(),
                        collection.media_type,
                        collection.id
                    );
                    for track in &collection.tracks {
                        println!("  {} — {}", track.title, track.artist);
                    }
                }
                for warning in &library.warnings {
                    eprintln!("{warning}");
                }
            }
        }
        Some(Command::Play {
            target,
            video,
            detach,
            volume,
        }) => {
            let target = match target {
                PlayTarget::Local {
                    query,
                    path,
                    id,
                    all,
                } => {
                    headless::play_local(
                        query.as_deref().unwrap_or(""),
                        path,
                        id.as_deref(),
                        all,
                        detach,
                        volume,
                        video,
                    )
                    .await?;
                    return Ok(());
                }
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
            headless::play(target.0, &target.1, detach, volume, video).await?;
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
        Some(Command::HeadlessResolve { start, session }) => {
            headless::resolve_remaining(start, session.as_deref()).await?
        }
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
            action:
                AuthCommand::Browser {
                    browser,
                    profile,
                    container,
                    keyring,
                    auth_user,
                },
        }) => {
            let credentials = auth::from_browser(
                &browser,
                profile.as_deref(),
                container.as_deref(),
                keyring.as_deref(),
                &auth_user,
            )
            .await?;
            let account = innertube::InnerTube::new()?
                .with_auth(credentials.clone())
                .validate_session()
                .await?;
            auth::save(&credentials)?;
            println!(
                "Signed in as {account}. Browser credentials saved to Dymus's private configuration directory."
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
    for program in ["ffmpeg", "ffprobe"] {
        match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(program)
                .arg("-version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                doctor_ok(&format!("{program}: available"))
            }
            _ => doctor_warning(&format!(
                "{program}: missing (downloads require the ffmpeg package)"
            )),
        }
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

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn missing_manual_options(command: &clap::Command, manual: &str) -> Vec<String> {
        let text = manual.replace("\\-", "-");
        let documented = text
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
            .collect::<std::collections::HashSet<_>>();
        fn visit(
            command: &clap::Command,
            path: &str,
            documented: &std::collections::HashSet<&str>,
            missing: &mut Vec<String>,
        ) {
            for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
                if let Some(long) = arg.get_long() {
                    let option = format!("--{long}");
                    if !documented.contains(option.as_str()) {
                        missing.push(format!("{path}: {option}"));
                    }
                }
            }
            for child in command
                .get_subcommands()
                .filter(|command| !command.is_hide_set())
            {
                visit(
                    child,
                    &format!("{path} {}", child.get_name()),
                    documented,
                    missing,
                );
            }
        }
        let mut missing = Vec::new();
        visit(command, command.get_name(), &documented, &mut missing);
        missing
    }

    #[test]
    fn manual_documents_all_public_long_options() {
        use clap::CommandFactory;
        let mut command = Cli::command();
        command.build();
        let missing = missing_manual_options(&command, &manual::render());
        assert!(
            missing.is_empty(),
            "Undocumented CLI options: {}",
            missing.join(", ")
        );
    }

    #[test]
    fn manual_coverage_detects_nested_flags_and_excludes_hidden_options() {
        let mut command = clap::Command::new("dymus")
            .disable_help_flag(true)
            .disable_help_subcommand(true)
            .arg(clap::Arg::new("path").long("path"))
            .arg(clap::Arg::new("internal").long("internal").hide(true))
            .subcommand(
                clap::Command::new("public")
                    .disable_help_flag(true)
                    .arg(clap::Arg::new("new-feature").long("new-feature")),
            )
            .subcommand(
                clap::Command::new("worker")
                    .hide(true)
                    .arg(clap::Arg::new("private").long("private")),
            );
        command.build();
        assert_eq!(
            missing_manual_options(&command, "\\-\\-path"),
            vec!["dymus public: --new-feature"]
        );
        assert_eq!(
            missing_manual_options(&command, "--pathology --new-feature"),
            vec!["dymus: --path"]
        );
        assert!(missing_manual_options(&command, "--path --new-feature").is_empty());
    }

    #[test]
    fn local_play_and_listing_accept_script_and_file_options() {
        assert!(matches!(
            Cli::try_parse_from(["dymus", "local", "Artist", "--json"])
                .unwrap()
                .command,
            Some(Command::Local { json: true, .. })
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "dymus",
                "play",
                "local",
                "--all",
                "--detach",
                "--path",
                "/tmp/music"
            ])
            .unwrap()
            .command,
            Some(Command::Play {
                target: PlayTarget::Local {
                    all: true,
                    path: Some(_),
                    ..
                },
                detach: true,
                ..
            })
        ));
        assert!(
            Cli::try_parse_from(["dymus", "play", "local", "/tmp/video.mp4", "--video"]).is_ok()
        );
    }

    #[test]
    fn download_targets_accept_global_video_and_path_overrides() {
        for target in ["song", "video", "album", "playlist"] {
            let cli = Cli::try_parse_from([
                "dymus",
                "download",
                target,
                "query",
                "--video",
                "--path",
                "/tmp/music space",
            ])
            .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Download {
                    video: true,
                    path: Some(_),
                    ..
                })
            ));
        }
        assert!(Cli::try_parse_from(["dymus", "download", "--video", "playlist", "query"]).is_ok());
        assert!(Cli::try_parse_from(["dymus", "download", "song"]).is_err());
    }

    #[test]
    fn video_flag_applies_to_every_headless_play_target() {
        for args in [
            vec!["dymus", "play", "song", "query", "--video"],
            vec!["dymus", "play", "--video", "album", "query"],
            vec!["dymus", "play", "playlist", "query", "--video", "--detach"],
            vec!["dymus", "play", "library", "playlists", "--video"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Play { video: true, .. })
            ));
        }
        assert!(matches!(
            Cli::try_parse_from(["dymus", "play", "song", "query"])
                .unwrap()
                .command,
            Some(Command::Play { video: false, .. })
        ));
        assert!(Cli::try_parse_from(["dymus", "play", "video", "query"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["dymus", "search", "query", "--video"])
                .unwrap()
                .command,
            Some(Command::Search { video: true, .. })
        ));
        assert!(Cli::try_parse_from(["dymus", "search", "query", "--json", "--video"]).is_err());
    }

    #[test]
    fn search_accepts_interactive_controls_and_json_with_a_bounded_limit() {
        let cli = Cli::try_parse_from([
            "dymus",
            "search",
            "any video",
            "--detach",
            "--limit",
            "15",
            "--volume",
            "65",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search {
                json: false,
                detach: true,
                limit: Some(15),
                volume: 65,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["dymus", "search", "any video", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Search { json: true, .. })
        ));
        for limit in ["0", "51"] {
            assert!(Cli::try_parse_from(["dymus", "search", "query", "--limit", limit]).is_err());
        }
    }
}
