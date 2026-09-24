//! Stream extraction and audio playback live outside the terminal event loop.
use std::{io::Write, process::Stdio, time::Duration};

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
    Ended,
    Notice(String),
    Error(String),
    AudioFrame(crate::visualizer::AudioFrame),
    AudioUnavailable(String),
}

pub struct Player {
    pub events: UnboundedReceiver<(u64, Event)>,
    sender: UnboundedSender<(u64, Event)>,
    commands: Option<UnboundedSender<Value>>,
    task: Option<JoinHandle<()>>,
    preload_task: Option<JoinHandle<()>>,
}

impl Player {
    pub fn new() -> Self {
        let (sender, events) = mpsc::unbounded_channel();
        Self {
            events,
            sender,
            commands: None,
            task: None,
            preload_task: None,
        }
    }

    pub fn play(&mut self, generation: u64, video_id: String, volume: u8) {
        self.stop();
        let sender = self.sender.clone();
        let (commands, receiver) = mpsc::unbounded_channel();
        self.commands = Some(commands);
        self.task = Some(tokio::spawn(async move {
            let result = async {
                let source = resolve(&video_id).await?;
                playback(&source, volume, false, generation, &sender, receiver).await
            }
            .await;
            if let Err(error) = result {
                let _ = sender.send((generation, Event::Error(format!("{error:#}"))));
            }
        }));
    }

    /// Resolve the following track while the current one is playing, then add
    /// it to mpv's playlist. Keeping one mpv process alive avoids the audible
    /// gap caused by launching a new resolver and audio device at EOF.
    pub fn preload(&mut self, generation: u64, video_id: String) {
        if let Some(task) = self.preload_task.take() {
            task.abort();
        }
        let Some(commands) = self.commands.clone() else {
            return;
        };
        let sender = self.sender.clone();
        self.preload_task = Some(tokio::spawn(async move {
            match resolve(&video_id).await {
                Ok(source) => {
                    let _ = commands.send(json!(["loadfile", source, "append"]));
                }
                Err(error) => {
                    let _ = sender.send((
                        generation,
                        Event::Notice(format!("Could not preload next track: {error:#}")),
                    ));
                }
            }
        }));
    }

    pub fn clear_preloaded(&mut self) {
        if let Some(task) = self.preload_task.take() {
            task.abort();
        }
        self.command(json!(["playlist-remove", 1]));
    }

    pub fn command(&self, command: Value) {
        if let Some(sender) = &self.commands {
            let _ = sender.send(command);
        }
    }

    pub fn stop(&mut self) {
        self.commands = None;
        if let Some(task) = self.preload_task.take() {
            task.abort();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    pub async fn shutdown(&mut self) {
        self.commands = None;
        if let Some(task) = self.task.take() {
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
    if let Some(stream_url) = video_id.strip_prefix("radio:") {
        anyhow::ensure!(
            stream_url.starts_with("https://") || stream_url.starts_with("http://"),
            "Radio station has an invalid stream URL"
        );
        return Ok(stream_url.to_owned());
    }
    // yt-dlp needs the same authenticated browser session as InnerTube when
    // YouTube presents a bot check. Keep its Netscape jar private and alive
    // only for this child process; it is never a command-line argument itself.
    let cookies = crate::auth::load()?
        .map(|auth| youtube_cookie_jar(auth.cookie()))
        .transpose()?;
    let mut command = Command::new("yt-dlp");
    command.args([
        "--ignore-config",
        "--no-playlist",
        "--no-warnings",
        "--format",
        "bestaudio/best",
        "--get-url",
    ]);
    if let Some(cookies) = &cookies {
        command.arg("--cookies").arg(cookies.path());
    }
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        command
            .arg("--")
            .arg(format!("https://music.youtube.com/watch?v={video_id}"))
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
    let url = text
        .lines()
        .find(|line| line.starts_with("https://") || line.starts_with("http://"))
        .context("yt-dlp returned no playable stream")?;
    Ok(url.to_owned())
}

fn youtube_cookie_jar(cookie_header: &str) -> Result<tempfile::NamedTempFile> {
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

async fn playback(
    source: &str,
    volume: u8,
    silent: bool,
    generation: u64,
    sender: &UnboundedSender<(u64, Event)>,
    mut commands: UnboundedReceiver<Value>,
) -> Result<()> {
    // A private directory prevents another user from controlling the IPC socket.
    let directory = tempfile::Builder::new().prefix("dymus-").tempdir()?;
    let socket_path = directory.path().join("mpv.sock");
    let mut command = Command::new("mpv");
    command
        .args([
            "--no-config",
            "--idle=yes",
            "--no-terminal",
            "--no-video",
            "--no-ytdl",
            "--audio-display=no",
            "--audio-client-name=Dymus",
        ])
        .arg(format!("--input-ipc-server={}", socket_path.display()))
        .arg(format!("--volume={volume}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if silent {
        command.arg("--ao=null");
    }
    let mut child = command
        .spawn()
        .context("Cannot start mpv; install or repair mpv, then run `dymus doctor`")?;
    let socket = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("mpv exited during startup: {status}");
            }
            if let Ok(socket) = UnixStream::connect(&socket_path).await {
                return Ok::<_, anyhow::Error>(socket);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("mpv did not open its control socket")??;
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read).lines();
    for (id, property) in [(1, "time-pos"), (2, "duration"), (3, "pause")] {
        write_command(&mut write, json!(["observe_property", id, property])).await?;
    }
    write_command(&mut write, json!(["loadfile", source, "replace"])).await?;
    // A stuck stream must not leave the player showing "Loading" forever.
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    let mut loaded = false;
    let mut capture = Box::pin(capture_audio(generation, sender.clone()));
    let mut capture_active = true;
    loop {
        tokio::select! {
            result = &mut capture, if capture_active => {
                capture_active = false;
                let message = result.err().unwrap_or_else(|| "PipeWire capture stopped".into());
                let _ = sender.send((generation, Event::AudioUnavailable(message)));
            }
            _ = &mut deadline, if !loaded => bail!("Audio stream did not start within 30 seconds"),
            command = commands.recv() => {
                match command {
                    Some(command) => write_command(&mut write, command).await?,
                    None => { child.kill().await?; return Ok(()); }
                }
            }
            line = lines.next_line() => {
                let Some(line) = line.context("Cannot read mpv events")? else { bail!("mpv disconnected unexpectedly"); };
                let message: Value = serde_json::from_str(&line).context("Invalid mpv event")?;
                let event = match message["event"].as_str() {
                    Some("file-loaded") => { loaded = true; Some(Event::Loaded) }
                    Some("property-change") => match message["name"].as_str() {
                        Some("time-pos") => message["data"].as_f64().map(Event::Position),
                        Some("duration") => message["data"].as_f64().map(Event::Duration),
                        Some("pause") => message["data"].as_bool().map(Event::Paused),
                        _ => None,
                    },
                    Some("end-file") => {
                        match message["reason"].as_str() {
                            Some("eof") => {
                                let _ = sender.send((generation, Event::Ended));
                                None
                            }
                            Some("error") => bail!("mpv could not play the stream: {}", message["file_error"].as_str().unwrap_or("unknown playback error")),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(event) = event { let _ = sender.send((generation, event)); }
                if let Some(error) = message["error"].as_str() && error != "success" {
                    let _ = sender.send((generation, Event::Notice(format!("mpv command failed: {error}"))));
                }
            }
        }
    }
}

async fn capture_audio(
    generation: u64,
    sender: UnboundedSender<(u64, Event)>,
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
            if let Some(id) = crate::visualizer::find_dymus_node(&output.stdout) {
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
        if sender.send((generation, Event::AudioFrame(frame))).is_err() {
            return Ok(());
        }
    }
}

async fn write_command(write: &mut tokio::net::unix::OwnedWriteHalf, command: Value) -> Result<()> {
    let mut line = serde_json::to_vec(&json!({"command": command}))?;
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
        let playback = playback(&source, 0, true, 1, &sender, receiver);
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
        let (_commands, receiver) = mpsc::unbounded_channel();
        tokio::time::timeout(
            Duration::from_secs(10),
            playback(path.to_str().unwrap(), 0, true, 7, &sender, receiver),
        )
        .await
        .unwrap()
        .unwrap();
        let mut loaded = false;
        let mut ended = false;
        let mut duration = false;
        while let Ok((generation, event)) = events.try_recv() {
            assert_eq!(generation, 7);
            match event {
                Event::Loaded => loaded = true,
                Event::Ended => ended = true,
                Event::Duration(seconds) => duration = seconds > 0.0,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(loaded && ended && duration);
    }
}
