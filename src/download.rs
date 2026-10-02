//! Headless downloads: yt-dlp owns extraction, resumable transfers and ffmpeg.
use crate::{
    auth,
    config::{Config, DownloadConfig},
    download_library::{DownloadContext, Recorder},
    headless::{self, Target},
    headless_ui::{self, Cancelled, Flow},
    player,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    env,
    io::{self, IsTerminal},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

pub async fn run(
    target: Target,
    query: &str,
    video: bool,
    override_path: Option<&Path>,
) -> Result<()> {
    ensure!(!query.trim().is_empty(), "Enter a download query or URL");
    let config = Config::load()?;
    let settings = &config.downloads;
    validate(settings)?;
    let destination = expand_path(override_path.unwrap_or_else(|| {
        Path::new(if video {
            &settings.video_path
        } else {
            &settings.audio_path
        })
    }))?;
    let collection = matches!(target, Target::Album | Target::Playlist);
    let direct = is_url(query);
    let mut flow = Flow::new(
        "Headless download",
        &format!(
            "{} · {}",
            if video { "Video" } else { "Audio" },
            destination.display()
        ),
    );
    flow.load("Check download tools", "yt-dlp · ffmpeg · ffprobe", async {
        for program in ["yt-dlp", "ffmpeg", "ffprobe"] {
            let flag = if program == "yt-dlp" {
                "--version"
            } else {
                "-version"
            };
            let status = Command::new(program)
                .arg(flag)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .with_context(|| format!("Cannot start {program}; install it to download media"))?;
            ensure!(status.success(), "{program} is not working");
        }
        Ok(())
    })
    .await?;
    let (jobs, collection_title, collection_id) = if direct {
        (
            vec![(query.to_owned(), None)],
            "Collection".into(),
            query.to_owned(),
        )
    } else {
        let selection = headless::resolve_download_target(target, query, &mut flow).await?;
        ensure!(!selection.tracks.is_empty(), "No downloadable tracks found");
        let jobs = selection
            .tracks
            .into_iter()
            .map(|track| {
                (
                    format!("https://www.youtube.com/watch?v={}", track.id),
                    Some(track),
                )
            })
            .collect();
        (jobs, selection.title, selection.id)
    };
    tokio::fs::create_dir_all(&destination)
        .await
        .with_context(|| format!("Cannot create {}", destination.display()))?;
    let destination = tokio::fs::canonicalize(&destination).await?;
    let cookies = auth::load()?
        .map(|auth| player::youtube_cookie_jar(auth.cookie()))
        .transpose()?;
    let mut recorder = Recorder::open(&destination, settings)?;
    let kind = if matches!(target, Target::Album) {
        "album"
    } else if collection {
        "playlist"
    } else {
        "singles"
    };
    let mut failed = 0;
    let mut saved = 0;
    for (index, (url, track)) in jobs.iter().enumerate() {
        let category = if settings.organize_by_type {
            match kind {
                "album" => "Albums",
                "playlist" => "Playlists",
                _ => "Singles",
            }
        } else {
            ""
        };
        let folder = if collection && settings.playlist_subdirectories {
            let name = if direct {
                "%(playlist_title|Collection)s [%(playlist_id|unknown)s]".into()
            } else {
                format!(
                    "{} [{}]",
                    folder_name(&collection_title).replace('%', "%%"),
                    folder_name(&collection_id).replace('%', "%%")
                )
            };
            Path::new(category).join(name)
        } else if !collection && settings.organize_by_type {
            Path::new(category).join("%(artist,uploader|Unknown Artist)s")
        } else {
            PathBuf::from(category)
        };
        let numbering = if collection && settings.number_tracks {
            if direct {
                "%(playlist_index)03d - ".into()
            } else {
                format!("{:03} - ", index + 1)
            }
        } else {
            String::new()
        };
        let template = folder.join(format!("{numbering}{}", settings.filename));
        if !io::stdout().is_terminal() {
            println!("Download {}/{}", index + 1, jobs.len());
        }
        let mut command = build_command(
            settings,
            video,
            settings.video_height.unwrap_or(config.video_height),
            &destination,
            &template,
            direct && collection,
        );
        if let Some(cookies) = &cookies {
            command.arg("--cookies").arg(cookies.path());
        }
        command.arg("--").arg(url);
        let context = DownloadContext {
            kind,
            video,
            title: collection_title.clone(),
            source_id: collection_id.clone(),
            source_url: direct.then(|| query.to_owned()),
            position: index + 1,
            fallback: track.clone(),
        };
        match transfer(
            command,
            settings.progress,
            &mut saved,
            &mut recorder,
            &context,
        )
        .await
        {
            Ok(()) => {}
            Err(error) if error.is::<Cancelled>() => {
                println!(
                    "Completed files are kept; rerun this command to resume partial downloads."
                );
                recorder.finish(false)?;
                return Err(error);
            }
            Err(error) => {
                failed += 1;
                eprintln!("Download failed: {error:#}");
                if !settings.continue_on_error {
                    recorder.finish(false)?;
                    return Err(error);
                }
            }
        }
    }
    recorder.finish(failed == 0)?;
    if io::stdout().is_terminal() {
        println!(
            "\n  saved {saved} file{}{}",
            if saved == 1 { "" } else { "s" },
            if failed > 0 {
                format!(" · {failed} failed")
            } else {
                String::new()
            }
        );
        println!("  {}\n", headless_ui::clean(&destination.to_string_lossy()));
    } else {
        println!(
            "\nDownload finished · {saved} file(s) saved · {failed} failed job(s)\nDestination: {}",
            destination.display()
        );
    }
    ensure!(
        failed == 0,
        "Some downloads failed; rerun to retry unfinished items"
    );
    Ok(())
}

fn is_url(query: &str) -> bool {
    query.starts_with("https://") || query.starts_with("http://")
}

fn validate(config: &DownloadConfig) -> Result<()> {
    ensure!(
        [
            "best", "aac", "alac", "flac", "m4a", "mp3", "opus", "vorbis", "wav"
        ]
        .contains(&config.audio_format.as_str()),
        "Unsupported downloads.audio_format"
    );
    ensure!(
        ["mp4", "mkv", "webm"].contains(&config.video_format.as_str()),
        "downloads.video_format must be mp4, mkv, or webm"
    );
    ensure!(
        !config.filename.trim().is_empty()
            && !Path::new(&config.filename)
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::RootDir)),
        "downloads.filename must be a relative filename template without '..'"
    );
    ensure!(
        config.filename.contains("%(ext)s"),
        "downloads.filename must include %(ext)s"
    );
    ensure!(
        config.audio_quality.parse::<u8>().is_ok_and(|n| n <= 10)
            || config
                .audio_quality
                .strip_suffix(['K', 'k'])
                .is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 0)),
        "downloads.audio_quality must be 0–10 or a bitrate such as 320K"
    );
    ensure!(
        (1..=32).contains(&config.concurrent_fragments),
        "downloads.concurrent_fragments must be 1–32"
    );
    ensure!(
        config.socket_timeout > 0,
        "downloads.socket_timeout must be positive"
    );
    ensure!(
        config
            .video_height
            .is_none_or(|height| (144..=4320).contains(&height)),
        "downloads.video_height must be 144–4320"
    );
    Ok(())
}

pub(crate) fn expand_path(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.as_os_str().is_empty(),
        "Download path cannot be empty"
    );
    let result = if path == Path::new("~") || path.starts_with("~/") {
        PathBuf::from(env::var_os("HOME").context("HOME is needed to expand the download path")?)
            .join(path.strip_prefix("~")?)
    } else {
        path.to_owned()
    };
    Ok(if result.is_absolute() {
        result
    } else {
        env::current_dir()?.join(result)
    })
}

fn folder_name(query: &str) -> String {
    let name: String = query
        .chars()
        .take(80)
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim_matches(['.', ' ']);
    if name.is_empty() {
        "Collection".into()
    } else {
        name.into()
    }
}

fn build_command(
    settings: &DownloadConfig,
    video: bool,
    height: u32,
    destination: &Path,
    template: &Path,
    playlist: bool,
) -> Command {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("yt-dlp");
    command.as_std_mut().process_group(0);
    command.args(["--ignore-config", "--no-mark-watched", "--no-color", "--newline", "--progress", "--progress-delta", "0.2", "--no-simulate", "--encoding", "utf-8", "--output-na-placeholder", "null"])
        .arg(if playlist { "--yes-playlist" } else { "--no-playlist" })
        .arg(if settings.resume { "--continue" } else { "--no-continue" })
        .arg(if settings.overwrite { "--force-overwrites" } else { "--no-overwrites" })
        .args(["--retries", &settings.retries.to_string(), "--fragment-retries", &settings.fragment_retries.to_string(), "--concurrent-fragments", &settings.concurrent_fragments.to_string(), "--socket-timeout", &settings.socket_timeout.to_string()])
        .arg("--paths").arg(destination)
        .arg("--output").arg(template)
        .args(["--print", "before_dl:dymus-item:{\"id\":%(id)j,\"title\":%(title)j,\"index\":%(playlist_index|0)j,\"total\":%(n_entries|0)j}", "--print", "after_move:dymus-file:{\"filepath\":%(filepath)j,\"id\":%(id)j,\"title\":%(title)j,\"artist\":%(artist)j,\"uploader\":%(uploader)j,\"album\":%(album)j,\"duration\":%(duration)j,\"webpage_url\":%(webpage_url)j,\"thumbnail\":%(thumbnail)j,\"playlist_title\":%(playlist_title)j,\"playlist_id\":%(playlist_id)j,\"playlist_index\":%(playlist_index)j}", "--progress-template", "download:dymus-progress:{\"id\":%(info.id)j,\"progress\":%(progress)j}", "--progress-template", "postprocess:dymus-postprocess:{\"id\":%(info.id)j,\"progress\":%(progress)j}"]);
    if settings.write_thumbnail {
        command.arg("--write-thumbnail");
    }
    if settings.embed_metadata {
        command.arg("--embed-metadata");
    }
    if settings.embed_thumbnail {
        command.arg("--embed-thumbnail");
    }
    if let Some(rate) = settings
        .rate_limit
        .as_ref()
        .filter(|rate| !rate.trim().is_empty())
    {
        command.arg("--limit-rate").arg(rate);
    }
    if settings.skip_downloaded && !settings.overwrite {
        command
            .arg("--download-archive")
            .arg(destination.join(if video {
                ".dymus-video-archive.txt"
            } else {
                ".dymus-audio-archive.txt"
            }));
    }
    if !settings.continue_on_error {
        command.arg("--abort-on-error");
    } else if playlist {
        command.arg("--ignore-errors");
    }
    if video {
        command
            .arg("--format")
            .arg(player::video_format(height))
            .arg("--merge-output-format")
            .arg(&settings.video_format)
            .arg("--remux-video")
            .arg(&settings.video_format);
    } else {
        command.args([
            "--format",
            "bestaudio/best",
            "--extract-audio",
            "--audio-format",
            &settings.audio_format,
            "--audio-quality",
            &settings.audio_quality,
        ]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

// yt-dlp can launch ffmpeg. Cancelling must also stop descendants in its group.
struct TransferChild(Child);
impl TransferChild {
    fn terminate(&mut self) {
        if let Some(pid) = self.0.id() {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", "--", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.0.start_kill();
        }
    }
}

impl Drop for TransferChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

async fn transfer(
    mut command: Command,
    show_progress: bool,
    saved: &mut usize,
    recorder: &mut Recorder,
    context: &DownloadContext,
) -> Result<()> {
    let mut child = TransferChild(command.spawn().context("Cannot start yt-dlp")?);
    let mut stdout =
        BufReader::new(child.0.stdout.take().context("Missing download output")?).lines();
    let mut stderr = BufReader::new(
        child
            .0
            .stderr
            .take()
            .context("Missing download diagnostics")?,
    )
    .lines();
    let mut output_open = true;
    let mut errors_open = true;
    let mut progress = Progress::new(show_progress);
    let mut errors = Vec::new();
    let status = loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal.context("Cannot listen for download cancellation")?; child.terminate(); let _ = child.0.wait().await; return Err(Cancelled.into()); }
            line = stdout.next_line(), if output_open => {
                match line? { Some(line) => consume(&line, &mut progress, saved, &mut errors, Some((recorder, context)))?, None => output_open = false }
            }
            line = stderr.next_line(), if errors_open => {
                match line? { Some(line) => consume(&line, &mut progress, saved, &mut errors, Some((recorder, context)))?, None => errors_open = false }
            }
            status = child.0.wait(), if !output_open && !errors_open => break status?,
        }
    };
    progress.clear()?;
    if !status.success() || !errors.is_empty() {
        bail!(
            "yt-dlp {}: {}",
            status,
            if errors.is_empty() {
                "download or conversion failed".into()
            } else {
                errors.join("\n")
            }
        );
    }
    Ok(())
}

fn consume(
    line: &str,
    progress: &mut Progress,
    saved: &mut usize,
    errors: &mut Vec<String>,
    record: Option<(&mut Recorder, &DownloadContext)>,
) -> Result<()> {
    if let Some(data) = line.strip_prefix("dymus-progress:") {
        if let Ok(value) = serde_json::from_str::<Value>(data)
            && progress.accepts(&value)
        {
            progress.draw(&progress_text(&value["progress"]))?;
        }
    } else if let Some(data) = line.strip_prefix("dymus-postprocess:") {
        if let Ok(value) = serde_json::from_str::<Value>(data)
            && progress.accepts(&value)
        {
            progress.draw("Processing · saving media")?;
        }
    } else if let Some(data) = line.strip_prefix("dymus-item:") {
        progress.clear()?;
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            progress.current_id = value["id"].as_str().map(str::to_owned);
            progress.completed = false;
            let title = headless_ui::clean(value["title"].as_str().unwrap_or("Media"));
            let index = value["index"].as_u64().unwrap_or_default();
            let total = value["total"].as_u64().unwrap_or_default();
            progress.title = title.clone();
            progress.subtitle = if index > 0 && total > 0 {
                format!("{index} / {total}")
            } else {
                String::new()
            };
            if progress.terminal && progress.enabled {
                progress.draw("Preparing download")?;
            } else {
                println!("  {title}");
            }
        }
    } else if let Some(data) = line.strip_prefix("dymus-file:") {
        progress.clear()?;
        let info: Value =
            serde_json::from_str(data).context("Invalid completed download metadata")?;
        let path = info["filepath"]
            .as_str()
            .context("Completed download has no path")?;
        if let Some((recorder, context)) = record {
            recorder.record(&info, context)?;
        }
        if !progress.terminal {
            println!("Saved: {}", headless_ui::clean(path));
        } else if !progress.enabled {
            println!("  saved · {}", headless_ui::clean(path));
        }
        progress.completed = true;
        *saved += 1;
    } else if line.starts_with("ERROR:") {
        progress.clear()?;
        if errors.len() < 20 {
            errors.push(headless_ui::clean(line));
        }
        eprintln!("{}", headless_ui::clean(line));
    } else if !line.trim().is_empty()
        && !line.starts_with("[download]")
        && (!progress.terminal || line.starts_with("WARNING:"))
    {
        progress.clear()?;
        println!(
            "{}",
            headless_ui::fit(
                &headless_ui::clean(line),
                headless_ui::width().saturating_sub(1)
            )
        );
    } else if line.contains("already") && !progress.terminal {
        progress.clear()?;
        println!("{}", headless_ui::clean(line));
    }
    Ok(())
}

fn progress_text(value: &Value) -> String {
    let downloaded = value["downloaded_bytes"].as_f64().unwrap_or(0.0);
    let total = value["total_bytes"]
        .as_f64()
        .or_else(|| value["total_bytes_estimate"].as_f64());
    let percent = total
        .filter(|total| *total > 0.0)
        .map(|total| format!("{:.1}%", (downloaded / total * 100.0).clamp(0.0, 100.0)))
        .unwrap_or_else(|| "Size unknown".into());
    let speed = value["speed"]
        .as_f64()
        .map(|speed| format!("{:.1} MiB/s", speed / 1_048_576.0))
        .unwrap_or_else(|| "Calculating speed".into());
    let eta = value["eta"]
        .as_u64()
        .map(|eta| format!("{}:{:02}", eta / 60, eta % 60))
        .unwrap_or_else(|| "--:--".into());
    format!(
        "Downloading · {percent} · {:.1} MiB · {speed} · ETA {eta} · Ctrl+C to cancel",
        downloaded / 1_048_576.0
    )
}

struct Progress {
    enabled: bool,
    terminal: bool,
    drawn: bool,
    last: Option<Instant>,
    processing: bool,
    current_id: Option<String>,
    completed: bool,
    block: headless_ui::Block,
    title: String,
    subtitle: String,
}
impl Progress {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            terminal: io::stdout().is_terminal(),
            drawn: false,
            last: None,
            processing: false,
            current_id: None,
            completed: false,
            block: headless_ui::Block::default(),
            title: "Download".into(),
            subtitle: String::new(),
        }
    }
    fn accepts(&self, event: &Value) -> bool {
        !self.completed
            && self
                .current_id
                .as_deref()
                .is_none_or(|id| event["id"].as_str() == Some(id))
    }
    fn draw(&mut self, text: &str) -> Result<()> {
        let processing = text.starts_with("Processing");
        if processing != self.processing {
            self.last = None;
            self.processing = processing;
        }
        if !self.enabled
            || self.last.is_some_and(|last| {
                last.elapsed() < Duration::from_millis(if self.terminal { 100 } else { 1000 })
            })
        {
            return Ok(());
        }
        self.last = Some(Instant::now());
        let text = headless_ui::clean(text);
        if self.terminal {
            self.block.draw(&[
                headless_ui::Row::title(&self.title),
                headless_ui::Row::muted(&self.subtitle),
                headless_ui::Row::blank(),
                headless_ui::Row::accent(text.trim_end_matches(" · Ctrl+C to cancel")),
                headless_ui::Row::blank(),
                headless_ui::Row::muted("ctrl+c cancel · completed files are kept"),
            ])?;
            self.drawn = true;
        } else {
            println!("{text}");
        }
        Ok(())
    }
    fn clear(&mut self) -> Result<()> {
        if self.drawn {
            self.block.clear()?;
            self.drawn = false;
        }
        self.last = None;
        Ok(())
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn commands_apply_formats_paths_and_independent_archives() {
        let settings = DownloadConfig {
            skip_downloaded: true,
            ..Default::default()
        };
        for video in [false, true] {
            let command = build_command(
                &settings,
                video,
                1080,
                Path::new("/tmp/Music space"),
                Path::new(&settings.filename),
                true,
            );
            let args: Vec<_> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(args.contains(&"/tmp/Music space".into()));
            assert!(args.contains(&if video {
                player::video_format(1080)
            } else {
                "bestaudio/best".into()
            }));
            assert!(args.iter().any(|arg| arg.ends_with(if video {
                ".dymus-video-archive.txt"
            } else {
                ".dymus-audio-archive.txt"
            })));
            assert!(args.contains(&"--no-simulate".into()));
        }
    }
    #[test]
    fn validates_templates_and_download_settings() {
        let mut settings = DownloadConfig::default();
        assert!(validate(&settings).is_ok());
        settings.filename = "../%(title)s.%(ext)s".into();
        assert!(validate(&settings).is_err());
        settings.filename = "%(title)s.%(ext)s".into();
        settings.audio_quality = "320K".into();
        assert!(validate(&settings).is_ok());
        settings.audio_format = "invalid".into();
        assert!(validate(&settings).is_err());
        assert_eq!(folder_name("../../Album/Name"), "_.._Album_Name");
        assert!(expand_path(Path::new("")).is_err());
    }
    #[test]
    fn delayed_progress_does_not_reopen_finished_or_previous_tracks() {
        let mut progress = Progress::new(false);
        progress.current_id = Some("second".into());
        assert!(!progress.accepts(&json!({"id":"first"})));
        assert!(progress.accepts(&json!({"id":"second"})));
        progress.completed = true;
        assert!(!progress.accepts(&json!({"id":"second"})));
    }
    #[test]
    fn progress_handles_estimates_unknown_sizes_and_terminal_controls() {
        assert!(
            progress_text(&json!({"downloaded_bytes": 50, "total_bytes_estimate": 100, "eta": 61}))
                .contains("50.0%")
        );
        assert!(progress_text(&json!({})).contains("Size unknown"));
        let mut progress = Progress::new(false);
        let mut saved = 0;
        let mut errors = Vec::new();
        consume(
            "ERROR: network failed",
            &mut progress,
            &mut saved,
            &mut errors,
            None,
        )
        .unwrap();
        assert_eq!(errors.len(), 1);
    }
}
