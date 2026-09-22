//! Non-interactive playback and control through mpv's local IPC socket.
use crate::{
    auth,
    config::Config,
    innertube::{InnerTube, LibraryKind, SearchFilter},
    model::Track,
    player,
};
use anyhow::{Context, Result, bail, ensure};
use crossterm::{
    style::{Color, Stylize},
    terminal,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Command,
    time::{Duration, Instant},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy)]
pub enum Target {
    Song,
    Album,
    Playlist,
    Library(LibraryKind),
}

#[derive(Clone, Copy)]
pub enum Control {
    Pause,
    Resume,
    Toggle,
    Next,
    Previous,
    Stop,
    Volume(u8),
    Status,
}

#[derive(Deserialize, Serialize)]
struct State {
    tracks: Vec<Track>,
}

pub async fn play(target: Target, query: &str, detach: bool, volume: u8) -> Result<()> {
    ensure!(volume <= 100, "Volume must be between 0 and 100");
    let tracks = resolve_target(target, query).await?;
    ensure!(!tracks.is_empty(), "No playable tracks found");
    let (socket, state) = paths()?;
    ensure_socket_is_available(&socket).await?;
    fs::write(
        &state,
        serde_json::to_vec(&State {
            tracks: tracks.clone(),
        })?,
    )
    .with_context(|| format!("Cannot save {}", state.display()))?;

    notice("Resolving the first track…");
    let first_source = player::resolve(&tracks[0].id).await?;
    let mut command = Command::new("mpv");
    command
        .args([
            "--no-config",
            "--no-terminal",
            "--no-video",
            "--no-ytdl",
            "--audio-display=no",
            "--audio-client-name=Dymus headless",
        ])
        .arg(format!("--input-ipc-server={}", socket.display()))
        .arg(format!("--volume={volume}"))
        .arg("--")
        .arg(first_source)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command
        .spawn()
        .context("Cannot start mpv; run `dymus doctor`")?;
    wait_for_socket(&socket, &mut child).await?;
    let config = Config::load()?;
    if tracks.len() > 1
        || config.report_history
        || (config.lastfm_scrobbling && auth::load_lastfm()?.is_some())
    {
        spawn_resolver()?;
    }
    println!(
        "{} {} track{}  {}",
        "▶".with(Color::Green).bold(),
        tracks.len(),
        if tracks.len() == 1 { "" } else { "s" },
        tracks[0].title.as_str().with(Color::White).bold(),
    );
    if detach {
        println!(
            "{} Control: {}  {}",
            "✓".with(Color::Green),
            "dymus control status".with(Color::Cyan),
            "dymus control stop".with(Color::Cyan),
        );
        return Ok(());
    }
    if let Err(error) = display_progress(&socket, &tracks).await {
        eprintln!("Progress display stopped: {error:#}");
    }
    let status = child.wait().await.context("Cannot wait for mpv")?;
    if !status.success() {
        bail!("mpv exited with {status}");
    }
    Ok(())
}

/// Renders a single, continually updated line for attached headless playback.
async fn display_progress(socket_path: &Path, tracks: &[Track]) -> Result<()> {
    let socket = UnixStream::connect(socket_path)
        .await
        .context("Cannot connect to mpv for progress updates")?;
    let (read, mut write) = socket.into_split();
    for (id, property) in [
        (1, "time-pos"),
        (2, "duration"),
        (3, "pause"),
        (4, "playlist-pos"),
    ] {
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"command": ["observe_property", id, property], "request_id": id})
                )
                .as_bytes(),
            )
            .await?;
    }
    let mut lines = BufReader::new(read).lines();
    let mut position = None;
    let mut duration = None;
    let mut paused = false;
    let mut track_index = 0;
    let mut refresh = tokio::time::interval_at(
        Instant::now() + Duration::from_millis(100),
        Duration::from_secs(1),
    );
    let mut changed = false;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let event: Value = serde_json::from_str(&line)
                    .context("Invalid progress event from mpv")?;
                if event["event"] != "property-change" {
                    continue;
                }
                match event["name"].as_str() {
                    Some("time-pos") => position = event["data"].as_f64(),
                    Some("duration") => duration = event["data"].as_f64(),
                    Some("pause") => paused = event["data"].as_bool().unwrap_or(false),
                    Some("playlist-pos") => {
                        track_index = event["data"].as_u64().unwrap_or_default() as usize;
                        position = None;
                        duration = None;
                    }
                    _ => continue,
                }
                changed = true;
            }
            _ = refresh.tick(), if changed => {
                let label = tracks
                    .get(track_index)
                    .map(|track| format!("{} — {}", track.title, track.artist))
                    .unwrap_or_else(|| "Unknown track".into());
                print!(
                    "\r\x1b[2K{} {} {}",
                    if paused {
                        "Ⅱ".with(Color::Yellow)
                    } else {
                        "▶".with(Color::Green)
                    },
                    label.with(Color::White).bold(),
                    format!("· {} / {}", clock(position), clock(duration)).with(Color::DarkGrey),
                );
                io::stdout()
                    .flush()
                    .context("Cannot update playback progress")?;
                changed = false;
            }
        }
    }
    println!();
    Ok(())
}

/// Resolves upcoming tracks after headless mpv has started, avoiding a long
/// up-front wait for albums and playlists.
pub async fn resolve_remaining(start: usize) -> Result<()> {
    let (socket_path, state_path) = paths()?;
    let state: State = serde_json::from_slice(
        &fs::read(&state_path).with_context(|| format!("Cannot read {}", state_path.display()))?,
    )
    .context("Headless playback queue is invalid")?;
    let config = Config::load()?;
    let history_task = if config.report_history {
        let api = InnerTube::configured()?;
        api.is_authenticated().then(|| {
            tokio::spawn(watch_history(
                socket_path.clone(),
                state.tracks.clone(),
                api,
                config.report_history_after_seconds,
            ))
        })
    } else {
        None
    };
    let lastfm_task = if config.lastfm_scrobbling {
        auth::load_lastfm()?
            .and_then(|credentials| crate::lastfm::Client::new(credentials).ok())
            .map(|client| {
                tokio::spawn(watch_lastfm(
                    socket_path.clone(),
                    state.tracks.clone(),
                    client,
                ))
            })
    } else {
        None
    };
    for track in state.tracks.iter().skip(start) {
        let source = match player::resolve(&track.id).await {
            Ok(source) => source,
            Err(error) => {
                eprintln!("Could not resolve {}: {error:#}", track.title);
                continue;
            }
        };
        let mut socket = match UnixStream::connect(&socket_path).await {
            Ok(socket) => socket,
            // Playback ended while a later track was resolving.
            Err(_) => return Ok(()),
        };
        if command(&mut socket, json!(["loadfile", source, "append-play"]))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
    if let Some(task) = history_task {
        let _ = task.await;
    }
    if let Some(task) = lastfm_task {
        let _ = task.await;
    }
    Ok(())
}

async fn watch_lastfm(
    socket_path: PathBuf,
    tracks: Vec<Track>,
    client: crate::lastfm::Client,
) -> Result<()> {
    let socket = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = socket.into_split();
    for (id, property) in [(1, "time-pos"), (2, "playlist-pos")] {
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"command": ["observe_property", id, property], "request_id": id})
                )
                .as_bytes(),
            )
            .await?;
    }
    let mut lines = BufReader::new(read).lines();
    let mut index = 0usize;
    let mut started_at = None;
    let mut announced = HashSet::new();
    let mut scrobbled = HashSet::new();
    while let Some(line) = lines.next_line().await? {
        let event: Value = serde_json::from_str(&line)?;
        match event["name"].as_str() {
            Some("playlist-pos") => {
                index = event["data"].as_u64().unwrap_or_default() as usize;
                started_at = Some(crate::auth::unix_timestamp()?);
                if announced.insert(index)
                    && let Some(track) = tracks
                        .get(index)
                        .filter(|track| !track.id.starts_with("radio:"))
                {
                    let track = track.clone();
                    let duration = crate::lastfm::duration_seconds(&track);
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Err(error) = client.now_playing(&track, duration).await {
                            eprintln!("Could not update Last.fm now playing: {error:#}");
                        }
                    });
                }
            }
            Some("time-pos") => {
                let position = event["data"].as_f64().unwrap_or_default();
                let Some(track) = tracks
                    .get(index)
                    .filter(|track| !track.id.starts_with("radio:"))
                else {
                    continue;
                };
                let duration = crate::lastfm::duration_seconds(track);
                if let (Some(after), Some(timestamp)) =
                    (crate::lastfm::eligible_after(duration), started_at)
                    && position >= after as f64
                    && scrobbled.insert(index)
                {
                    let track = track.clone();
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Err(error) = client.scrobble(&track, duration, timestamp).await {
                            eprintln!("Could not scrobble to Last.fm: {error:#}");
                        }
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn watch_history(
    socket_path: PathBuf,
    tracks: Vec<Track>,
    api: InnerTube,
    threshold: u64,
) -> Result<()> {
    let socket = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = socket.into_split();
    for (id, property) in [(1, "time-pos"), (2, "playlist-pos")] {
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"command": ["observe_property", id, property], "request_id": id})
                )
                .as_bytes(),
            )
            .await?;
    }
    let mut lines = BufReader::new(read).lines();
    let mut index = 0usize;
    let mut reported = HashSet::new();
    while let Some(line) = lines.next_line().await? {
        let event: Value = serde_json::from_str(&line)?;
        match event["name"].as_str() {
            Some("playlist-pos") => index = event["data"].as_u64().unwrap_or_default() as usize,
            Some("time-pos") if event["data"].as_f64().unwrap_or_default() >= threshold as f64 => {
                if reported.insert(index)
                    && let Some(track) = tracks.get(index)
                    && let Err(error) = api.add_history_item(&track.id).await
                {
                    eprintln!("Could not report playback history: {error:#}");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub async fn control(action: Control) -> Result<()> {
    let (socket, state_path) = paths()?;
    let mut socket = UnixStream::connect(&socket)
        .await
        .context("No headless Dymus player is running")?;
    match action {
        Control::Pause => {
            command(&mut socket, json!(["set_property", "pause", true])).await?;
        }
        Control::Resume => {
            command(&mut socket, json!(["set_property", "pause", false])).await?;
        }
        Control::Toggle => {
            command(&mut socket, json!(["cycle", "pause"])).await?;
        }
        Control::Next => {
            command(&mut socket, json!(["playlist-next", "force"])).await?;
        }
        Control::Previous => {
            command(&mut socket, json!(["playlist-prev", "force"])).await?;
        }
        Control::Stop => {
            command(&mut socket, json!(["quit"])).await?;
        }
        Control::Volume(level) => {
            ensure!(level <= 100, "Volume must be between 0 and 100");
            command(&mut socket, json!(["set_property", "volume", level])).await?;
        }
        Control::Status => print_status(&mut socket, &state_path).await?,
    }
    Ok(())
}

async fn resolve_target(target: Target, query: &str) -> Result<Vec<Track>> {
    let result_limit = Config::load()?.headless_results;
    let label = match target {
        Target::Song => "songs",
        Target::Album => "albums",
        Target::Playlist => "playlists",
        Target::Library(kind) => library_label(kind),
    };
    if matches!(target, Target::Library(_)) {
        notice(&format!("Loading library {label}…"));
    } else {
        notice(&format!("Searching {label}…"));
    }
    let api = InnerTube::configured()?;
    match target {
        Target::Song => {
            let tracks = api
                .search(query, SearchFilter::Songs)
                .await?
                .tracks
                .into_iter()
                .take(result_limit)
                .collect::<Vec<_>>();
            ensure!(!tracks.is_empty(), "No playable song found");
            Ok(vec![choose("song", &tracks, |track| {
                let album = (!track.album.is_empty()).then(|| format!(" · {}", track.album));
                format!(
                    "{} — {}{}",
                    track.title,
                    track.artist,
                    album.unwrap_or_default()
                )
            })?])
        }
        Target::Album => {
            collection_tracks(&api, query, SearchFilter::Albums, "album", result_limit).await
        }
        Target::Playlist => {
            collection_tracks(
                &api,
                query,
                SearchFilter::Playlists,
                "playlist",
                result_limit,
            )
            .await
        }
        Target::Library(kind) => library_item_tracks(&api, kind, result_limit).await,
    }
}

async fn library_item_tracks(
    api: &InnerTube,
    kind: LibraryKind,
    result_limit: usize,
) -> Result<Vec<Track>> {
    let label = library_label(kind);
    let items = api
        .library(kind)
        .await?
        .items
        .into_iter()
        .filter(|item| {
            item.track.is_some() || !item.browse_id.is_empty() || !item.playlist_id.is_empty()
        })
        .take(result_limit)
        .collect::<Vec<_>>();
    ensure!(!items.is_empty(), "No playable library {label} found");
    let item = choose(&format!("library {label}"), &items, |item| {
        if item.detail.is_empty() {
            item.title.clone()
        } else {
            format!("{} — {}", item.title, item.detail)
        }
    })?;
    notice(&format!("Loading {} tracks…", item.title));
    api.library_tracks(&item).await.map(|page| page.tracks)
}

fn library_label(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Playlists => "playlists",
        LibraryKind::Albums => "albums",
        LibraryKind::Artists => "artists",
        LibraryKind::Podcasts => "podcasts",
    }
}

async fn collection_tracks(
    api: &InnerTube,
    query: &str,
    filter: SearchFilter,
    label: &str,
    result_limit: usize,
) -> Result<Vec<Track>> {
    let items = api
        .search(query, filter)
        .await?
        .items
        .into_iter()
        .filter(|item| !item.browse_id.is_empty() || !item.playlist_id.is_empty())
        .take(result_limit)
        .collect::<Vec<_>>();
    ensure!(!items.is_empty(), "No playable {label} found");
    let item = choose(label, &items, |item| {
        if item.detail.is_empty() {
            item.title.clone()
        } else {
            format!("{} — {}", item.title, item.detail)
        }
    })?;
    notice(&format!("Loading {label} tracks…"));
    api.library_tracks(&item).await.map(|page| page.tracks)
}

fn choose<T: Clone>(label: &str, choices: &[T], format: impl Fn(&T) -> String) -> Result<T> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Choosing a {label} requires an interactive terminal"
    );
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(80);
    let index_width = choices.len().to_string().len().max(1);
    let table_width = terminal_width.min(120);
    let item_width = table_width.saturating_sub(index_width + 7);
    if item_width < 12 {
        println!(
            "{}",
            format!("Top {label} results").with(Color::Cyan).bold()
        );
        for (index, choice) in choices.iter().enumerate() {
            let value = truncate(
                &format(choice),
                terminal_width.saturating_sub(index_width + 3),
            );
            println!(
                "{} {}",
                format!("{:>index_width$}.", index + 1)
                    .with(Color::Yellow)
                    .bold(),
                value.with(Color::White)
            );
        }
        return prompt_for_choice(choices);
    }
    let inner_width = index_width + item_width + 5;
    println!(
        "{}",
        format!("┌{}┐", "─".repeat(inner_width)).with(Color::DarkGrey)
    );
    println!(
        "{}{}{}",
        "│ ".with(Color::DarkGrey),
        pad(
            &truncate(&format!("Top {label} results"), inner_width - 2),
            inner_width - 2
        )
        .with(Color::Cyan)
        .bold(),
        " │".with(Color::DarkGrey)
    );
    println!(
        "{}",
        format!(
            "├{}┬{}┤",
            "─".repeat(index_width + 2),
            "─".repeat(item_width + 2)
        )
        .with(Color::DarkGrey)
    );
    for (index, choice) in choices.iter().enumerate() {
        let value = truncate(&format(choice), item_width);
        println!(
            "{}{}{}{}{}",
            "│ ".with(Color::DarkGrey),
            format!("{:>index_width$}", index + 1)
                .with(Color::Yellow)
                .bold(),
            " │ ".with(Color::DarkGrey),
            pad(&value, item_width).with(Color::White),
            " │".with(Color::DarkGrey),
        );
    }
    println!(
        "{}",
        format!(
            "└{}┴{}┘",
            "─".repeat(index_width + 2),
            "─".repeat(item_width + 2)
        )
        .with(Color::DarkGrey)
    );
    prompt_for_choice(choices)
}

fn prompt_for_choice<T: Clone>(choices: &[T]) -> Result<T> {
    loop {
        print!(
            "{}",
            format!("Select a result [1-{}]: ", choices.len())
                .with(Color::Cyan)
                .bold()
        );
        io::stdout()
            .flush()
            .context("Cannot write selection prompt")?;
        let mut input = String::new();
        let read = io::stdin()
            .read_line(&mut input)
            .context("Cannot read selection")?;
        ensure!(read != 0, "Selection was cancelled");
        if let Ok(index) = input.trim().parse::<usize>()
            && (1..=choices.len()).contains(&index)
        {
            return Ok(choices[index - 1].clone());
        }
        eprintln!("Enter a number from 1 to {}.", choices.len());
    }
}

fn notice(message: &str) {
    println!(
        "{} {}",
        "●".with(Color::Blue).bold(),
        message.with(Color::DarkGrey)
    );
}

fn truncate(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width + 1 > width {
            break;
        }
        result.push(character);
        used += character_width;
    }
    result.push('…');
    result
}

fn pad(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(UnicodeWidthStr::width(value)))
    )
}

fn spawn_resolver() -> Result<()> {
    let executable = env::current_exe().context("Cannot locate the Dymus executable")?;
    Command::new(executable)
        .args(["__headless-resolve", "--start", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .context("Cannot start background track resolver")?;
    Ok(())
}

fn paths() -> Result<(PathBuf, PathBuf)> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("XDG_CONFIG_HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("Cannot locate a directory for headless playback")?
        .join("dymus");
    fs::create_dir_all(&base).with_context(|| format!("Cannot create {}", base.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    }
    Ok((base.join("headless.sock"), base.join("headless.json")))
}

async fn ensure_socket_is_available(path: &Path) -> Result<()> {
    match UnixStream::connect(path).await {
        Ok(_) => bail!("A headless Dymus player is already running; use `dymus control`"),
        Err(_) => {}
    }
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            ensure!(
                fs::symlink_metadata(path)?.file_type().is_socket(),
                "Headless socket path is not a socket"
            );
        }
        fs::remove_file(path)
            .with_context(|| format!("Cannot remove stale socket {}", path.display()))?;
    }
    Ok(())
}

async fn wait_for_socket(path: &Path, child: &mut tokio::process::Child) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("mpv exited during startup: {status}");
            }
            if UnixStream::connect(path).await.is_ok() {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .context("mpv did not open its control socket")??;
    Ok(())
}

async fn command(socket: &mut UnixStream, value: Value) -> Result<Value> {
    socket
        .write_all(format!("{}\n", json!({"command": value, "request_id": 1})).as_bytes())
        .await?;
    let mut lines = BufReader::new(socket).lines();
    let line = lines
        .next_line()
        .await?
        .context("mpv closed its control socket")?;
    let response: Value = serde_json::from_str(&line).context("Invalid response from mpv")?;
    ensure!(
        response["error"] == "success",
        "mpv command failed: {}",
        response["error"].as_str().unwrap_or("unknown error")
    );
    Ok(response["data"].clone())
}

async fn print_status(socket: &mut UnixStream, state_path: &Path) -> Result<()> {
    let title = command(socket, json!(["get_property", "media-title"])).await?;
    let paused = command(socket, json!(["get_property", "pause"])).await?;
    let position = command(socket, json!(["get_property", "time-pos"])).await?;
    let duration = command(socket, json!(["get_property", "duration"])).await?;
    let index = command(socket, json!(["get_property", "playlist-pos"])).await?;
    let track = fs::read(&state_path)
        .ok()
        .and_then(|body| serde_json::from_slice::<State>(&body).ok())
        .and_then(|state| {
            index
                .as_u64()
                .and_then(|index| state.tracks.get(index as usize).cloned())
        });
    if let Some(track) = track {
        println!("{} — {}", track.title, track.artist);
    } else {
        println!("{}", title.as_str().unwrap_or("Unknown track"));
    }
    println!(
        "{} · {} / {}",
        if paused.as_bool() == Some(true) {
            "paused"
        } else {
            "playing"
        },
        clock(position.as_f64()),
        clock(duration.as_f64())
    );
    Ok(())
}

fn clock(seconds: Option<f64>) -> String {
    let seconds = seconds.unwrap_or_default().max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
