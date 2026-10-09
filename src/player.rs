//! Stream extraction and audio playback live outside the terminal event loop.
use std::os::unix::process::CommandExt;
use std::{
    io::Write,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Command,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};

#[derive(Debug)]
pub enum Event {
    Loaded,
    Position(f64),
    Duration(f64),
    Paused(bool),
    Seeking,
    Ended,
    Notice(String),
    Error(String),
    AudioFrame(crate::visualizer::AudioFrame),
    AudioUnavailable(String),
}

pub struct Player {
    pub events: UnboundedReceiver<(u64, Event)>,
    sender: UnboundedSender<(u64, Event)>,
    commands: Option<UnboundedSender<PlayerCommand>>,
    task: Option<JoinHandle<()>>,
    load_task: Option<JoinHandle<()>>,
    preload_task: Option<JoinHandle<()>>,
    prepared: Arc<Mutex<PreloadCache>>,
    active_generation: Arc<AtomicU64>,
    pub local_video: bool,
    pub video: bool,
    pub video_height: u32,
    pub repeat_track: bool,
}

enum PlayerCommand {
    Control(Value),
    Load(LoadRequest),
}

#[derive(Clone, Copy, Default)]
struct Resume {
    position: f64,
    paused: bool,
}

struct LoadRequest {
    generation: u64,
    id: String,
    source: VideoSource,
    resume: Resume,
}

#[derive(Default)]
struct PreloadCache {
    revision: u64,
    requested: Option<(String, PlaybackOptions)>,
    source: Option<(VideoSource, Instant)>,
}

impl PreloadCache {
    fn take(&mut self, id: &str, options: PlaybackOptions) -> Option<VideoSource> {
        if self.requested.as_ref() != Some(&(id.to_owned(), options)) {
            return None;
        }
        self.requested = None;
        self.revision += 1;
        self.source
            .take()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(300))
            .map(|(source, _)| source)
    }
}

impl Player {
    pub fn new() -> Self {
        let (sender, events) = mpsc::unbounded_channel();
        Self {
            events,
            sender,
            commands: None,
            task: None,
            load_task: None,
            preload_task: None,
            prepared: Arc::new(Mutex::new(PreloadCache::default())),
            active_generation: Arc::new(AtomicU64::new(0)),
            local_video: false,
            video: false,
            video_height: 1080,
            repeat_track: false,
        }
    }

    pub fn play(&mut self, generation: u64, video_id: String, volume: u8) {
        self.play_from(generation, video_id, volume, 0.0, false);
    }

    fn options(&self) -> PlaybackOptions {
        PlaybackOptions {
            local_video: self.local_video,
            video: self.video,
            height: self.video_height,
            repeat_track: self.repeat_track,
        }
    }

    pub fn play_from(
        &mut self,
        generation: u64,
        video_id: String,
        volume: u8,
        position: f64,
        paused: bool,
    ) {
        let options = self.options();
        let prepared = self.prepared.lock().unwrap().take(&video_id, options);
        self.stop();
        self.active_generation = Arc::new(AtomicU64::new(generation));
        let active = self.active_generation.clone();
        let sender = self.sender.clone();
        let (commands, receiver) = mpsc::unbounded_channel();
        self.commands = Some(commands);
        self.task = Some(tokio::spawn(async move {
            let result = async {
                let source = prepared_source(&video_id, options, prepared).await?;
                playback(
                    LoadRequest {
                        generation,
                        id: video_id,
                        source,
                        resume: Resume { position, paused },
                    },
                    PlaybackConfig::new(volume, false, options),
                    &sender,
                    receiver,
                    active.clone(),
                )
                .await
            }
            .await;
            if let Err(error) = result {
                let _ = sender.send((
                    active.load(Ordering::Relaxed),
                    Event::Error(crate::headless_ui::error_message(&error)),
                ));
            }
        }));
    }

    /// Select the next track only after the UI has advanced its authoritative
    /// queue. Reuse mpv, but never let an outdated preloaded entry auto-play.
    pub fn advance(&mut self, generation: u64, id: String, volume: u8) {
        if self.task.as_ref().is_none_or(JoinHandle::is_finished) {
            self.play(generation, id, volume);
            return;
        }
        if let Some(task) = self.load_task.take() {
            task.abort();
        }
        let options = self.options();
        let prepared = self.prepared.lock().unwrap().take(&id, options);
        let Some(commands) = self.commands.clone() else {
            self.play(generation, id, volume);
            return;
        };
        self.active_generation.store(generation, Ordering::Relaxed);
        let sender = self.sender.clone();
        self.load_task = Some(tokio::spawn(async move {
            let result = async {
                let source = prepared_source(&id, options, prepared).await?;
                commands
                    .send(PlayerCommand::Load(LoadRequest {
                        generation,
                        id,
                        source,
                        resume: Resume::default(),
                    }))
                    .map_err(|_| anyhow::anyhow!("mpv stopped before the next track could load"))
            }
            .await;
            if let Err(error) = result {
                let _ = sender.send((
                    generation,
                    Event::Error(crate::headless_ui::error_message(&error)),
                ));
            }
        }));
    }

    /// Resolve one upcoming source privately; it is consumed only if its ID,
    /// playback mode and age still match when the user actually advances.
    pub fn preload(&mut self, generation: u64, video_id: String) {
        let options = self.options();
        let revision = {
            let mut cache = self.prepared.lock().unwrap();
            if cache.requested.as_ref() == Some(&(video_id.clone(), options))
                && (self
                    .preload_task
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
                    || cache
                        .source
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(300)))
            {
                return;
            }
            cache.revision += 1;
            cache.requested = Some((video_id.clone(), options));
            cache.source = None;
            cache.revision
        };
        if let Some(task) = self.preload_task.take() {
            task.abort();
        }
        if self.commands.is_none() {
            return;
        }
        let sender = self.sender.clone();
        let prepared = self.prepared.clone();
        self.preload_task = Some(tokio::spawn(async move {
            let result = resolve_playback(&video_id, options).await;
            let mut cache = prepared.lock().unwrap();
            if cache.revision != revision {
                return;
            }
            match result {
                Ok(source) => {
                    cache.source = Some((source, Instant::now()));
                }
                Err(error) => {
                    let _ = sender.send((
                        generation,
                        Event::Notice(format!(
                            "Next track will be resolved when needed: {}",
                            crate::headless_ui::error_message(&error)
                        )),
                    ));
                }
            }
        }));
    }

    pub fn clear_preloaded(&mut self) {
        if let Some(task) = self.preload_task.take() {
            task.abort();
        }
        let mut cache = self.prepared.lock().unwrap();
        cache.revision += 1;
        cache.requested = None;
        cache.source = None;
    }

    pub fn command(&self, command: Value) {
        if let Some(sender) = &self.commands {
            let _ = sender.send(PlayerCommand::Control(command));
        }
    }

    pub fn stop(&mut self) {
        self.commands = None;
        self.clear_preloaded();
        if let Some(task) = self.load_task.take() {
            task.abort();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    pub async fn shutdown(&mut self) {
        self.commands = None;
        let preload = self.preload_task.take();
        self.clear_preloaded();
        for task in [preload, self.load_task.take(), self.task.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

pub async fn resolve(video_id: &str) -> Result<String> {
    if let Some(path) = crate::local::path(video_id) {
        ensure!(
            path.is_absolute() && path.is_file(),
            "Local media is missing: {}",
            path.display()
        );
        return Ok(path.to_str().context("Local path is not UTF-8")?.to_owned());
    }
    if let Some(stream_url) = video_id.strip_prefix("radio:") {
        anyhow::ensure!(
            stream_url.starts_with("https://") || stream_url.starts_with("http://"),
            "Radio station has an invalid stream URL"
        );
        return Ok(stream_url.to_owned());
    }
    resolve_format(video_id, "bestaudio/best")
        .await?
        .into_iter()
        .next()
        .context("yt-dlp returned no playable stream")
}

#[derive(Clone)]
pub struct VideoSource {
    pub video: String,
    pub audio: Option<String>,
}

/// Prefer separate video within the height cap and audio; allow a combined fallback.
pub async fn resolve_video(video_id: &str, height: u32) -> Result<VideoSource> {
    if crate::local::path(video_id).is_some() || video_id.starts_with("radio:") {
        return Ok(VideoSource {
            video: resolve(video_id).await?,
            audio: None,
        });
    }
    let urls = resolve_format(video_id, &video_format(height)).await?;
    video_source(urls)
}

pub(crate) fn video_format(height: u32) -> String {
    format!("bestvideo[height<={height}]+bestaudio/best[height<={height}]")
}

fn video_source(urls: Vec<String>) -> Result<VideoSource> {
    anyhow::ensure!(
        (1..=2).contains(&urls.len()),
        "Expected one combined stream or a video/audio pair"
    );
    let mut urls = urls.into_iter();
    Ok(VideoSource {
        video: urls.next().context("yt-dlp returned no video stream")?,
        audio: urls.next(),
    })
}

async fn resolve_format(video_id: &str, format: &str) -> Result<Vec<String>> {
    resolve_format_using(video_id, format, std::path::Path::new("yt-dlp")).await
}

async fn resolve_format_using(
    video_id: &str,
    format: &str,
    program: &std::path::Path,
) -> Result<Vec<String>> {
    // yt-dlp needs the same authenticated browser session as InnerTube when
    // YouTube presents a bot check. Keep its Netscape jar private and alive
    // only for this child process; it is never a command-line argument itself.
    let cookies = crate::auth::load()?
        .map(|auth| youtube_cookie_jar(auth.cookie()))
        .transpose()?;
    let mut command = Command::new(program);
    command.as_std_mut().process_group(0);
    command.args([
        "--ignore-config",
        "--no-playlist",
        "--no-warnings",
        // YouTube can advertise signed URLs that reject playback with 403.
        // Probe selected formats so yt-dlp can skip them and try a fallback.
        "--check-formats",
        "--format",
        format,
        "--get-url",
    ]);
    if let Some(cookies) = &cookies {
        command.arg("--cookies").arg(cookies.path());
    }
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        command
            .arg("--")
            .arg(format!("https://www.youtube.com/watch?v={video_id}"))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Stream lookup timed out; try again")?
    .context("Cannot start yt-dlp; run `dymus doctor`")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!(
            "yt-dlp could not resolve this stream: {}. Run `dymus doctor`; if yt-dlp is installed, update it and try another track",
            error.trim()
        );
    }
    let text = String::from_utf8(output.stdout).context("Invalid stream URL")?;
    parse_stream_urls(&text)
}

fn parse_stream_urls(text: &str) -> Result<Vec<String>> {
    let urls: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("https://") || line.starts_with("http://"))
        .map(ToOwned::to_owned)
        .collect();
    anyhow::ensure!(!urls.is_empty(), "yt-dlp returned no playable stream");
    Ok(urls)
}

pub(crate) fn youtube_cookie_jar(cookie_header: &str) -> Result<tempfile::NamedTempFile> {
    let mut jar =
        tempfile::NamedTempFile::new().context("Cannot create private yt-dlp cookie jar")?;
    jar.write_all(b"# Netscape HTTP Cookie File\n")?;
    let mut count = 0;
    for part in cookie_header.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        ensure!(
            !name.is_empty() && !value.is_empty(),
            "Saved YouTube cookie is invalid"
        );
        ensure!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "Saved YouTube cookie has an invalid name"
        );
        writeln!(jar, ".youtube.com\tTRUE\t/\tTRUE\t0\t{name}\t{value}")?;
        count += 1;
    }
    ensure!(count > 0, "Saved YouTube cookie is empty");
    jar.as_file().sync_all()?;
    Ok(jar)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PlaybackOptions {
    local_video: bool,
    video: bool,
    height: u32,
    repeat_track: bool,
}

async fn prepared_source(
    id: &str,
    options: PlaybackOptions,
    prepared: Option<VideoSource>,
) -> Result<VideoSource> {
    if crate::local::path(id).is_some() {
        resolve(id).await?;
    }
    match prepared {
        Some(source) => Ok(source),
        None => resolve_playback(id, options).await,
    }
}

async fn resolve_playback(id: &str, options: PlaybackOptions) -> Result<VideoSource> {
    resolve_playback_using(id, options, std::path::Path::new("yt-dlp")).await
}

async fn resolve_playback_using(
    id: &str,
    options: PlaybackOptions,
    program: &std::path::Path,
) -> Result<VideoSource> {
    if crate::local::path(id).is_some() || id.starts_with("radio:") {
        return Ok(VideoSource {
            video: resolve(id).await?,
            audio: None,
        });
    }
    let format = if options.video {
        video_format(options.height)
    } else {
        "bestaudio/best".into()
    };
    video_source(resolve_format_using(id, &format, program).await?)
}

fn load_command(request: &LoadRequest, options: PlaybackOptions, has_index: bool) -> Value {
    // Audio and video options are scoped to each loaded file.
    let mut command =
        crate::headless::append_command(&request.source, true, has_index, "Dymus video");
    command[2] = json!("replace");
    let settings = command.as_array_mut().unwrap().last_mut().unwrap();
    let local_video =
        options.local_video && crate::local::path(&request.id).is_some_and(crate::local::is_video);
    settings["vid"] = json!(if options.video || local_video {
        "auto"
    } else {
        "no"
    });
    let position = if request.resume.position.is_finite() {
        request.resume.position.max(0.0)
    } else {
        0.0
    };
    settings["start"] = json!(position.to_string());
    settings["aid"] = json!("auto");
    settings["pause"] = json!(if request.resume.paused { "yes" } else { "no" });
    settings["loop-file"] = json!(if options.repeat_track { "inf" } else { "no" });
    command
}

fn has_external_audio(tracks: &Value) -> bool {
    tracks.as_array().is_some_and(|tracks| {
        tracks.iter().any(|track| {
            track["type"] == "audio" && track["external"] == true && track["selected"] == true
        })
    })
}

struct PlaybackConfig {
    volume: u8,
    silent: bool,
    options: PlaybackOptions,
    program: std::path::PathBuf,
    resolver: std::path::PathBuf,
    startup_timeout: Duration,
    load_timeout: Duration,
}

impl PlaybackConfig {
    fn new(volume: u8, silent: bool, options: PlaybackOptions) -> Self {
        Self {
            volume,
            silent,
            options,
            program: "mpv".into(),
            resolver: "yt-dlp".into(),
            startup_timeout: Duration::from_secs(5),
            load_timeout: Duration::from_secs(30),
        }
    }
}

async fn playback(
    mut current: LoadRequest,
    config: PlaybackConfig,
    sender: &UnboundedSender<(u64, Event)>,
    mut commands: UnboundedReceiver<PlayerCommand>,
    active_generation: Arc<AtomicU64>,
) -> Result<()> {
    let PlaybackConfig {
        volume,
        silent,
        mut options,
        program,
        resolver,
        startup_timeout,
        load_timeout,
    } = config;
    let directory = tempfile::Builder::new().prefix("dymus-").tempdir()?;
    let socket_path = directory.path().join("mpv.sock");
    let diagnostics = tempfile::NamedTempFile::new()?;
    let mut command = Command::new(program);
    command
        .args([
            "--no-config",
            "--idle=yes",
            "--terminal=yes",
            "--input-terminal=no",
            "--term-status-msg=",
            "--msg-level=all=warn",
            "--no-video",
            "--no-ytdl",
            "--audio-display=no",
            "--audio-client-name=Dymus",
            "--autofit=640x360",
            "--ontop=no",
            "--title=Dymus video",
            "--gapless-audio=yes",
        ])
        .arg(format!("--input-ipc-server={}", socket_path.display()))
        .arg(format!("--volume={volume}"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(diagnostics.as_file().try_clone()?))
        .stderr(Stdio::from(diagnostics.as_file().try_clone()?))
        .kill_on_drop(true);
    if silent {
        command.arg("--ao=null");
    }
    let mut child = command
        .spawn()
        .context("Cannot start mpv; install or repair mpv, then run `dymus doctor`")?;
    let process_id = child.id().context("mpv has no process ID")?;
    let result = async {
        let socket = tokio::time::timeout(startup_timeout, async {
            loop {
                if let Some(status) = child.try_wait()? { bail!("mpv exited during startup: {status}"); }
                if let Ok(socket) = UnixStream::connect(&socket_path).await { return Ok::<_, anyhow::Error>(socket); }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await.context("mpv did not open its control socket")??;
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        for (id, property) in [(1, "time-pos"), (2, "duration"), (3, "pause")] {
            write_command(&mut write, json!(["observe_property", id, property])).await?;
        }
        const CAPABILITIES: u64 = 900;
        write_request(&mut write, json!(["get_property", "command-list"]), Some(CAPABILITIES)).await?;
        let has_index = tokio::time::timeout(startup_timeout, async {
            loop {
                let line = lines.next_line().await?.context("mpv disconnected during startup")?;
                let message: Value = serde_json::from_str(&line).context("Invalid mpv startup event")?;
                if message["request_id"].as_u64() != Some(CAPABILITIES) { continue; }
                ensure!(message["error"] == "success" && message["data"].is_array(), "mpv did not provide its loadfile capabilities");
                return Ok::<_, anyhow::Error>(crate::headless::loadfile_has_index(&message["data"]));
            }
        }).await.context("mpv control negotiation timed out")??;
        let mut load_id = 1000;
        write_request(&mut write, load_command(&current, options, has_index), Some(load_id)).await?;
        let deadline = tokio::time::sleep(load_timeout);
        tokio::pin!(deadline);
        let mut loading = true;
        let mut started = false;
        let mut loaded = false;
        let mut ended = false;
        let mut retried = false;
        let mut capture = Box::pin(capture_audio(active_generation.clone(), sender.clone(), process_id));
        let mut capture_active = !silent;
        loop {
            tokio::select! {
                result = &mut capture, if capture_active => {
                    capture_active = false;
                    let message = result.err().unwrap_or_else(|| "PipeWire capture stopped".into());
                    let _ = sender.send((current.generation, Event::AudioUnavailable(message)));
                }
                _ = &mut deadline, if loading => bail!("Playback did not start within {} seconds", load_timeout.as_secs()),
                status = child.wait() => bail!("mpv exited during playback: {}", status?),
                command = commands.recv() => {
                    match command {
                        Some(PlayerCommand::Load(request)) => {
                            // A later skip/retry supersedes an already-resolved load.
                            if request.generation != active_generation.load(Ordering::Relaxed) { continue; }
                            current = request;
                            load_id += 1;
                            loading = true;
                            started = false;
                            loaded = false;
                            ended = false;
                            retried = false;
                            diagnostics.as_file().set_len(0)?;
                            {
                                use std::io::{Seek, SeekFrom};
                                diagnostics.as_file().try_clone()?.seek(SeekFrom::Start(0))?;
                            }
                            deadline.as_mut().reset(tokio::time::Instant::now() + load_timeout);
                            write_request(&mut write, load_command(&current, options, has_index), Some(load_id)).await?;
                        }
                        Some(PlayerCommand::Control(command)) => {
                            if command[0] == "set_property" && command[1] == "loop-file" {
                                options.repeat_track = command[2] == "inf";
                            }
                            write_command(&mut write, command).await?;
                        }
                        None => { child.kill().await?; return Ok(()); }
                    }
                }
                line = lines.next_line() => {
                    let line = line.context("Cannot read mpv events")?.context("mpv disconnected unexpectedly")?;
                    let message: Value = serde_json::from_str(&line).context("Invalid mpv event")?;
                    if message["request_id"].as_u64() == Some(load_id) && message["error"] != "success" {
                        bail!("mpv could not load the stream: {}", message["error"]);
                    }
                    let mut failure = None;
                    let mut report_loaded = false;
                    if started && !ended && message["event"] == "file-loaded" {
                        if current.source.audio.is_some() {
                            write_request(&mut write, json!(["get_property", "track-list"]), Some(load_id * 10 + 4)).await?;
                        } else {
                            report_loaded = true;
                        }
                    }
                    if message["request_id"].as_u64() == Some(load_id * 10 + 4) {
                        if message["error"] == "success" && has_external_audio(&message["data"]) {
                            report_loaded = true;
                        } else {
                            failure = Some("The video's separate audio stream could not be loaded".to_owned());
                        }
                    }
                    if report_loaded {
                        loaded = true;
                        loading = false;
                        let _ = sender.send((current.generation, Event::Loaded));
                        for (offset, property) in [(1, "time-pos"), (2, "duration"), (3, "pause")] {
                            write_request(&mut write, json!(["get_property", property]), Some(load_id * 10 + offset)).await?;
                        }
                        if !silent && !capture_active {
                            capture = Box::pin(capture_audio(active_generation.clone(), sender.clone(), process_id));
                            capture_active = true;
                        }
                    }
                    let event = match message["event"].as_str() {
                        Some("start-file") => { started = true; None }
                        Some("seek" | "playback-restart") if loaded && !ended => Some(Event::Seeking),
                        Some("property-change") if loaded && !ended => match message["name"].as_str() {
                            Some("time-pos") => message["data"].as_f64().filter(|value| value.is_finite() && *value >= 0.0).map(Event::Position),
                            Some("duration") => message["data"].as_f64().filter(|value| value.is_finite() && *value > 0.0).map(Event::Duration),
                            Some("pause") => message["data"].as_bool().map(Event::Paused),
                            _ => None,
                        },
                        Some("end-file") if started && !ended => match message["reason"].as_str() {
                            Some("eof") => {
                                ended = true;
                                loading = false;
                                if !loaded && current.source.audio.is_some() {
                                    failure = Some("Playback ended before the video's separate audio could be verified".to_owned());
                                    None
                                } else {
                                    Some(Event::Ended)
                                }
                            }
                            Some("error") => {
                                failure = Some(message["file_error"].as_str().unwrap_or("unknown playback error").to_owned());
                                None
                            }
                            Some("quit") => bail!("The mpv window was closed; retry playback to reopen it"),
                            Some("stop") => bail!("The mpv player stopped playback; retry the track or skip it"),
                            _ => None,
                        },
                        Some("shutdown") => bail!("The mpv player was closed; retry playback to reopen it"),
                        _ => None,
                    };
                    if let Some(error) = failure {
                        let details = diagnostic_text(diagnostics.path());
                        // Refresh a rejected signed URL once, including a missing
                        // external audio stream. Never hide permanent failures.
                        if !loaded && !retried && !current.id.is_empty()
                            && !current.id.starts_with("local:") && !current.id.starts_with("radio:")
                            && details.to_ascii_lowercase().contains("403") {
                            retried = true;
                            let _ = sender.send((current.generation, Event::Notice("YouTube rejected the stream; refreshing it once…".into())));
                            current.source = resolve_playback_using(&current.id, options, &resolver).await?;
                            load_id += 1;
                            started = false;
                            deadline.as_mut().reset(tokio::time::Instant::now() + load_timeout);
                            write_request(&mut write, load_command(&current, options, has_index), Some(load_id)).await?;
                            continue;
                        }
                        bail!("mpv could not play the stream: {error}");
                    }
                    if let Some(event) = event { let _ = sender.send((current.generation, event)); }
                    if loaded && !ended && message["error"] == "success" {
                        let event = match message["request_id"].as_u64().and_then(|id| id.checked_sub(load_id * 10)) {
                            Some(1) => message["data"].as_f64().filter(|value| value.is_finite() && *value >= 0.0).map(Event::Position),
                            Some(2) => message["data"].as_f64().filter(|value| value.is_finite() && *value > 0.0).map(Event::Duration),
                            Some(3) => message["data"].as_bool().map(Event::Paused),
                            _ => None,
                        };
                        if let Some(event) = event { let _ = sender.send((current.generation, event)); }
                    }
                    if !message["request_id"].as_u64().is_some_and(|id| (load_id * 10 + 1..=load_id * 10 + 4).contains(&id))
                        && message["request_id"].as_u64() != Some(load_id)
                        && let Some(error) = message["error"].as_str() && error != "success" {
                        let _ = sender.send((current.generation, Event::Notice(format!("mpv command failed: {error}"))));
                    }
                }
            }
        }
    }.await;
    result.map_err(|error| {
        let details = diagnostic_text(diagnostics.path());
        if details.is_empty() {
            error
        } else {
            anyhow::anyhow!("{error:#}\nmpv: {details}")
        }
    })
}

fn diagnostic_text(path: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    (|| -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        // Recent errors matter on long-lived players. Discard a partial first
        // line rather than risk exposing the tail of a truncated signed URL.
        let offset = file.metadata()?.len().saturating_sub(8192);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(8192).read_to_end(&mut bytes)?;
        if offset > 0 {
            bytes = bytes
                .splitn(2, |byte| *byte == b'\n')
                .nth(1)
                .unwrap_or_default()
                .to_vec();
        }
        Ok(String::from_utf8_lossy(&bytes)
            .split_whitespace()
            .map(|word| {
                if word.contains("https://") || word.contains("http://") {
                    "[stream URL]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" "))
    })()
    .unwrap_or_default()
}

async fn capture_audio(
    generation: Arc<AtomicU64>,
    sender: UnboundedSender<(u64, Event)>,
    process_id: u32,
) -> Result<(), String> {
    let node_id = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let output = tokio::process::Command::new("pw-dump")
                .arg("--no-colors")
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| format!("cannot run pw-dump: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "PipeWire session unavailable: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            if let Some(id) = crate::visualizer::find_dymus_node(&output.stdout, process_id) {
                return Ok::<_, String>(id);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "timed out waiting for the Dymus PipeWire stream".to_owned())??;

    let mut child = tokio::process::Command::new("pw-record")
        .args([
            "--raw",
            "--media-category",
            "Capture",
            "--target",
            &node_id,
            "--format",
            "f32",
            "--rate",
            "48000",
            "--channels",
            "2",
            "--latency",
            "20ms",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot start pw-record: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "pw-record has no audio stream".to_owned())?;
    let mut stdout = tokio::io::BufReader::new(stdout);
    const FRAMES: usize = 2048;
    let mut bytes = vec![0u8; FRAMES * 2 * std::mem::size_of::<f32>()];
    loop {
        use tokio::io::AsyncReadExt;
        stdout
            .read_exact(&mut bytes)
            .await
            .map_err(|error| format!("PipeWire audio stream ended: {error}"))?;
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        let frame = crate::visualizer::analyze(&samples, 2);
        if sender
            .send((generation.load(Ordering::Relaxed), Event::AudioFrame(frame)))
            .is_err()
        {
            return Ok(());
        }
    }
}

async fn write_command(write: &mut tokio::net::unix::OwnedWriteHalf, command: Value) -> Result<()> {
    write_request(write, command, None).await
}

async fn write_request(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    command: Value,
    request_id: Option<u64>,
) -> Result<()> {
    let mut message = json!({"command": command});
    if let Some(id) = request_id {
        message["request_id"] = json!(id);
    }
    let mut line = serde_json::to_vec(&message)?;
    line.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), write.write_all(&line))
        .await
        .context("mpv control socket timed out")?
        .context("Cannot send command to mpv")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    fn request(generation: u64, audio: Option<&str>) -> LoadRequest {
        LoadRequest {
            generation,
            id: String::new(),
            source: VideoSource {
                video: "https://example.com/video".into(),
                audio: audio.map(str::to_owned),
            },
            resume: Resume::default(),
        }
    }

    fn fake_player(mode: &str, has_index: bool) -> (tempfile::TempDir, PlaybackConfig) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("mpv");
        let script = r#"#!/usr/bin/python3
import json, os, socket, sys, time
mode = MODE
root = ROOT
indexed = INDEXED
with open(root + '/pid', 'w') as f: f.write(str(os.getpid()))
if mode == 'no_socket': time.sleep(10); sys.exit(0)
path = next(arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--input-ipc-server='))
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen(1)
connection, _ = server.accept()
stream = connection.makefile('r')
count = 0
settings = {}
pending_eof = False
def send(value): connection.sendall((json.dumps(value) + '\n').encode())
def eof(): send({'event': 'end-file', 'reason': 'eof'})
for line in stream:
    message = json.loads(line)
    assert 'request_id' not in message or type(message['request_id']) is int
    command = message['command']
    response = {'request_id': message.get('request_id'), 'error': 'success'}
    if command[0] == 'get_property':
        name = command[1]
        if name == 'command-list':
            send({'request_id': 1, 'error': 'success', 'data': []})
            if mode == 'no_capabilities': continue
            args = [{'name': 'url'}, {'name': 'flags'}]
            if indexed: args.append({'name': 'index'})
            args.append({'name': 'options'})
            response['data'] = [{'name': 'loadfile', 'args': args}]
        elif name == 'track-list':
            response['data'] = [] if mode == 'missing_audio' else [{'type': 'audio', 'external': True, 'selected': True}]
        elif name == 'duration': response['data'] = 1.0
        elif name == 'time-pos': response['data'] = float(settings.get('start', '0')) + 0.25
        elif name == 'pause': response['data'] = settings.get('pause') == 'yes'
        send(response)
        if name == 'track-list' and pending_eof: eof(); pending_eof = False
    elif command[0] == 'loadfile':
        count += 1
        settings = command[-1]
        with open(root + '/loads.jsonl', 'a') as f: f.write(json.dumps({'pid': os.getpid(), 'command': command}) + '\n')
        if mode == 'load_error': response['error'] = 'invalid parameter'; send(response); continue
        if mode == 'disconnect':
            print('Video output initialization failed', file=sys.stderr, flush=True)
            connection.close(); sys.exit(2)
        send(response)
        send({'event': 'start-file', 'playlist_entry_id': count})
        if mode == 'stall' or (mode == 'stall_second' and count == 2): continue
        if mode == 'reject' or (mode == 'reject_once' and count == 1):
            print('HTTP error 403 Forbidden https://example.com/video?token=private', file=sys.stderr, flush=True)
            send({'event': 'end-file', 'reason': 'error', 'file_error': 'loading failed'})
            continue
        send({'event': 'property-change', 'name': 'duration', 'data': 1.0})
        send({'event': 'file-loaded'})
        send({'event': 'property-change', 'name': 'duration', 'data': 1.0})
        send({'event': 'property-change', 'name': 'time-pos', 'data': 0.25})
        if mode in ('eof', 'stall_second'):
            if settings.get('audio-files'): pending_eof = True
            else: eof()
    else: send(response)
"#
            .replace("MODE", &serde_json::to_string(mode).unwrap())
            .replace("ROOT", &serde_json::to_string(root.path().to_str().unwrap()).unwrap())
            .replace("INDEXED", if has_index { "True" } else { "False" });
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = PlaybackConfig::new(0, true, PlaybackOptions::default());
        config.program = program;
        config.startup_timeout = Duration::from_secs(2);
        config.load_timeout = Duration::from_millis(300);
        (root, config)
    }

    async fn until_ended(events: &mut UnboundedReceiver<(u64, Event)>, expected: u64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some((generation, event)) = events.recv().await {
                assert_eq!(generation, expected);
                if matches!(event, Event::Ended) {
                    return;
                }
            }
            panic!("player stopped without EOF");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn consecutive_video_and_combined_tracks_reuse_mpv_and_ignore_stale_loads() {
        for indexed in [false, true] {
            let (root, mut config) = fake_player("eof", indexed);
            config.options.video = true;
            let (sender, mut events) = mpsc::unbounded_channel();
            let (commands, receiver) = mpsc::unbounded_channel();
            let active = Arc::new(AtomicU64::new(10));
            let task_active = active.clone();
            let task = tokio::spawn(async move {
                playback(
                    request(10, Some("https://example.com/audio?a=b,c:d")),
                    config,
                    &sender,
                    receiver,
                    task_active,
                )
                .await
            });
            until_ended(&mut events, 10).await;
            active.store(11, Ordering::Relaxed);
            commands
                .send(PlayerCommand::Load(request(
                    9,
                    Some("https://example.com/stale"),
                )))
                .unwrap();
            commands
                .send(PlayerCommand::Load(request(11, None)))
                .unwrap();
            until_ended(&mut events, 11).await;
            drop(commands);
            task.await.unwrap().unwrap();
            let loads: Vec<Value> = std::fs::read_to_string(root.path().join("loads.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(loads.len(), 2);
            assert_eq!(loads[0]["pid"], loads[1]["pid"]);
            assert_eq!(
                loads[0]["command"].as_array().unwrap().len(),
                if indexed { 5 } else { 4 }
            );
            assert_eq!(
                loads[0]["command"].as_array().unwrap().last().unwrap()["audio-files"],
                r"https\://example.com/audio?a=b,c\:d"
            );
            assert_eq!(
                loads[1]["command"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .get("audio-files"),
                None
            );
        }
    }

    #[tokio::test]
    async fn ipc_and_loading_failures_are_bounded_and_include_diagnostics() {
        for (mode, expected) in [
            ("no_socket", "control socket"),
            ("no_capabilities", "negotiation timed out"),
            ("stall", "Playback did not start"),
            ("load_error", "invalid parameter"),
            ("disconnect", "Video output initialization failed"),
            ("missing_audio", "separate audio stream"),
        ] {
            let (_root, mut config) = fake_player(mode, true);
            config.startup_timeout = Duration::from_millis(300);
            let (sender, _events) = mpsc::unbounded_channel();
            let (_commands, receiver) = mpsc::unbounded_channel();
            let error = tokio::time::timeout(
                Duration::from_secs(3),
                playback(
                    request(1, Some("https://example.com/audio")),
                    config,
                    &sender,
                    receiver,
                    Arc::new(AtomicU64::new(1)),
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{mode}: {error:#}");
        }
    }

    #[tokio::test]
    async fn every_queued_track_gets_a_new_loading_deadline() {
        let (_root, config) = fake_player("stall_second", true);
        let (sender, mut events) = mpsc::unbounded_channel();
        let (commands, receiver) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicU64::new(1));
        let task_active = active.clone();
        let task = tokio::spawn(async move {
            playback(request(1, None), config, &sender, receiver, task_active).await
        });
        until_ended(&mut events, 1).await;
        active.store(2, Ordering::Relaxed);
        commands
            .send(PlayerCommand::Load(request(2, None)))
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("Playback did not start"));
    }

    #[tokio::test]
    async fn rejected_youtube_streams_refresh_once_then_recover_or_fail() {
        use std::os::unix::fs::PermissionsExt;
        for mode in ["reject_once", "reject"] {
            let (root, mut config) = fake_player(mode, true);
            let resolver = root.path().join("yt-dlp");
            let count = root.path().join("resolutions");
            std::fs::write(&resolver, format!("#!/bin/sh\nprintf 'called\\n' >> '{}'\nprintf 'https://example.com/refreshed\\n'\n", count.display())).unwrap();
            std::fs::set_permissions(&resolver, std::fs::Permissions::from_mode(0o755)).unwrap();
            config.resolver = resolver;
            let (sender, mut events) = mpsc::unbounded_channel();
            let (commands, receiver) = mpsc::unbounded_channel();
            let mut initial = request(1, None);
            initial.id = "test-video".into();
            let task = tokio::spawn(async move {
                playback(
                    initial,
                    config,
                    &sender,
                    receiver,
                    Arc::new(AtomicU64::new(1)),
                )
                .await
            });
            if mode == "reject_once" {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while let Some((_, event)) = events.recv().await {
                        if matches!(event, Event::Loaded) {
                            return;
                        }
                    }
                    panic!("stream never loaded after refresh");
                })
                .await
                .unwrap();
                drop(commands);
                task.await.unwrap().unwrap();
            } else {
                let error = tokio::time::timeout(Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err();
                assert!(error.to_string().contains("403 Forbidden"));
                assert!(!error.to_string().contains("token=private"));
                drop(commands);
            }
            assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 1);
            assert_eq!(
                std::fs::read_to_string(root.path().join("loads.jsonl"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
    }

    #[test]
    fn per_file_options_preserve_resume_repeat_and_local_video_policy() {
        let mut initial = request(1, None);
        initial.id = "local:/tmp/video.mp4".into();
        initial.resume = Resume {
            position: 42.5,
            paused: true,
        };
        let cmd = load_command(
            &initial,
            PlaybackOptions {
                local_video: true,
                repeat_track: true,
                ..Default::default()
            },
            true,
        );
        assert_eq!(cmd[4]["start"], "42.5");
        assert_eq!(cmd[4]["pause"], "yes");
        assert_eq!(cmd[4]["loop-file"], "inf");
        assert_eq!(cmd[4]["vid"], "auto");
        assert_eq!(cmd[4]["aid"], "auto");
        initial.resume.position = f64::INFINITY;
        assert_eq!(
            load_command(&initial, PlaybackOptions::default(), true)[4]["start"],
            "0"
        );
        initial.id = "local:/tmp/audio.mp3".into();
        assert_eq!(
            load_command(
                &initial,
                PlaybackOptions {
                    local_video: true,
                    ..Default::default()
                },
                false
            )[3]["vid"],
            "no"
        );
        assert!(!has_external_audio(
            &json!([{"type":"audio", "selected":true, "external":false}])
        ));
        assert!(!has_external_audio(
            &json!([{"type":"audio", "selected":false, "external":true}])
        ));
    }

    #[tokio::test]
    async fn shutdown_aborts_all_pending_work_and_clears_sources() {
        let mut player = Player::new();
        player.task = Some(tokio::spawn(std::future::pending()));
        player.load_task = Some(tokio::spawn(std::future::pending()));
        player.preload_task = Some(tokio::spawn(std::future::pending()));
        player.prepared.lock().unwrap().source = Some((request(1, None).source, Instant::now()));
        player.shutdown().await;
        assert!(
            player.task.is_none() && player.load_task.is_none() && player.preload_task.is_none()
        );
        assert!(player.prepared.lock().unwrap().source.is_none());
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg, mpv and permission to open a local Unix socket"]
    async fn real_mpv_video_queue_preserves_pause_and_switches_external_to_combined_audio() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let video = root.path().join("video.mkv");
        let audio = root.path().join("audio.wav");
        let combined = root.path().join("combined.mkv");
        for (inputs, output) in [
            (
                vec![
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=blue:s=64x64:r=10",
                    "-c:v",
                    "ffv1",
                    "-an",
                ],
                &video,
            ),
            (
                vec![
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=r=8000:cl=mono",
                    "-c:a",
                    "pcm_s16le",
                ],
                &audio,
            ),
            (
                vec![
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=red:s=64x64:r=10",
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=r=8000:cl=mono",
                    "-c:v",
                    "ffv1",
                    "-c:a",
                    "pcm_s16le",
                ],
                &combined,
            ),
        ] {
            assert!(
                std::process::Command::new("ffmpeg")
                    .args(["-nostdin", "-loglevel", "error", "-y"])
                    .args(inputs)
                    .args(["-t", "0.5"])
                    .arg(output)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let wrapper = root.path().join("mpv-null-video");
        std::fs::write(&wrapper, "#!/bin/sh\nexec mpv --vo=null \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = PlaybackConfig::new(
            0,
            true,
            PlaybackOptions {
                video: true,
                ..Default::default()
            },
        );
        config.program = wrapper;
        let initial = LoadRequest {
            generation: 1,
            id: format!("local:{}", video.display()),
            source: VideoSource {
                video: video.to_string_lossy().into_owned(),
                audio: Some(audio.to_string_lossy().into_owned()),
            },
            resume: Resume {
                position: 0.1,
                paused: true,
            },
        };
        let (sender, mut events) = mpsc::unbounded_channel();
        let (commands, receiver) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicU64::new(1));
        let task_active = active.clone();
        let task =
            tokio::spawn(
                async move { playback(initial, config, &sender, receiver, task_active).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some((_, event)) = events.recv().await {
                if matches!(event, Event::Paused(true)) {
                    return;
                }
            }
            panic!("video did not load paused");
        })
        .await
        .unwrap();
        commands
            .send(PlayerCommand::Control(json!([
                "set_property",
                "pause",
                false
            ])))
            .unwrap();
        until_ended(&mut events, 1).await;
        for (generation, path) in [(2, &combined), (3, &audio)] {
            active.store(generation, Ordering::Relaxed);
            commands
                .send(PlayerCommand::Load(LoadRequest {
                    generation,
                    id: format!("local:{}", path.display()),
                    source: VideoSource {
                        video: path.to_string_lossy().into_owned(),
                        audio: None,
                    },
                    resume: Resume::default(),
                }))
                .unwrap();
            until_ended(&mut events, generation).await;
        }
        drop(commands);
        task.await.unwrap().unwrap();
    }

    #[test]
    fn preloads_are_bound_to_id_mode_and_age() {
        let options = PlaybackOptions::default();
        let mut cache = PreloadCache {
            revision: 1,
            requested: Some(("next".into(), options)),
            source: Some((request(1, None).source, Instant::now())),
        };
        assert!(cache.take("different", options).is_none());
        assert!(
            cache
                .take(
                    "next",
                    PlaybackOptions {
                        video: true,
                        ..options
                    }
                )
                .is_none()
        );
        assert!(cache.take("next", options).is_some());
        assert!(cache.take("next", options).is_none());
        cache.requested = Some(("next".into(), options));
        cache.source = Some((
            request(1, None).source,
            Instant::now() - Duration::from_secs(301),
        ));
        assert!(cache.take("next", options).is_none());
    }

    #[tokio::test]
    async fn queue_edits_cancel_obsolete_preloads_without_appending_to_mpv() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.wav");
        let second = root.path().join("second.wav");
        std::fs::write(&first, b"audio").unwrap();
        std::fs::write(&second, b"audio").unwrap();
        let mut player = Player::new();
        let (commands, mut receiver) = mpsc::unbounded_channel();
        player.commands = Some(commands);
        player.preload(1, format!("local:{}", first.display()));
        player.preload(1, format!("local:{}", second.display()));
        player.preload_task.take().unwrap().await.unwrap();
        assert_eq!(
            player
                .prepared
                .lock()
                .unwrap()
                .source
                .as_ref()
                .unwrap()
                .0
                .video,
            second.to_str().unwrap()
        );
        assert!(receiver.try_recv().is_err());
        player.clear_preloaded();
        assert!(player.prepared.lock().unwrap().source.is_none());
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn cached_local_media_is_revalidated_before_loading() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("deleted.wav");
        let prepared = VideoSource {
            video: path.to_string_lossy().into_owned(),
            audio: None,
        };
        assert!(
            prepared_source(
                &format!("local:{}", path.display()),
                PlaybackOptions::default(),
                Some(prepared)
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn preserves_separate_video_and_audio_urls() {
        let source = video_source(
            parse_stream_urls("https://example.com/video?x=1&y=2\nhttps://example.com/audio\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(source.video, "https://example.com/video?x=1&y=2");
        assert_eq!(source.audio.as_deref(), Some("https://example.com/audio"));
    }

    #[test]
    fn accepts_combined_stream_and_rejects_invalid_resolver_output() {
        let source =
            video_source(parse_stream_urls("https://example.com/combined\n").unwrap()).unwrap();
        assert!(source.audio.is_none());
        assert!(parse_stream_urls("\nnot a stream\n").is_err());
        assert!(video_source(Vec::new()).is_err());
        assert!(video_source(vec!["a".into(), "b".into(), "c".into()]).is_err());
    }

    #[test]
    fn writes_a_netscape_cookie_jar_for_youtube() {
        let mut jar = youtube_cookie_jar("SID=session; __Secure-3PAPISID=token").unwrap();
        let mut body = String::new();
        jar.seek(SeekFrom::Start(0)).unwrap();
        jar.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("# Netscape HTTP Cookie File\n"));
        assert!(body.contains(".youtube.com\tTRUE\t/\tTRUE\t0\tSID\tsession"));
        assert!(body.contains("__Secure-3PAPISID\ttoken"));
    }

    #[tokio::test]
    async fn local_resolution_is_direct_and_missing_files_fail_without_extraction() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("音楽 space.mp3");
        std::fs::write(&path, b"audio").unwrap();
        let id = format!("local:{}", path.display());
        assert_eq!(resolve(&id).await.unwrap(), path.to_str().unwrap());
        assert_eq!(
            resolve_video(&id, 1080).await.unwrap().video,
            path.to_str().unwrap()
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            resolve(&id)
                .await
                .unwrap_err()
                .to_string()
                .contains("Local media is missing")
        );
    }

    #[tokio::test]
    async fn radio_streams_skip_youtube_resolution() {
        let source = resolve("radio:https://stream.example/live").await.unwrap();
        assert_eq!(source, "https://stream.example/live");
        assert!(resolve("radio:not-a-url").await.is_err());
    }

    #[tokio::test]
    #[ignore = "requires YouTube network access, yt-dlp, mpv and a local Unix socket"]
    async fn live_search_resolve_and_stream_audio() {
        let songs = crate::innertube::InnerTube::new()
            .unwrap()
            .search("Nujabes Feather", crate::innertube::SearchFilter::Songs)
            .await
            .unwrap();
        let track = songs
            .tracks
            .first()
            .expect("live search should return songs");
        eprintln!("Streaming smoke test: {} — {}", track.title, track.artist);
        let source = resolve(&track.id).await.unwrap();
        let (sender, mut events) = mpsc::unbounded_channel();
        let (_commands, receiver) = mpsc::unbounded_channel();
        let source = VideoSource {
            video: source,
            audio: None,
        };
        let playback = playback(
            LoadRequest {
                generation: 1,
                id: track.id.clone(),
                source,
                resume: Resume::default(),
            },
            PlaybackConfig::new(0, true, PlaybackOptions::default()),
            &sender,
            receiver,
            Arc::new(AtomicU64::new(1)),
        );
        tokio::pin!(playback);
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                tokio::select! {
                    result = &mut playback => {
                        result.unwrap();
                        panic!("stream ended before reporting playback progress");
                    }
                    event = events.recv() => match event {
                        Some((_, Event::Position(seconds))) if seconds > 0.0 => break,
                        Some((_, Event::Error(error) | Event::Notice(error))) => panic!("{error}"),
                        _ => {}
                    }
                }
            }
        })
        .await
        .expect("stream should make progress");
        // Dropping the playback future stops mpv through kill_on_drop.
    }

    #[tokio::test]
    #[ignore = "requires mpv and permission to open a local Unix socket"]
    async fn mpv_reports_playback_and_eof_with_local_audio() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("silence.wav");
        // One second of mono, 8 kHz, 16-bit PCM. No audio hardware or network needed.
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend(16036_u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(16000_u32.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(16000_u32.to_le_bytes());
        wav.resize(16044, 0);
        std::fs::write(&path, wav).unwrap();
        let (sender, mut events) = mpsc::unbounded_channel();
        let (commands, receiver) = mpsc::unbounded_channel();
        let source = VideoSource {
            video: path.to_string_lossy().into_owned(),
            audio: None,
        };
        let task = tokio::spawn(async move {
            playback(
                LoadRequest {
                    generation: 7,
                    id: String::new(),
                    source,
                    resume: Resume::default(),
                },
                PlaybackConfig::new(0, true, PlaybackOptions::default()),
                &sender,
                receiver,
                Arc::new(AtomicU64::new(7)),
            )
            .await
        });
        let mut loaded = false;
        let mut duration = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some((generation, event)) = events.recv().await {
                assert_eq!(generation, 7);
                match event {
                    Event::Loaded => loaded = true,
                    Event::Duration(seconds) => duration = seconds > 0.0,
                    Event::Ended => break,
                    Event::Error(error) => panic!("{error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        drop(commands);
        task.await.unwrap().unwrap();
        assert!(loaded && duration);
    }
}
