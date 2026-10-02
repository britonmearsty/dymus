//! Non-interactive playback and control through mpv's local IPC socket.
use crate::{
    config::Config,
    headless_ui::{self, Flow},
    innertube::{InnerTube, LibraryKind, SearchFilter},
    model::Track,
    player,
};
use anyhow::{Context, Result, bail, ensure};
use crossterm::terminal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::{
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
    #[serde(default)]
    session: String,
    #[serde(default)]
    video: bool,
    #[serde(default = "default_video_height")]
    video_height: u32,
}

fn default_video_height() -> u32 {
    Config::default().video_height
}

pub async fn search(
    query: &str,
    detach: bool,
    volume: u8,
    limit: Option<usize>,
    video: bool,
) -> Result<()> {
    ensure!(volume <= 100, "Volume must be between 0 and 100");
    let config = Config::load()?;
    let mut flow = Flow::new(
        "YouTube search",
        &format!(
            "{} · {} results · {}",
            headless_ui::clean(query),
            limit.unwrap_or(config.headless_search_results),
            if detach {
                "detached playback"
            } else {
                "attached playback"
            }
        ),
    );
    let track = choose_youtube(query, limit, &mut flow).await?;
    let video_height = Config::load()?.video_height;
    let video = if video {
        true
    } else {
        flow.choose("Choose playback mode", &[false, true], |video| {
            if *video {
                format!("Video (up to {video_height}p)")
            } else {
                "Audio only".into()
            }
        })?
    };
    play_tracks(vec![track], video, detach, volume, &mut flow).await
}

async fn resolve_source(id: &str, video: bool, height: u32) -> Result<player::VideoSource> {
    if video {
        player::resolve_video(id, height).await
    } else {
        Ok(player::VideoSource {
            video: player::resolve(id).await?,
            audio: None,
        })
    }
}

pub(crate) fn append_command(
    source: &player::VideoSource,
    video: bool,
    has_index: bool,
    tag: &str,
) -> Value {
    // Replace external audio for each entry, including combined-stream fallback.
    // mpv audio-files is a colon-separated path list on Linux.
    let audio = source
        .audio
        .as_ref()
        .map(|url| url.replace('\\', "\\\\").replace(':', "\\:"))
        .unwrap_or_default();
    let mut command = vec![json!("loadfile"), json!(source.video), json!("append-play")];
    if has_index {
        command.push(json!(-1));
    }
    let mut options = json!({"force-media-title": tag});
    if video {
        options["audio-files"] = json!(audio);
    }
    command.push(options);
    Value::Array(command)
}

pub(crate) fn loadfile_has_index(commands: &Value) -> bool {
    commands
        .as_array()
        .into_iter()
        .flatten()
        .find(|command| command["name"] == "loadfile")
        .and_then(|command| command["args"].as_array())
        .is_some_and(|args| args.iter().any(|arg| arg["name"] == "index"))
}

async fn choose_youtube(query: &str, limit: Option<usize>, flow: &mut Flow) -> Result<Track> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Choosing a YouTube result requires an interactive terminal; use `dymus search --json` for JSON results"
    );
    let limit = limit.unwrap_or(Config::load()?.headless_search_results);
    let tracks = flow
        .load(
            "Search YouTube",
            &format!("{} · up to {limit} results", headless_ui::clean(query)),
            crate::youtube::search(query, limit),
        )
        .await?;
    ensure!(!tracks.is_empty(), "No YouTube results found");
    flow.choose("Choose a YouTube result", &tracks, |track| {
        let duration = if track.duration.is_empty() {
            String::new()
        } else {
            format!(" · {}", track.duration)
        };
        format!(
            "{}\n{}{duration}",
            headless_ui::clean(&track.title),
            headless_ui::clean(&track.artist)
        )
    })
}

pub async fn play_local(
    query: &str,
    path: Option<PathBuf>,
    id: Option<&str>,
    all: bool,
    detach: bool,
    volume: u8,
    video: bool,
) -> Result<()> {
    ensure!(volume <= 100, "Volume must be between 0 and 100");
    let explicit = path.is_some() || Path::new(query).exists();
    let path = path.or_else(|| Path::new(query).exists().then(|| PathBuf::from(query)));
    let filter = if Path::new(query).exists() { "" } else { query };
    let mut flow = Flow::new(
        "Local playback",
        "Offline files · downloaded and imported collections",
    );
    let library = flow
        .load(
            "Read local library",
            "Read metadata and available local files",
            crate::local::load(Config::load()?, path),
        )
        .await?
        .filtered(filter);
    for warning in &library.warnings {
        eprintln!("{warning}");
    }
    let mut collections = library
        .collections
        .into_iter()
        .filter(|c| !c.tracks.is_empty() && id.is_none_or(|id| c.id == id))
        .collect::<Vec<_>>();
    ensure!(
        !collections.is_empty(),
        "No playable local content found; download media or configure [local].roots (use `dymus local --json` to list collection IDs)"
    );
    let tracks = if all {
        collections.into_iter().flat_map(|c| c.tracks).collect()
    } else {
        let collection =
            if collections.len() == 1 && (explicit || id.is_some() || !io::stdin().is_terminal()) {
                collections.remove(0)
            } else {
                flow.choose("Choose a local collection", &collections, |c| {
                    let missing = if c.missing > 0 {
                        format!(" · {} missing", c.missing)
                    } else {
                        String::new()
                    };
                    format!(
                        "{}\n{} tracks · {}{missing}",
                        c.title,
                        c.tracks.len(),
                        c.media_type
                    )
                })?
            };
        if explicit || id.is_some() || !io::stdin().is_terminal() || collection.tracks.len() == 1 {
            collection.tracks
        } else {
            let choices = std::iter::once(None)
                .chain((0..collection.tracks.len()).map(Some))
                .collect::<Vec<_>>();
            let selected =
                flow.choose(
                    "Play collection or choose a track",
                    &choices,
                    |index| match index {
                        None => format!("Entire collection · {} tracks", collection.tracks.len()),
                        Some(index) => format!(
                            "{}\n{} · {}",
                            collection.tracks[*index].title,
                            collection.tracks[*index].artist,
                            collection.tracks[*index].duration
                        ),
                    },
                )?;
            match selected {
                None => collection.tracks,
                Some(index) => vec![collection.tracks[index].clone()],
            }
        }
    };
    play_tracks(tracks, video, detach, volume, &mut flow).await
}

pub async fn play(
    target: Target,
    query: &str,
    detach: bool,
    volume: u8,
    video: bool,
) -> Result<()> {
    ensure!(volume <= 100, "Volume must be between 0 and 100");
    let mode = if video {
        format!("Video ≤ {}p", Config::load()?.video_height)
    } else {
        "Audio only".into()
    };
    let detail = format!(
        "{mode} · volume {volume}% · {}",
        if detach { "detached" } else { "attached" }
    );
    let mut flow = Flow::new("Headless playback", &detail);
    let tracks = resolve_target(target, query, &mut flow).await?;
    play_tracks(tracks, video, detach, volume, &mut flow).await
}

struct HeadlessChild {
    child: tokio::process::Child,
    stop_on_drop: bool,
}
impl std::ops::Deref for HeadlessChild {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl std::ops::DerefMut for HeadlessChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}
impl Drop for HeadlessChild {
    fn drop(&mut self) {
        if self.stop_on_drop {
            let _ = self.child.start_kill();
        }
    }
}

async fn play_tracks(
    tracks: Vec<Track>,
    video: bool,
    detach: bool,
    volume: u8,
    flow: &mut Flow,
) -> Result<()> {
    ensure!(!tracks.is_empty(), "No playable tracks found");
    let config = Config::load()?;
    let (socket, state) = paths()?;
    ensure_socket_is_available(&socket).await?;
    let session = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    fs::write(
        &state,
        serde_json::to_vec(&State {
            tracks: tracks.clone(),
            session: session.clone(),
            video,
            video_height: config.video_height,
        })?,
    )
    .with_context(|| format!("Cannot save {}", state.display()))?;

    let mode = if video {
        format!("Video ≤ {}p", config.video_height)
    } else {
        "Audio only".into()
    };
    let source = flow
        .load(
            "Prepare playback",
            &format!("{} · {mode}", headless_ui::clean(&tracks[0].title)),
            resolve_source(&tracks[0].id, video, config.video_height),
        )
        .await?;
    let mut command = Command::new("mpv");
    command.as_std_mut().process_group(0);
    if let Some(audio) = source.audio {
        command.arg(format!("--audio-file={audio}"));
    }
    command
        .args([
            "--no-config",
            "--no-terminal",
            if video {
                "--force-window=yes"
            } else {
                "--no-video"
            },
            "--no-ytdl",
            "--audio-display=no",
            "--audio-client-name=Dymus headless",
        ])
        .arg(format!("--input-ipc-server={}", socket.display()))
        .arg(format!("--volume={volume}"))
        .arg(format!(
            "--force-media-title={}",
            crate::headless_service::media_tag(0)
        ))
        .arg("--")
        .arg(source.video)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = HeadlessChild {
        child: command
            .spawn()
            .context("Cannot start mpv; run `dymus doctor`")?,
        stop_on_drop: true,
    };
    flow.load(
        "Start playback",
        &format!("{mode} · volume {volume}% · buffering stream"),
        async {
            wait_for_socket(&socket, &mut child).await?;
            wait_for_playback(&socket, &mut child).await
        },
    )
    .await?;
    spawn_resolver(&session)?;
    flow.playing(
        &tracks[0].title,
        &tracks[0].artist,
        &format!(
            "{mode} · {} track{} · volume {volume}%",
            tracks.len(),
            if tracks.len() == 1 { "" } else { "s" }
        ),
        detach,
    );
    if detach {
        child.stop_on_drop = false;
        return Ok(());
    }
    if let Err(error) = display_progress(&socket, &tracks, video, volume).await {
        eprintln!("Progress display stopped: {error:#}");
    }
    let status = child.wait().await.context("Cannot wait for mpv")?;
    if !status.success() {
        bail!("mpv exited with {status}");
    }
    println!("  playback ended\n");
    Ok(())
}

#[derive(Default)]
struct ProgressState {
    position: Option<f64>,
    duration: Option<f64>,
    paused: bool,
    volume: Option<u8>,
    track_index: usize,
    tagged: bool,
}

impl ProgressState {
    fn event(&mut self, event: &Value) -> bool {
        if event["event"] == "start-file" {
            self.position = None;
            self.duration = None;
            return true;
        }
        if event["request_id"] == 100 && event["error"] == "success" {
            if let Some(duration) = valid_duration(event["data"].as_f64()) {
                self.duration = Some(duration);
            }
            return true;
        }
        if event["event"] != "property-change" {
            return false;
        }
        match event["name"].as_str() {
            Some("time-pos") => self.position = event["data"].as_f64(),
            Some("duration") => self.duration = valid_duration(event["data"].as_f64()),
            Some("volume") => {
                self.volume = event["data"]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(|value| value.clamp(0.0, 100.0).round() as u8)
            }
            Some("pause") => self.paused = event["data"].as_bool().unwrap_or(false),
            // Identity notifications can follow the initial duration snapshot.
            // Only start-file marks a new playback timeline.
            Some("media-title") => {
                if let Some(index) = event["data"]
                    .as_str()
                    .and_then(crate::headless_service::track_index)
                {
                    self.tagged = true;
                    self.track_index = index;
                }
            }
            Some("playlist-pos") if !self.tagged => {
                self.track_index = event["data"].as_u64().unwrap_or_default() as usize;
            }
            _ => return false,
        }
        true
    }

    fn duration(&self, tracks: &[Track]) -> Option<f64> {
        valid_duration(self.duration).or_else(|| {
            tracks
                .get(self.track_index)
                .map(crate::lastfm::duration_seconds)
                .and_then(|seconds| valid_duration(Some(seconds as f64)))
        })
    }
}

fn valid_duration(duration: Option<f64>) -> Option<f64> {
    duration.filter(|seconds| seconds.is_finite() && *seconds > 0.0)
}

fn duration_clock(duration: Option<f64>) -> String {
    valid_duration(duration)
        .map(|seconds| clock(Some(seconds)))
        .unwrap_or_else(|| "--:--".into())
}

/// Renders a single, continually updated line for attached headless playback.
async fn display_progress(
    socket_path: &Path,
    tracks: &[Track],
    video: bool,
    volume: u8,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        return Ok(());
    }
    let socket = UnixStream::connect(socket_path)
        .await
        .context("Cannot connect to mpv for progress updates")?;
    let (read, mut write) = socket.into_split();
    for (id, property) in [
        (1, "time-pos"),
        (2, "duration"),
        (3, "pause"),
        (4, "playlist-pos"),
        (5, "media-title"),
        (6, "volume"),
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
    let mut progress = ProgressState::default();
    let mut block = headless_ui::Block::default();
    let mut refresh = tokio::time::interval_at(
        Instant::now() + Duration::from_millis(100),
        Duration::from_secs(1),
    );
    let mut changed = false;
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("Cannot listen for playback stop")?;
                write.write_all(format!("{}\n", json!({"command":["quit"]})).as_bytes()).await?;
                break;
            }
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let event: Value = serde_json::from_str(&line)
                    .context("Invalid progress event from mpv")?;
                if event["event"] == "file-loaded" {
                    write.write_all(format!("{}\n", json!({"command":["get_property", "duration"], "request_id":100})).as_bytes()).await?;
                }
                if !progress.event(&event) { continue; }
                changed = true;
            }
            _ = refresh.tick(), if changed => {
                let track = tracks.get(progress.track_index);
                let view = headless_ui::PlaybackView {
                    title: track.map(|track| track.title.as_str()).unwrap_or("Unknown track"),
                    artist: track.map(|track| track.artist.as_str()).unwrap_or_default(),
                    position: progress.position,
                    duration: progress.duration(tracks),
                    paused: progress.paused,
                    video,
                    index: progress.track_index,
                    total: tracks.len(),
                    volume: progress.volume.unwrap_or(volume),
                };
                let (width, height) = terminal::size().unwrap_or((80, 24));
                if height < 10 {
                    let label = format!("{} — {}", view.title, view.artist);
                    let line = progress_line(&label, view.paused, view.position, view.duration, usize::from(width));
                    print!("\r\x1b[2K{}", headless_ui::fit(&line, usize::from(width).saturating_sub(1)));
                    io::stdout().flush()?;
                } else {
                    block.draw(&headless_ui::playback_rows(&view, headless_ui::width()))?;
                }
                changed = false;
            }
        }
    }
    println!();
    Ok(())
}

/// Resolves upcoming tracks after headless mpv has started, avoiding a long
/// up-front wait for albums and playlists.
pub async fn resolve_remaining(start: usize, session: Option<&str>) -> Result<()> {
    let (socket_path, state_path) = paths()?;
    let state: State = serde_json::from_slice(&fs::read(&state_path)?)
        .context("Headless playback queue is invalid")?;
    ensure!(
        session.is_none_or(|session| session == state.session),
        "Headless playback session has already ended"
    );
    let config = Config::load()?;
    let (closed_sender, mut closed) = tokio::sync::oneshot::channel();
    let service_socket = socket_path.clone();
    let tracks = state.tracks.clone();
    let service_config = config.clone();
    let service = tokio::spawn(async move {
        crate::headless_service::run(&service_socket, tracks, &service_config, closed_sender).await
    });
    let resolving = async {
        let mut socket = UnixStream::connect(&socket_path).await?;
        let commands = command(&mut socket, json!(["get_property", "command-list"])).await?;
        let has_index = loadfile_has_index(&commands);
        for (index, track) in state.tracks.iter().enumerate().skip(start) {
            let source = match resolve_source(&track.id, state.video, state.video_height).await {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("Could not resolve {}: {error:#}", track.title);
                    continue;
                }
            };
            command(
                &mut socket,
                append_command(
                    &source,
                    state.video,
                    has_index,
                    &crate::headless_service::media_tag(index),
                ),
            )
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        _ = &mut closed => {},
        result = resolving => if let Err(error) = result { eprintln!("Queue resolution ended: {error:#}"); },
    }
    service
        .await
        .context("Headless integration worker failed")?
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

pub(crate) async fn resolve_target(
    target: Target,
    query: &str,
    flow: &mut Flow,
) -> Result<Vec<Track>> {
    if matches!(target, Target::Song) {
        return Ok(vec![choose_youtube(query, None, flow).await?]);
    }
    let result_limit = Config::load()?.headless_results;
    let api = InnerTube::configured()?;
    match target {
        Target::Song => unreachable!("handled by YouTube search above"),
        Target::Album => collection_tracks(
            &api,
            query,
            SearchFilter::Albums,
            "album",
            result_limit,
            flow,
        )
        .await
        .map(|selection| selection.tracks),
        Target::Playlist => collection_tracks(
            &api,
            query,
            SearchFilter::Playlists,
            "playlist",
            result_limit,
            flow,
        )
        .await
        .map(|selection| selection.tracks),
        Target::Library(kind) => library_item_tracks(&api, kind, result_limit, flow).await,
    }
}

async fn library_item_tracks(
    api: &InnerTube,
    kind: LibraryKind,
    result_limit: usize,
    flow: &mut Flow,
) -> Result<Vec<Track>> {
    let label = library_label(kind);
    let items = flow
        .load(
            &format!("Load library {label}"),
            "Your YouTube Music library",
            api.library(kind),
        )
        .await?
        .items
        .into_iter()
        .filter(|item| {
            item.track.is_some() || !item.browse_id.is_empty() || !item.playlist_id.is_empty()
        })
        .take(result_limit)
        .collect::<Vec<_>>();
    ensure!(!items.is_empty(), "No playable library {label} found");
    let item = flow.choose(&format!("Choose from library {label}"), &items, |item| {
        if item.detail.is_empty() {
            headless_ui::clean(&item.title)
        } else {
            format!(
                "{}\n{}",
                headless_ui::clean(&item.title),
                headless_ui::clean(&item.detail)
            )
        }
    })?;
    load_collection_tracks(api, &item, flow).await
}

fn library_label(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Playlists => "playlists",
        LibraryKind::Albums => "albums",
        LibraryKind::Artists => "artists",
        LibraryKind::Podcasts => "podcasts",
    }
}

pub(crate) struct DownloadSelection {
    pub tracks: Vec<Track>,
    pub title: String,
    pub id: String,
}

pub(crate) async fn resolve_download_target(
    target: Target,
    query: &str,
    flow: &mut Flow,
) -> Result<DownloadSelection> {
    if matches!(target, Target::Song) {
        return Ok(DownloadSelection {
            tracks: vec![choose_youtube(query, None, flow).await?],
            title: "Singles".into(),
            id: "singles".into(),
        });
    }
    let api = InnerTube::configured()?;
    let (filter, label) = if matches!(target, Target::Album) {
        (SearchFilter::Albums, "album")
    } else {
        (SearchFilter::Playlists, "playlist")
    };
    collection_tracks(
        &api,
        query,
        filter,
        label,
        Config::load()?.headless_results,
        flow,
    )
    .await
}

async fn collection_tracks(
    api: &InnerTube,
    query: &str,
    filter: SearchFilter,
    label: &str,
    result_limit: usize,
    flow: &mut Flow,
) -> Result<DownloadSelection> {
    let items = flow
        .load(
            &format!("Search {label}s"),
            query,
            api.search(query, filter),
        )
        .await?
        .items
        .into_iter()
        .filter(|item| !item.browse_id.is_empty() || !item.playlist_id.is_empty())
        .take(result_limit)
        .collect::<Vec<_>>();
    ensure!(!items.is_empty(), "No playable {label} found");
    let item = flow.choose(&format!("Choose a {label}"), &items, |item| {
        if item.detail.is_empty() {
            headless_ui::clean(&item.title)
        } else {
            format!(
                "{}\n{}",
                headless_ui::clean(&item.title),
                headless_ui::clean(&item.detail)
            )
        }
    })?;
    let tracks = load_collection_tracks(api, &item, flow).await?;
    Ok(DownloadSelection {
        tracks,
        title: item.title,
        id: if item.playlist_id.is_empty() {
            item.browse_id
        } else {
            item.playlist_id
        },
    })
}

async fn load_collection_tracks(
    api: &InnerTube,
    item: &crate::innertube::LibraryItem,
    flow: &mut Flow,
) -> Result<Vec<Track>> {
    flow.load("Load all tracks", &item.title, async {
        let mut page = api.library_tracks(item).await?;
        let mut tracks = page.tracks;
        let mut seen = std::collections::HashSet::new();
        while let Some(token) = page.continuation {
            ensure!(
                seen.insert(token.clone()),
                "Collection pagination repeated a token"
            );
            page = api.library_tracks_more(&token).await?;
            tracks.extend(page.tracks);
        }
        Ok(tracks)
    })
    .await
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

fn spawn_resolver(session: &str) -> Result<()> {
    let executable = env::current_exe().context("Cannot locate the Dymus executable")?;
    let (_, state_path) = paths()?;
    let log_path = state_path.with_file_name("headless.log");
    let log = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(log_path)?
    };
    let mut command = Command::new(executable);
    command.as_std_mut().process_group(0);
    command
        .args(["__headless-resolve", "--start", "1", "--session", session])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(false)
        .spawn()
        .context("Cannot start headless playback integrations")?;
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
    if UnixStream::connect(path).await.is_ok() {
        bail!("A headless Dymus player is already running; use `dymus control`");
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

async fn wait_for_playback(path: &Path, child: &mut tokio::process::Child) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut socket = UnixStream::connect(path).await?;
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("mpv exited before playback started: {status}");
            }
            if let Ok(Ok(position)) = tokio::time::timeout(
                Duration::from_secs(2),
                command(&mut socket, json!(["get_property", "time-pos"])),
            )
            .await
                && position.as_f64().is_some()
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("Playback did not start within 30 seconds; try another result")?
}

async fn command(socket: &mut UnixStream, value: Value) -> Result<Value> {
    socket
        .write_all(format!("{}\n", json!({"command": value, "request_id": 1})).as_bytes())
        .await?;
    let mut lines = BufReader::new(socket).lines();
    loop {
        let line = lines
            .next_line()
            .await?
            .context("mpv closed its control socket")?;
        let response: Value = serde_json::from_str(&line).context("Invalid response from mpv")?;
        // Playback events can arrive before the reply even on a fresh connection.
        if response["request_id"] != 1 {
            continue;
        }
        ensure!(response["error"] == "success", "mpv: {}", response["error"]);
        return Ok(response["data"].clone());
    }
}

async fn print_status(socket: &mut UnixStream, state_path: &Path) -> Result<()> {
    let title = command(socket, json!(["get_property", "media-title"])).await?;
    let paused = command(socket, json!(["get_property", "pause"])).await?;
    let position = command(socket, json!(["get_property", "time-pos"])).await?;
    let duration = command(socket, json!(["get_property", "duration"])).await?;
    let index = command(socket, json!(["get_property", "playlist-pos"])).await?;
    let state = fs::read(state_path)
        .ok()
        .and_then(|body| serde_json::from_slice::<State>(&body).ok());
    let index = title
        .as_str()
        .and_then(crate::headless_service::track_index)
        .or_else(|| index.as_u64().map(|index| index as usize))
        .unwrap_or_default();
    let track = state
        .as_ref()
        .and_then(|state| state.tracks.get(index))
        .cloned();
    let duration = valid_duration(duration.as_f64()).or_else(|| {
        track
            .as_ref()
            .map(crate::lastfm::duration_seconds)
            .and_then(|seconds| valid_duration(Some(seconds as f64)))
    });
    if io::stdout().is_terminal() {
        let volume = command(socket, json!(["get_property", "volume"])).await?;
        let mut flow = Flow::new("Playback", "");
        flow.snapshot(&headless_ui::PlaybackView {
            title: track
                .as_ref()
                .map(|track| track.title.as_str())
                .unwrap_or_else(|| title.as_str().unwrap_or("Unknown track")),
            artist: track
                .as_ref()
                .map(|track| track.artist.as_str())
                .unwrap_or_default(),
            position: position.as_f64(),
            duration,
            paused: paused.as_bool().unwrap_or(false),
            video: state.as_ref().is_some_and(|state| state.video),
            index,
            total: state.as_ref().map(|state| state.tracks.len()).unwrap_or(1),
            volume: volume
                .as_f64()
                .unwrap_or_default()
                .clamp(0.0, 100.0)
                .round() as u8,
        })?;
        println!();
        return Ok(());
    }
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
        duration_clock(duration)
    );
    Ok(())
}

fn clock(seconds: Option<f64>) -> String {
    let seconds = seconds.unwrap_or_default().max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Leave the final column unused so printing never triggers terminal autowrap.
fn progress_line(
    label: &str,
    paused: bool,
    position: Option<f64>,
    duration: Option<f64>,
    width: usize,
) -> String {
    let available = width.saturating_sub(1);
    let status = if paused { "Ⅱ" } else { "▶" };
    let time = format!("· {} / {}", clock(position), duration_clock(duration));
    let fixed = format!("{status}  {time}");
    let label: String = label
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let label = truncate(
        &label,
        available.saturating_sub(UnicodeWidthStr::width(fixed.as_str())),
    );
    truncate(&format!("{status} {label} {time}"), available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_keeps_duration_when_identity_arrives_after_snapshot() {
        let mut progress = ProgressState::default();
        for event in [
            json!({"event":"property-change","name":"duration","data":125}),
            json!({"event":"property-change","name":"time-pos","data":52}),
            json!({"event":"property-change","name":"playlist-pos","data":0}),
            json!({"event":"property-change","name":"media-title","data":"dymus-track-0"}),
        ] {
            assert!(progress.event(&event));
        }
        assert_eq!(progress.duration(&[]), Some(125.0));
        assert_eq!(progress.position, Some(52.0));
        progress.event(&json!({"event":"start-file"}));
        assert_eq!(progress.duration(&[]), None);
        assert_eq!(progress.position, None);
        progress.event(&json!({"request_id":100,"error":"success","data":125}));
        assert_eq!(progress.duration(&[]), Some(125.0));
    }

    #[test]
    fn progress_falls_back_to_metadata_and_distinguishes_unknown_duration() {
        let mut track = crate::model::track("test");
        track.duration = "3:24".into();
        let mut progress = ProgressState::default();
        assert_eq!(progress.duration(std::slice::from_ref(&track)), Some(204.0));
        progress.duration = Some(220.0);
        assert_eq!(progress.duration(&[track]), Some(220.0));
        for duration in [None, Some(0.0), Some(-1.0), Some(f64::NAN)] {
            assert_eq!(duration_clock(duration), "--:--");
        }
    }

    #[test]
    fn persisted_playback_mode_defaults_to_audio_for_old_state() {
        let old: State = serde_json::from_value(json!({"tracks":[]})).unwrap();
        assert!(!old.video);
        assert_eq!(old.video_height, 1080);
        let state = State {
            tracks: Vec::new(),
            session: "test".into(),
            video: true,
            video_height: 720,
        };
        let saved: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(saved.video);
        assert_eq!(saved.video_height, 720);
    }

    #[test]
    fn queued_video_uses_its_own_audio_and_clears_combined_fallback() {
        let source = player::VideoSource {
            video: "https://example.com/video".into(),
            audio: Some("https://example.com/audio?x=a,b:c".into()),
        };
        let cmd = append_command(&source, true, true, "dymus-track-1");
        assert_eq!(cmd[0], "loadfile");
        assert_eq!(cmd[2], "append-play");
        assert_eq!(cmd[3], -1);
        assert_eq!(
            cmd[4]["audio-files"],
            r"https\://example.com/audio?x=a,b\:c"
        );
        let combined = player::VideoSource {
            video: source.video.clone(),
            audio: None,
        };
        assert_eq!(
            append_command(&combined, true, true, "dymus-track-1")[4]["audio-files"],
            ""
        );
        assert_eq!(
            append_command(&combined, false, false, "dymus-track-1"),
            json!(["loadfile", source.video, "append-play", {"force-media-title":"dymus-track-1"}])
        );
    }

    #[test]
    fn queued_video_supports_mpv_before_and_after_index_argument() {
        let old = json!([{"name":"loadfile", "args":[{"name":"url"},{"name":"flags"},{"name":"options"}]}]);
        let new = json!([{"name":"loadfile", "args":[{"name":"url"},{"name":"flags"},{"name":"index"},{"name":"options"}]}]);
        assert!(!loadfile_has_index(&old));
        assert!(loadfile_has_index(&new));
        let source = player::VideoSource {
            video: "video".into(),
            audio: None,
        };
        assert_eq!(
            append_command(&source, true, false, "dymus-track-1"),
            json!(["loadfile", "video", "append-play", {"audio-files":"", "force-media-title":"dymus-track-1"}])
        );
    }

    #[test]
    fn progress_stays_on_one_row_as_timestamps_grow() {
        let title = "A very long YouTube title with 日本語 and emoji 🎵 ".repeat(8);
        for width in [0, 1, 2, 10, 40, 80, 120] {
            for seconds in [9.0, 60.0, 600.0, 36000.0] {
                let line = progress_line(&title, false, Some(seconds), Some(40000.0), width);
                assert!(UnicodeWidthStr::width(line.as_str()) <= width.saturating_sub(1));
                assert!(!line.contains('\n'));
                if width >= 40 {
                    assert!(line.ends_with(&format!(
                        "· {} / {}",
                        clock(Some(seconds)),
                        clock(Some(40000.0))
                    )));
                }
            }
        }
    }

    #[test]
    fn progress_sanitizes_terminal_controls_and_preserves_short_titles() {
        let line = progress_line("Title\nArtist\r\t\x1b[2J", true, Some(5.0), Some(125.0), 80);
        assert!(!line.chars().any(char::is_control));
        assert_eq!(
            progress_line("Title — Artist", false, Some(5.0), Some(125.0), 80),
            "▶ Title — Artist · 0:05 / 2:05"
        );
    }
}
