//! Standalone MPD client interface, styled to match the main Dymus TUI.

use crate::config::{Config, Palette};
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Layout, Rect, Size},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table, TableState, Wrap},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};
use std::{env, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
    task::JoinHandle,
};

#[derive(Clone, Default)]
struct QueueTrack {
    title: String,
    artist: String,
    album: String,
    duration: String,
    uri: String,
}

#[derive(Clone, Default)]
struct Status {
    state: String,
    title: String,
    artist: String,
    elapsed: f64,
    duration: f64,
    volume: String,
    song: Option<usize>,
    repeat: bool,
    random: bool,
    uri: Option<String>,
    playlist: Vec<QueueTrack>,
}

struct CoverArt {
    image: image::DynamicImage,
    protocol: Option<Protocol>,
    protocol_area: Option<Size>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    NowPlaying,
    Queue,
    Directories,
    Artists,
    AlbumArtists,
    Albums,
    Playlists,
    Search,
}

impl View {
    const ALL: [Self; 8] = [
        Self::NowPlaying,
        Self::Queue,
        Self::Directories,
        Self::Artists,
        Self::AlbumArtists,
        Self::Albums,
        Self::Playlists,
        Self::Search,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NowPlaying => "now playing",
            Self::Queue => "queue",
            Self::Directories => "directories",
            Self::Artists => "artists",
            Self::AlbumArtists => "album artists",
            Self::Albums => "albums",
            Self::Playlists => "playlists",
            Self::Search => "search",
        }
    }

    fn from_key(key: char) -> Option<Self> {
        key.to_digit(10)
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| Self::ALL.get(index as usize).copied())
    }
}

#[derive(Clone)]
enum EntryAction {
    Directory(String),
    Artist { tag: String, value: String },
    Album(Vec<(String, String)>),
    Track(String),
    Playlist(String),
}

#[derive(Clone)]
struct CatalogEntry {
    title: String,
    subtitle: String,
    action: EntryAction,
}

#[derive(Clone, Default)]
struct BrowsePage {
    title: String,
    entries: Vec<CatalogEntry>,
}

struct Browser {
    view: View,
    page: BrowsePage,
    parents: Vec<BrowsePage>,
    selected: usize,
    query: String,
    editing: bool,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            view: View::NowPlaying,
            page: BrowsePage {
                title: "now playing".into(),
                entries: Vec::new(),
            },
            parents: Vec::new(),
            selected: 0,
            query: String::new(),
            editing: false,
        }
    }
}

fn address() -> String {
    match (env::var("MPD_HOST").ok(), env::var("MPD_PORT").ok()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) if host.contains(':') => host,
        (Some(host), None) => format!("{host}:6600"),
        (_, Some(port)) => format!("localhost:{port}"),
        _ => "localhost:6600".into(),
    }
}

async fn mpd_command(command: &str) -> Result<Vec<String>> {
    let stream = TcpStream::connect(address())
        .await
        .with_context(|| format!("Could not connect to MPD at {}", address()))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if !line.starts_with("OK MPD ") {
        bail!("Unexpected MPD greeting: {}", line.trim());
    }
    write.write_all(format!("{command}\n").as_bytes()).await?;
    let mut lines = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            bail!("MPD closed the connection");
        }
        let line = line.trim_end().to_owned();
        if line == "OK" {
            return Ok(lines);
        }
        if line.starts_with("ACK ") {
            bail!("MPD rejected command: {line}");
        }
        lines.push(line);
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn filter_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
}

fn filter_expression(filters: &[(String, String)]) -> String {
    let expressions = filters
        .iter()
        .map(|(tag, value)| format!("({tag} == '{}')", filter_value(value)))
        .collect::<Vec<_>>();
    if expressions.len() == 1 {
        expressions[0].clone()
    } else {
        format!("({})", expressions.join(" AND "))
    }
}

fn songs_from_lines(lines: &[String]) -> Vec<QueueTrack> {
    let mut songs = Vec::new();
    let mut current: Option<QueueTrack> = None;
    for (key, value) in fields(lines) {
        if key == "file" {
            if let Some(song) = current.take() {
                songs.push(song);
            }
            current = Some(QueueTrack {
                title: display_name(&value),
                uri: value,
                ..QueueTrack::default()
            });
        } else if let Some(song) = current.as_mut() {
            match key.as_str() {
                "Title" => song.title = value,
                "Artist" | "AlbumArtist" => {
                    if song.artist.is_empty() || key == "Artist" {
                        song.artist = value;
                    }
                }
                "Album" => song.album = value,
                "duration" | "Time" => song.duration = format_time(seconds(&value)),
                _ => {}
            }
        }
    }
    if let Some(song) = current {
        songs.push(song);
    }
    songs
}

fn track_entries(songs: Vec<QueueTrack>) -> Vec<CatalogEntry> {
    songs
        .into_iter()
        .map(|song| CatalogEntry {
            title: song.title,
            subtitle: song.artist,
            action: EntryAction::Track(song.uri),
        })
        .collect()
}

async fn list_tag(tag: &str) -> Result<Vec<String>> {
    let lines = mpd_command(&format!("list {tag}")).await?;
    let mut values = fields(&lines)
        .into_iter()
        .filter(|(key, value)| key.eq_ignore_ascii_case(tag) && !value.is_empty())
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    values.sort_unstable_by_key(|value| value.to_lowercase());
    values.dedup();
    Ok(values)
}

async fn list_directory(uri: &str) -> Result<BrowsePage> {
    let command = if uri.is_empty() {
        "lsinfo".to_owned()
    } else {
        format!("lsinfo {}", quote(uri))
    };
    let lines = mpd_command(&command).await?;
    let mut page = BrowsePage {
        title: if uri.is_empty() {
            "directories".into()
        } else {
            uri.into()
        },
        entries: Vec::new(),
    };
    for (key, value) in fields(&lines) {
        match key.as_str() {
            "directory" => page.entries.push(CatalogEntry {
                title: value.rsplit('/').next().unwrap_or(&value).to_owned(),
                subtitle: "folder".into(),
                action: EntryAction::Directory(value),
            }),
            "file" => page.entries.push(CatalogEntry {
                title: display_name(&value),
                subtitle: "track".into(),
                action: EntryAction::Track(value),
            }),
            _ => {}
        }
    }
    page.entries.sort_by_key(|entry| {
        (
            matches!(entry.action, EntryAction::Track(_)),
            entry.title.to_lowercase(),
        )
    });
    Ok(page)
}

async fn find_songs(filters: &[(String, String)]) -> Result<Vec<QueueTrack>> {
    let expression = filter_expression(filters);
    let lines = mpd_command(&format!("find {} sort Artist", quote(&expression))).await?;
    Ok(songs_from_lines(&lines))
}

async fn albums_for_artist(tag: &str, artist: &str) -> Result<Vec<CatalogEntry>> {
    let filters = vec![(tag.to_owned(), artist.to_owned())];
    let songs = find_songs(&filters).await?;
    let mut albums = Vec::<CatalogEntry>::new();
    let mut seen = std::collections::BTreeSet::new();
    for song in songs {
        let album = song.album;
        if album.is_empty() || !seen.insert(album.to_lowercase()) {
            continue;
        }
        let mut album_filters = filters.clone();
        album_filters.push(("Album".into(), album.clone()));
        albums.push(CatalogEntry {
            title: album,
            subtitle: artist.into(),
            action: EntryAction::Album(album_filters),
        });
    }
    albums.sort_by_key(|entry| entry.title.to_lowercase());
    Ok(albums)
}

async fn load_view(view: View) -> Result<BrowsePage> {
    match view {
        View::NowPlaying => Ok(BrowsePage {
            title: view.label().into(),
            entries: Vec::new(),
        }),
        View::Queue => Ok(BrowsePage {
            title: view.label().into(),
            entries: Vec::new(),
        }),
        View::Directories => list_directory("").await,
        View::Artists | View::AlbumArtists => {
            let tag = if view == View::Artists {
                "Artist"
            } else {
                "AlbumArtist"
            };
            let values = list_tag(tag).await?;
            Ok(BrowsePage {
                title: view.label().into(),
                entries: values
                    .into_iter()
                    .map(|value| CatalogEntry {
                        title: value.clone(),
                        subtitle: tag.into(),
                        action: EntryAction::Artist {
                            tag: tag.into(),
                            value,
                        },
                    })
                    .collect(),
            })
        }
        View::Albums => {
            let albums = list_tag("Album").await?;
            Ok(BrowsePage {
                title: view.label().into(),
                entries: albums
                    .into_iter()
                    .map(|album| CatalogEntry {
                        title: album.clone(),
                        subtitle: "album".into(),
                        action: EntryAction::Album(vec![("Album".into(), album)]),
                    })
                    .collect(),
            })
        }
        View::Playlists => {
            let lines = mpd_command("listplaylists").await?;
            let entries = fields(&lines)
                .into_iter()
                .filter(|(key, _)| key == "playlist")
                .map(|(_, name)| CatalogEntry {
                    title: name.clone(),
                    subtitle: "playlist".into(),
                    action: EntryAction::Playlist(name),
                })
                .collect();
            Ok(BrowsePage {
                title: view.label().into(),
                entries,
            })
        }
        View::Search => Ok(BrowsePage {
            title: view.label().into(),
            entries: Vec::new(),
        }),
    }
}

async fn enter_entry(entry: &CatalogEntry) -> Result<BrowsePage> {
    match &entry.action {
        EntryAction::Directory(uri) => list_directory(uri).await,
        EntryAction::Artist { tag, value } => Ok(BrowsePage {
            title: value.clone(),
            entries: albums_for_artist(tag, value).await?,
        }),
        EntryAction::Album(filters) => Ok(BrowsePage {
            title: entry.title.clone(),
            entries: track_entries(find_songs(filters).await?),
        }),
        EntryAction::Playlist(name) => {
            let lines = mpd_command(&format!("listplaylistinfo {}", quote(name))).await?;
            Ok(BrowsePage {
                title: name.clone(),
                entries: track_entries(songs_from_lines(&lines)),
            })
        }
        EntryAction::Track(_) => Ok(BrowsePage::default()),
    }
}

async fn search_songs(query: &str) -> Result<Vec<CatalogEntry>> {
    let expression = format!("(any contains '{}')", filter_value(query));
    let lines = mpd_command(&format!("search {} sort Artist", quote(&expression))).await?;
    Ok(track_entries(songs_from_lines(&lines)))
}

async fn add_entry(entry: &CatalogEntry, play: bool) -> Result<()> {
    match &entry.action {
        EntryAction::Track(uri) => {
            if play {
                let lines = mpd_command(&format!("addid {}", quote(uri))).await?;
                let id = fields(&lines)
                    .into_iter()
                    .find(|(key, _)| key == "Id")
                    .map(|(_, value)| value)
                    .context("MPD did not return the added song ID")?;
                mpd_command(&format!("playid {id}")).await?;
            } else {
                mpd_command(&format!("add {}", quote(uri))).await?;
            }
        }
        EntryAction::Playlist(name) if !play => {
            mpd_command(&format!("load {}", quote(name))).await?;
        }
        EntryAction::Directory(uri) if !play => {
            mpd_command(&format!("add {}", quote(uri))).await?;
        }
        EntryAction::Album(filters) => {
            for song in find_songs(filters).await? {
                mpd_command(&format!("add {}", quote(&song.uri))).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn switch_view(browser: &mut Browser, view: View) -> Result<()> {
    browser.view = view;
    browser.parents.clear();
    browser.selected = 0;
    browser.query.clear();
    browser.page = load_view(view).await?;
    browser.editing = view == View::Search;
    Ok(())
}

async fn enter_browser_selection(browser: &mut Browser) -> Result<()> {
    let Some(entry) = browser.page.entries.get(browser.selected).cloned() else {
        return Ok(());
    };
    if matches!(entry.action, EntryAction::Track(_)) {
        return add_entry(&entry, true).await;
    }
    let next = enter_entry(&entry).await?;
    if !next.title.is_empty() {
        browser.parents.push(browser.page.clone());
        browser.page = next;
        browser.selected = 0;
    }
    Ok(())
}

async fn add_visible_entries(browser: &Browser) -> Result<()> {
    for entry in &browser.page.entries {
        if matches!(
            &entry.action,
            EntryAction::Track(_) | EntryAction::Directory(_) | EntryAction::Playlist(_)
        ) {
            add_entry(entry, false).await?;
        }
    }
    Ok(())
}

async fn artwork_chunk(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    command: &str,
) -> Result<Option<(usize, Vec<u8>)>> {
    writer.write_all(format!("{command}\n").as_bytes()).await?;
    let mut line = String::new();
    let mut total_size = None;
    let binary_size = loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            bail!("MPD closed the connection while sending artwork");
        }
        let trimmed = line.trim_end();
        if trimmed == "OK" {
            return Ok(None);
        }
        if trimmed.starts_with("ACK ") {
            bail!("MPD rejected artwork request: {trimmed}");
        }
        if let Some(value) = trimmed.strip_prefix("size: ") {
            total_size = Some(value.parse::<usize>()?);
        } else if let Some(value) = trimmed.strip_prefix("binary: ") {
            break value.parse::<usize>()?;
        }
    };
    let total_size = total_size.context("MPD artwork response has no size")?;
    if total_size > 24 * 1024 * 1024 {
        bail!("MPD artwork exceeds the 24 MiB limit");
    }
    if binary_size == 0 {
        bail!("MPD returned an empty artwork chunk");
    }
    let mut bytes = vec![0; binary_size];
    reader.read_exact(&mut bytes).await?;

    // Binary responses are followed by a newline and then the normal status line.
    line.clear();
    reader.read_line(&mut line).await?;
    if !line.trim().is_empty() {
        bail!("Malformed MPD artwork chunk terminator");
    }
    line.clear();
    reader.read_line(&mut line).await?;
    let completion = line.trim_end();
    if completion.starts_with("ACK ") {
        bail!("MPD rejected artwork response: {completion}");
    }
    if completion != "OK" {
        bail!("Unexpected MPD artwork response terminator: {completion}");
    }
    Ok(Some((total_size, bytes)))
}

async fn fetch_artwork(command: &str, uri: &str) -> Result<Option<Vec<u8>>> {
    let stream = TcpStream::connect(address())
        .await
        .with_context(|| format!("Could not connect to MPD at {}", address()))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut greeting = String::new();
    reader.read_line(&mut greeting).await?;
    if !greeting.starts_with("OK MPD ") {
        bail!("Unexpected MPD greeting: {}", greeting.trim());
    }

    let quoted_uri = uri.replace('\\', "\\\\").replace('"', "\\\"");
    let mut offset = 0usize;
    let mut artwork = Vec::new();
    let mut expected_size;
    loop {
        let request = format!("{command} \"{quoted_uri}\" {offset}");
        let Some((size, chunk)) = artwork_chunk(&mut reader, &mut write, &request).await? else {
            return Ok(None);
        };
        expected_size = size;
        offset = offset.saturating_add(chunk.len());
        if offset > size {
            bail!("MPD artwork chunk exceeds the reported image size");
        }
        artwork.extend_from_slice(&chunk);
        if offset >= size {
            break;
        }
    }
    if expected_size == artwork.len() {
        Ok(Some(artwork))
    } else {
        bail!("MPD artwork ended before the reported image size");
    }
}

fn music_directory() -> Option<PathBuf> {
    for variable in ["MPD_MUSIC_DIR", "MPD_MUSIC_DIRECTORY"] {
        if let Some(path) = env::var_os(variable) {
            return Some(PathBuf::from(path));
        }
    }
    let home = env::var_os("HOME").map(PathBuf::from)?;
    let config_path = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("mpd/mpd.conf");
    let config = std::fs::read_to_string(config_path).ok()?;
    let value = config.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        line.strip_prefix("music_directory")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })?;
    let value = value
        .trim_matches('"')
        .trim_matches('\'')
        .replace("${HOME}", &home.to_string_lossy())
        .replace("$HOME", &home.to_string_lossy());
    if let Some(path) = value.strip_prefix("~/") {
        Some(home.join(path))
    } else {
        Some(PathBuf::from(value))
    }
}

fn local_media_path(uri: &str) -> Option<PathBuf> {
    if uri.contains("://") {
        return None;
    }
    let path = PathBuf::from(uri);
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let path = music_directory()?.join(path);
    path.is_file().then_some(path)
}

async fn video_thumbnail(uri: &str, duration: f64) -> Option<image::DynamicImage> {
    let path = local_media_path(uri)?;
    let seek = (duration * 0.1).clamp(1.0, 30.0).to_string();
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-ss", &seek, "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-vf",
            "scale=512:512:force_original_aspect_ratio=decrease",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    image::load_from_memory(&output.stdout).ok()
}

async fn load_artwork(uri: &str, duration: f64) -> Option<image::DynamicImage> {
    for command in ["albumart", "readpicture"] {
        if let Ok(Some(bytes)) = fetch_artwork(command, uri).await
            && let Ok(image) = image::load_from_memory(&bytes)
        {
            return Some(image);
        }
    }
    video_thumbnail(uri, duration).await
}

fn fields(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|line| {
            line.split_once(": ")
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn display_name(file: &str) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file);
    stem.rsplit_once(" [")
        .map_or(
            stem,
            |(title, suffix)| {
                if suffix.ends_with(']') { title } else { stem }
            },
        )
        .to_owned()
}

fn seconds(value: &str) -> f64 {
    value.parse().unwrap_or(0.0)
}

async fn refresh() -> Result<Status> {
    let raw = mpd_command("status").await?;
    let mut status = Status::default();
    for (key, value) in fields(&raw) {
        match key.as_str() {
            "state" => status.state = value,
            "elapsed" => status.elapsed = seconds(&value),
            "duration" => status.duration = seconds(&value),
            "volume" => status.volume = value,
            "song" => status.song = value.parse().ok(),
            "repeat" => status.repeat = value == "1",
            "random" => status.random = value == "1",
            _ => {}
        }
    }

    let raw = mpd_command("currentsong").await?;
    for (key, value) in fields(&raw) {
        match key.as_str() {
            "Title" => status.title = value,
            "Artist" => status.artist = value,
            "file" => {
                status.uri = Some(value.clone());
                if status.title.is_empty() {
                    status.title = display_name(&value);
                }
            }
            _ => {}
        }
    }

    let raw = mpd_command("playlistinfo").await?;
    status.playlist = songs_from_lines(&raw);
    Ok(status)
}

pub async fn run() -> Result<()> {
    let config = Config::load()?;
    let mut terminal = ratatui::init();
    let picker = Picker::from_query_stdio_with_options(
        ratatui_image::picker::cap_parser::QueryStdioOptions {
            timeout: Duration::from_millis(150),
            ..Default::default()
        },
    )
    .unwrap_or_else(|_| Picker::halfblocks());
    let result = run_terminal(&mut terminal, config.palette(), picker).await;
    ratatui::restore();
    result
}

async fn run_terminal(
    terminal: &mut DefaultTerminal,
    palette: Palette,
    picker: Picker,
) -> Result<()> {
    let mut status = Status::default();
    let mut browser = Browser::default();
    let mut error = String::new();
    let mut queue_selected = 0usize;
    let mut show_help = false;
    let mut queue_state = TableState::default();
    let mut catalog_state = TableState::default();
    let mut cover_uri: Option<String> = None;
    let mut cover: Option<CoverArt> = None;
    let mut cover_task: Option<JoinHandle<Option<image::DynamicImage>>> = None;
    let mut redraw_tick = tokio::time::interval(Duration::from_millis(50));
    let mut refresh_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = redraw_tick.tick() => {},
            _ = refresh_tick.tick() => {
                match refresh().await {
                    Ok(next) => {
                        if next.uri != cover_uri {
                            if let Some(task) = cover_task.take() {
                                task.abort();
                            }
                            cover = None;
                            cover_uri = next.uri.clone();
                            if let Some(uri) = next.uri.clone() {
                                let duration = next.duration;
                                cover_task = Some(tokio::spawn(async move {
                                    load_artwork(&uri, duration).await
                                }));
                            }
                        }
                        status = next;
                        error.clear();
                        if !status.playlist.is_empty() {
                            queue_selected = queue_selected.min(status.playlist.len() - 1);
                            queue_state.select(Some(queue_selected));
                        } else {
                            queue_selected = 0;
                            queue_state.select(None);
                        }
                    }
                    Err(e) => error = format!("{e:#}"),
                }
            }
        }
        if cover_task.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(task) = cover_task.take()
                && let Ok(Some(image)) = task.await
            {
                cover = Some(CoverArt {
                    image,
                    protocol: None,
                    protocol_area: None,
                });
            }
        }
        terminal.draw(|frame| {
            draw(
                frame,
                &status,
                &error,
                show_help,
                &browser,
                &mut queue_state,
                &mut catalog_state,
                queue_selected,
                cover.as_mut(),
                cover_task.is_some(),
                &picker,
                palette,
            )
        })?;
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                if show_help {
                    if matches!(
                        key.code,
                        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                    ) {
                        show_help = false;
                    }
                    continue;
                }
                if browser.editing {
                    match key.code {
                        KeyCode::Esc => browser.editing = false,
                        KeyCode::Enter => match search_songs(&browser.query).await {
                            Ok(entries) => {
                                browser.page = BrowsePage {
                                    title: format!("results · {}", browser.query),
                                    entries,
                                };
                                browser.selected = 0;
                                browser.editing = false;
                            }
                            Err(e) => error = format!("{e:#}"),
                        },
                        KeyCode::Backspace => {
                            browser.query.pop();
                        }
                        KeyCode::Char(character) => browser.query.push(character),
                        _ => {}
                    }
                    continue;
                }

                if let KeyCode::Char(key @ '1'..='8') = key.code
                    && let Some(view) = View::from_key(key)
                {
                    if let Err(e) = switch_view(&mut browser, view).await {
                        error = format!("{e:#}");
                    }
                    continue;
                }

                let result = match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => {
                        if let Some(parent) = browser.parents.pop() {
                            browser.page = parent;
                            browser.selected = 0;
                        } else if browser.view != View::NowPlaying {
                            if let Err(e) = switch_view(&mut browser, View::NowPlaying).await {
                                error = format!("{e:#}");
                            }
                        }
                        Ok(())
                    }
                    KeyCode::Tab => {
                        let index = View::ALL
                            .iter()
                            .position(|view| *view == browser.view)
                            .unwrap_or(0);
                        if let Err(e) =
                            switch_view(&mut browser, View::ALL[(index + 1) % View::ALL.len()])
                                .await
                        {
                            error = format!("{e:#}");
                        }
                        Ok(())
                    }
                    KeyCode::BackTab => {
                        let index = View::ALL
                            .iter()
                            .position(|view| *view == browser.view)
                            .unwrap_or(0);
                        let previous = (index + View::ALL.len() - 1) % View::ALL.len();
                        if let Err(e) = switch_view(&mut browser, View::ALL[previous]).await {
                            error = format!("{e:#}");
                        }
                        Ok(())
                    }
                    KeyCode::Char('/') => {
                        if let Err(e) = switch_view(&mut browser, View::Search).await {
                            error = format!("{e:#}");
                        }
                        Ok(())
                    }
                    KeyCode::Char('?') => {
                        show_help = true;
                        Ok(())
                    }
                    KeyCode::Char(' ') => mpd_command("pause").await.map(|_| ()),
                    KeyCode::Char('n') => mpd_command("next").await.map(|_| ()),
                    KeyCode::Char('p') => mpd_command("previous").await.map(|_| ()),
                    KeyCode::Char('s') => mpd_command("stop").await.map(|_| ()),
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        mpd_command("volume +5").await.map(|_| ())
                    }
                    KeyCode::Char('-') => mpd_command("volume -5").await.map(|_| ()),
                    KeyCode::Char('r') => {
                        mpd_command(&format!("repeat {}", u8::from(!status.repeat)))
                            .await
                            .map(|_| ())
                    }
                    KeyCode::Char('z') => {
                        mpd_command(&format!("random {}", u8::from(!status.random)))
                            .await
                            .map(|_| ())
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        mpd_command("seekcur -5").await.map(|_| ())
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        mpd_command("seekcur +5").await.map(|_| ())
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if browser.view == View::Queue || browser.view == View::NowPlaying {
                            queue_selected = queue_selected.saturating_sub(1);
                            queue_state.select(Some(queue_selected));
                        } else {
                            browser.selected = browser.selected.saturating_sub(1);
                            catalog_state.select(Some(browser.selected));
                        }
                        Ok(())
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if browser.view == View::Queue || browser.view == View::NowPlaying {
                            if !status.playlist.is_empty() {
                                queue_selected =
                                    (queue_selected + 1).min(status.playlist.len() - 1);
                                queue_state.select(Some(queue_selected));
                            }
                        } else if !browser.page.entries.is_empty() {
                            browser.selected =
                                (browser.selected + 1).min(browser.page.entries.len() - 1);
                            catalog_state.select(Some(browser.selected));
                        }
                        Ok(())
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        if browser.view == View::Queue || browser.view == View::NowPlaying {
                            queue_selected = 0;
                            queue_state.select(Some(0));
                        } else {
                            browser.selected = 0;
                            catalog_state.select(Some(0));
                        }
                        Ok(())
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        if browser.view == View::Queue || browser.view == View::NowPlaying {
                            if !status.playlist.is_empty() {
                                queue_selected = status.playlist.len() - 1;
                                queue_state.select(Some(queue_selected));
                            }
                        } else if !browser.page.entries.is_empty() {
                            browser.selected = browser.page.entries.len() - 1;
                            catalog_state.select(Some(browser.selected));
                        }
                        Ok(())
                    }
                    KeyCode::Enter
                        if browser.view == View::Queue || browser.view == View::NowPlaying =>
                    {
                        if status.playlist.is_empty() {
                            Ok(())
                        } else {
                            mpd_command(&format!("play {queue_selected}"))
                                .await
                                .map(|_| ())
                        }
                    }
                    KeyCode::Enter => enter_browser_selection(&mut browser).await,
                    KeyCode::Char('a')
                        if browser.view != View::Queue && browser.view != View::NowPlaying =>
                    {
                        match browser.page.entries.get(browser.selected) {
                            Some(entry) => add_entry(entry, false).await,
                            None => Ok(()),
                        }
                    }
                    KeyCode::Char('A')
                        if browser.view != View::Queue && browser.view != View::NowPlaying =>
                    {
                        add_visible_entries(&browser).await
                    }
                    KeyCode::Char('d') if browser.view == View::Queue => {
                        if status.playlist.is_empty() {
                            Ok(())
                        } else {
                            mpd_command(&format!("delete {queue_selected}"))
                                .await
                                .map(|_| ())
                        }
                    }
                    _ => Ok(()),
                };
                if let Err(e) = result {
                    error = format!("{e:#}");
                }
            }
        }
    }
}

fn draw(
    frame: &mut Frame,
    status: &Status,
    error: &str,
    show_help: bool,
    browser: &Browser,
    table_state: &mut TableState,
    catalog_state: &mut TableState,
    queue_selected: usize,
    cover: Option<&mut CoverArt>,
    cover_loading: bool,
    picker: &Picker,
    palette: Palette,
) {
    let area = frame.area();
    let muted = Style::default().fg(palette.muted);
    let text = Style::default().fg(palette.text);
    let accent = Style::default().fg(palette.accent);
    frame.render_widget(Block::default().style(text), area);
    if area.width < 40 || area.height < 14 {
        frame.render_widget(
            Paragraph::new("Resize to 40 × 14\nq quit").style(muted),
            area,
        );
        return;
    }

    let inner = area.inner(ratatui::layout::Margin::new(2, 1));
    if show_help {
        help(frame, inner, palette);
        return;
    }

    let main_rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    navigation(frame, main_rows[0], browser.view, palette);
    let inner = main_rows[1];
    if browser.view == View::Queue {
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        draw_queue_table(
            frame,
            rows[0],
            &status.playlist,
            status.song,
            Some(queue_selected),
            table_state,
            rows[0].width,
            palette,
        );
        view_footer(frame, rows[1], browser, error, palette);
        return;
    }
    if browser.view != View::NowPlaying {
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        draw_catalog(frame, rows[0], browser, catalog_state, palette);
        view_footer(frame, rows[1], browser, error, palette);
        return;
    }

    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let panes = Layout::horizontal([
        Constraint::Percentage(44),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(rows[0]);
    let (play_symbol, play_color) = match status.state.as_str() {
        "play" => ("▶", palette.good),
        "pause" => ("Ⅱ", palette.warning),
        _ => ("■", palette.muted),
    };
    let title = if status.title.is_empty() {
        "Nothing playing"
    } else {
        status.title.as_str()
    };
    let left = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(panes[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{play_symbol}  "), Style::default().fg(play_color)),
            Span::styled(title, text.add_modifier(Modifier::BOLD)),
        ]))
        .wrap(Wrap { trim: true }),
        left[0],
    );
    let subtitle = if status.artist.is_empty() {
        status.song.map_or_else(
            || format!("{} tracks in queue", status.playlist.len()),
            |index| format!("track {} of {}", index + 1, status.playlist.len()),
        )
    } else {
        status.artist.clone()
    };
    frame.render_widget(
        Paragraph::new(subtitle)
            .style(Style::default().fg(palette.secondary))
            .wrap(Wrap { trim: true }),
        left[1],
    );
    render_cover(frame, cover, cover_loading, picker, left[2], palette);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("repeat", muted),
            Span::styled(if status.repeat { " on" } else { " off" }, text),
            Span::styled("   shuffle", muted),
            Span::styled(if status.random { " on" } else { " off" }, text),
        ])),
        left[3],
    );
    let ratio = if status.duration > 0.0 {
        (status.elapsed / status.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .use_unicode(true)
            .gauge_style(Style::default().fg(palette.progress_elapsed))
            .label(""),
        left[4],
    );
    let volume = if status.volume.is_empty() {
        "—".to_owned()
    } else {
        format!("{}%", status.volume)
    };
    let timing = if status.duration > 0.0 {
        format!(
            "{} / {}",
            format_time(status.elapsed),
            format_time(status.duration)
        )
    } else {
        "--:-- / --:--".into()
    };
    let time_volume =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(8)]).split(left[5]);
    frame.render_widget(Paragraph::new(timing).style(muted), time_volume[0]);
    frame.render_widget(
        Paragraph::new(format!("vol {volume}"))
            .alignment(Alignment::Right)
            .style(muted),
        time_volume[1],
    );

    let queue_rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(panes[2]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("queue", accent),
            Span::styled(format!("   {} tracks", status.playlist.len()), muted),
        ])),
        queue_rows[0],
    );
    let queue = status.playlist.iter().enumerate().map(|(index, track)| {
        let marker = if Some(index) == status.song {
            "▶"
        } else {
            " "
        };
        let artist = if track.artist.is_empty() {
            "—"
        } else {
            &track.artist
        };
        if panes[2].width >= 64 {
            Row::new(vec![
                Cell::from(format!("{marker} {:02}", index + 1)).style(muted),
                Cell::from(track.title.as_str()).style(text),
                Cell::from(artist).style(Style::default().fg(palette.secondary)),
                Cell::from(track.duration.as_str()).style(muted),
            ])
        } else {
            Row::new(vec![
                Cell::from(format!("{marker} {:02}", index + 1)).style(muted),
                Cell::from(if track.duration.is_empty() {
                    track.title.clone()
                } else {
                    format!("{} · {}", track.title, track.duration)
                })
                .style(text),
            ])
        }
    });
    let (columns, header) = if panes[2].width >= 64 {
        (
            vec![
                Constraint::Length(5),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(5),
            ],
            Row::new(["", "title", "artist", "time"]).style(muted.add_modifier(Modifier::DIM)),
        )
    } else {
        (
            vec![Constraint::Length(5), Constraint::Min(1)],
            Row::new(["", "title · time"]).style(muted.add_modifier(Modifier::DIM)),
        )
    };
    frame.render_stateful_widget(
        Table::new(queue, columns)
            .column_spacing(2)
            .header(header)
            .row_highlight_style(
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› "),
        queue_rows[1],
        table_state,
    );

    let footer = if !error.is_empty() {
        Line::from(Span::styled(error, Style::default().fg(palette.error)))
    } else if inner.width >= 110 {
        Line::from(
            vec![
                ("Space", "pause"),
                ("n/p", "track"),
                ("j/k", "queue"),
                ("Enter", "play"),
                ("h/l", "seek"),
                ("r/z", "repeat/shuffle"),
                ("+/-", "volume"),
                ("?", "help"),
                ("q", "quit"),
            ]
            .into_iter()
            .flat_map(|(key, label)| {
                [
                    Span::styled(key, accent.add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {label}  "), muted),
                ]
            })
            .collect::<Vec<_>>(),
        )
    } else {
        Line::from(
            vec![
                ("Space", "pause"),
                ("n/p", "skip"),
                ("j/k", "move"),
                ("Enter", "play"),
                ("+/-", "vol"),
                ("?", "help"),
                ("q", "quit"),
            ]
            .into_iter()
            .flat_map(|(key, label)| {
                [
                    Span::styled(key, accent.add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {label}  "), muted),
                ]
            })
            .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(Paragraph::new(footer), rows[1]);
}

fn navigation(frame: &mut Frame, area: Rect, active: View, palette: Palette) {
    let compact = area.width < 110;
    let labels = if compact {
        [
            "now", "queue", "dirs", "art", "al-art", "albums", "lists", "find",
        ]
    } else {
        [
            "now playing",
            "queue",
            "directories",
            "artists",
            "album artists",
            "albums",
            "playlists",
            "search",
        ]
    };
    let mut spans = Vec::new();
    for (index, (view, label)) in View::ALL.into_iter().zip(labels).enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(palette.muted)));
        }
        spans.push(Span::styled(
            format!("{}", index + 1),
            Style::default().fg(palette.accent),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default()
                .fg(if view == active {
                    palette.text
                } else {
                    palette.muted
                })
                .add_modifier(if view == active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_catalog(
    frame: &mut Frame,
    area: Rect,
    browser: &Browser,
    state: &mut TableState,
    palette: Palette,
) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    let title = if browser.view == View::Search && browser.editing {
        format!("search · {}▏", browser.query)
    } else {
        browser.page.title.clone()
    };
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(palette.accent)),
        rows[0],
    );
    if browser.page.entries.is_empty() {
        let message = if browser.view == View::Search && browser.editing {
            "Type a query, then press Enter"
        } else {
            "No items here"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(palette.muted)),
            rows[1],
        );
        return;
    }
    let entries = browser
        .page
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            Row::new(vec![
                Cell::from(format!("{:02}", index + 1)).style(Style::default().fg(palette.muted)),
                Cell::from(entry.title.as_str()).style(Style::default().fg(palette.text)),
                Cell::from(entry.subtitle.as_str()).style(Style::default().fg(palette.secondary)),
            ])
        });
    state.select(Some(browser.selected));
    frame.render_stateful_widget(
        Table::new(
            entries,
            [
                Constraint::Length(4),
                Constraint::Fill(3),
                Constraint::Fill(2),
            ],
        )
        .column_spacing(2)
        .row_highlight_style(
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› "),
        rows[1],
        state,
    );
}

fn draw_queue_table(
    frame: &mut Frame,
    area: Rect,
    tracks: &[QueueTrack],
    playing: Option<usize>,
    selected: Option<usize>,
    state: &mut TableState,
    width: u16,
    palette: Palette,
) {
    let muted = Style::default().fg(palette.muted);
    let text = Style::default().fg(palette.text);
    state.select(selected);
    let rows = tracks.iter().enumerate().map(|(index, track)| {
        let marker = if Some(index) == playing { "▶" } else { " " };
        if width >= 64 {
            Row::new(vec![
                Cell::from(format!("{marker} {:02}", index + 1)).style(muted),
                Cell::from(track.title.as_str()).style(text),
                Cell::from(if track.artist.is_empty() {
                    "—"
                } else {
                    &track.artist
                })
                .style(Style::default().fg(palette.secondary)),
                Cell::from(track.duration.as_str()).style(muted),
            ])
        } else {
            Row::new(vec![
                Cell::from(format!("{marker} {:02}", index + 1)).style(muted),
                Cell::from(if track.duration.is_empty() {
                    track.title.clone()
                } else {
                    format!("{} · {}", track.title, track.duration)
                })
                .style(text),
            ])
        }
    });
    let (columns, header) = if width >= 64 {
        (
            vec![
                Constraint::Length(5),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(5),
            ],
            Row::new(["", "title", "artist", "time"]).style(muted.add_modifier(Modifier::DIM)),
        )
    } else {
        (
            vec![Constraint::Length(5), Constraint::Min(1)],
            Row::new(["", "title · time"]).style(muted.add_modifier(Modifier::DIM)),
        )
    };
    frame.render_stateful_widget(
        Table::new(rows, columns)
            .column_spacing(2)
            .header(header)
            .row_highlight_style(
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› "),
        area,
        state,
    );
}

fn view_footer(frame: &mut Frame, area: Rect, browser: &Browser, error: &str, palette: Palette) {
    if !error.is_empty() {
        frame.render_widget(
            Paragraph::new(error).style(Style::default().fg(palette.error)),
            area,
        );
        return;
    }
    let muted = Style::default().fg(palette.muted);
    let accent = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let hints: &[(&str, &str)] = if browser.editing {
        &[("Enter", "search"), ("Esc", "cancel")]
    } else {
        match browser.view {
            View::Queue => &[("j/k", "move"), ("Enter", "play"), ("d", "remove")],
            _ => &[
                ("j/k", "move"),
                ("Enter", "open/play"),
                ("a/A", "queue"),
                ("Esc", "back"),
                ("/", "search"),
            ],
        }
    };
    let mut spans = Vec::new();
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", muted));
        }
        spans.push(Span::styled(*key, accent));
        spans.push(Span::styled(format!(" {label}"), muted));
    }
    spans.push(Span::styled("   q quit", muted));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_cover(
    frame: &mut Frame,
    cover: Option<&mut CoverArt>,
    loading: bool,
    picker: &Picker,
    area: Rect,
    palette: Palette,
) {
    if area.is_empty() {
        return;
    }
    if let Some(cover) = cover {
        let size = area.as_size();
        if cover.protocol_area != Some(size) {
            cover.protocol = picker
                .new_protocol(cover.image.clone(), size, Resize::Fit(None))
                .ok();
            cover.protocol_area = Some(size);
        }
        if let Some(protocol) = &cover.protocol {
            frame.render_widget(Image::new(protocol), area);
            return;
        }
    }
    frame.render_widget(
        Paragraph::new(if loading {
            "cover · loading…"
        } else {
            "cover unavailable"
        })
        .style(Style::default().fg(palette.muted)),
        area,
    );
}

fn help(frame: &mut Frame, area: Rect, palette: Palette) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new("MPD controls").style(
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        rows[0],
    );
    for (index, (key, description)) in [
        (
            "1–8 · Tab",
            "Switch between player, queue, library and search",
        ),
        ("j / k · ↑ / ↓", "Move through a list"),
        ("Enter", "Open a folder, artist or album; play a track"),
        ("a · A", "Add selected item or visible items to queue"),
        ("/", "Open search; type a query and press Enter"),
        ("Space · n / p", "Pause, next or previous track"),
        ("h / l · + / -", "Seek or adjust volume"),
        ("r · z · d", "Repeat, shuffle or remove from queue"),
    ]
    .iter()
    .enumerate()
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{key:<18}"), Style::default().fg(palette.accent)),
                Span::styled(*description, Style::default().fg(palette.secondary)),
            ])),
            rows[index + 1],
        );
    }
    frame.render_widget(
        Paragraph::new("? or Esc close help · q quit")
            .style(Style::default().fg(palette.muted))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::NONE)),
        rows[9],
    );
}

fn format_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
