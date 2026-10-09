//! One mpv event stream drives desktop controls, Last.fm, and playback history.
use crate::{
    auth,
    config::Config,
    innertube::InnerTube,
    lastfm::{self, Listening},
    model::Track,
    mpris::{LoopStatus, Mpris, MprisCommand, PlaybackStatus, Update},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
};

pub fn media_tag(index: usize) -> String {
    format!("dymus-track-{index}")
}
pub fn track_index(title: &str) -> Option<usize> {
    title.strip_prefix("dymus-track-")?.parse().ok()
}

#[derive(Debug)]
enum Report {
    NowPlaying {
        track: Track,
        duration: u64,
    },
    Scrobble {
        track: Track,
        duration: u64,
        timestamp: u64,
    },
    History(String),
}
#[derive(Default)]
struct Effects {
    updates: Vec<Update>,
    reports: Vec<Report>,
}

struct Playback {
    tracks: Vec<Track>,
    current: Option<usize>,
    loaded: bool,
    announced: bool,
    scrobbled: bool,
    history_reported: bool,
    listening: Listening,
    timestamp: u64,
    duration: f64,
    position: f64,
    paused: bool,
    seeking: bool,
    seek_notify: bool,
    stopped: bool,
    playlist_position: usize,
    playlist_count: usize,
    loop_track: bool,
    loop_queue: bool,
    shuffle: bool,
    lastfm: bool,
    history_after: Option<u64>,
}
impl Playback {
    fn new(tracks: Vec<Track>, lastfm: bool, history_after: Option<u64>) -> Self {
        Self {
            tracks,
            current: None,
            loaded: true,
            announced: false,
            scrobbled: false,
            history_reported: false,
            listening: Listening::default(),
            timestamp: 0,
            duration: 0.0,
            position: 0.0,
            paused: false,
            seeking: false,
            seek_notify: false,
            stopped: false,
            playlist_position: 0,
            playlist_count: 1,
            loop_track: false,
            loop_queue: false,
            shuffle: false,
            lastfm,
            history_after,
        }
    }
    fn track(&self) -> Option<&Track> {
        self.current.and_then(|index| self.tracks.get(index))
    }
    fn duration(&self) -> u64 {
        if self.duration > 0.0 {
            self.duration as u64
        } else {
            self.track()
                .map(lastfm::duration_seconds)
                .unwrap_or_default()
        }
    }
    fn status(&self) -> PlaybackStatus {
        if !self.loaded || self.current.is_none() || self.stopped {
            PlaybackStatus::Stopped
        } else if self.paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        }
    }
    fn announce(&mut self, epoch: u64, now: Instant, effects: &mut Effects) {
        if !self.loaded || self.announced || self.current.is_none() {
            return;
        }
        self.announced = true;
        self.timestamp = epoch;
        self.listening = Listening::default();
        self.listening.sample(self.position, now, false);
        let track = self.track().expect("current track checked");
        effects.updates.push(Update::Track {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            video_id: (!track.id.starts_with("radio:")).then(|| track.id.clone()),
            duration_secs: self.duration() as f64,
        });
        effects
            .updates
            .push(Update::CanSeek(!track.id.starts_with("radio:")));
        effects
            .updates
            .push(Update::CanGoPrevious(!track.id.starts_with("radio:")));
        effects.updates.push(Update::Status(self.status()));
        if self.lastfm && !track.id.starts_with("radio:") {
            effects.reports.push(Report::NowPlaying {
                track: track.clone(),
                duration: self.duration(),
            });
        }
    }
    fn event(&mut self, event: &Value, now: Instant, epoch: u64) -> Effects {
        let mut effects = Effects::default();
        match event["event"].as_str() {
            Some("start-file" | "end-file") => {
                self.current = None;
                self.loaded = false;
                self.announced = false;
                self.scrobbled = false;
                self.history_reported = false;
                self.listening = Listening::default();
                self.position = 0.0;
                self.duration = 0.0;
                self.seeking = false;
                self.seek_notify = false;
                self.stopped = false;
                effects
                    .updates
                    .push(Update::Status(PlaybackStatus::Stopped));
                effects.updates.push(Update::ClearTrack);
            }
            Some("file-loaded") => {
                self.loaded = true;
                self.announce(epoch, now, &mut effects);
            }
            Some("seek") => {
                self.seeking = true;
                self.seek_notify = true;
                self.listening.discontinuity();
            }
            Some("playback-restart") => {
                self.seeking = false;
                self.listening.discontinuity();
            }
            Some("property-change") => match event["name"].as_str() {
                Some("media-title") => {
                    if let Some(index) = event["data"].as_str().and_then(track_index)
                        && self.tracks.get(index).is_some()
                    {
                        self.current = Some(index);
                        self.announce(epoch, now, &mut effects);
                    }
                }
                Some("duration") => {
                    self.duration = event["data"]
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .unwrap_or(0.0)
                        .max(0.0);
                    effects.updates.push(Update::Duration {
                        seconds: self.duration() as f64,
                    });
                }
                Some("pause") => {
                    self.paused = event["data"].as_bool().unwrap_or(false);
                    if !self.paused {
                        self.stopped = false;
                    }
                    self.listening.discontinuity();
                    effects.updates.push(Update::Status(self.status()));
                }
                Some("time-pos") => {
                    if let Some(position) = event["data"].as_f64().filter(|value| value.is_finite())
                    {
                        self.position = position.max(0.0);
                        if self.seek_notify {
                            self.seek_notify = false;
                            effects.updates.push(Update::Seeked {
                                seconds: self.position,
                            });
                        }
                        self.listening.sample(
                            self.position,
                            now,
                            self.loaded && !self.paused && !self.seeking && !self.stopped,
                        );
                        effects.updates.push(Update::Position {
                            seconds: self.position,
                        });
                        if self.announced
                            && let Some(track) = self
                                .track()
                                .cloned()
                                .filter(|track| !track.id.starts_with("radio:"))
                        {
                            let duration = self.duration();
                            if self.lastfm
                                && !self.scrobbled
                                && lastfm::eligible_after(duration)
                                    .is_some_and(|after| self.listening.seconds >= after as f64)
                            {
                                self.scrobbled = true;
                                effects.reports.push(Report::Scrobble {
                                    track: track.clone(),
                                    duration,
                                    timestamp: self.timestamp,
                                });
                            }
                            if !self.history_reported
                                && crate::local::path(&track.id).is_none()
                                && self
                                    .history_after
                                    .is_some_and(|after| self.listening.seconds >= after as f64)
                            {
                                self.history_reported = true;
                                effects.reports.push(Report::History(track.id));
                            }
                        }
                    }
                }
                Some("volume") => {
                    if let Some(volume) = event["data"].as_f64() {
                        effects.updates.push(Update::Volume {
                            ratio: volume / 100.0,
                        });
                    }
                }
                Some("playlist-pos" | "playlist-count") => {
                    let value = event["data"].as_u64().unwrap_or(0) as usize;
                    if event["name"] == "playlist-pos" {
                        self.playlist_position = value;
                    } else {
                        self.playlist_count = value;
                    }
                    effects.updates.push(Update::CanGoNext(
                        self.playlist_position + 1 < self.playlist_count || self.loop_queue,
                    ));
                }
                Some("loop-file" | "loop-playlist") => {
                    let enabled = matches!(event["data"].as_str(), Some("inf" | "force"))
                        || event["data"].as_i64().is_some_and(|value| value > 0);
                    if event["name"] == "loop-file" {
                        self.loop_track = enabled;
                    } else {
                        self.loop_queue = enabled;
                    }
                    effects.updates.push(Update::LoopStatus(if self.loop_track {
                        LoopStatus::Track
                    } else if self.loop_queue {
                        LoopStatus::Playlist
                    } else {
                        LoopStatus::None
                    }));
                    effects.updates.push(Update::CanGoNext(
                        self.playlist_position + 1 < self.playlist_count || self.loop_queue,
                    ));
                }
                _ => {}
            },
            _ => {}
        }
        effects
    }
    fn commands(&mut self, command: MprisCommand) -> Vec<Value> {
        match command {
            MprisCommand::Play => {
                self.stopped = false;
                vec![json!(["set_property", "pause", false])]
            }
            MprisCommand::PlayPause => {
                self.stopped = false;
                vec![json!(["cycle", "pause"])]
            }
            MprisCommand::Pause => vec![json!(["set_property", "pause", true])],
            MprisCommand::Stop => {
                self.stopped = true;
                self.listening.discontinuity();
                vec![
                    json!(["set_property", "pause", true]),
                    json!(["seek", 0, "absolute+exact"]),
                ]
            }
            MprisCommand::Next => vec![json!(["playlist-next", "force"])],
            MprisCommand::Previous if self.playlist_position > 0 => {
                vec![json!(["playlist-prev", "force"])]
            }
            MprisCommand::Previous => vec![json!(["seek", 0, "absolute+exact"])],
            MprisCommand::Seek { offset_micros } => vec![json!([
                "seek",
                offset_micros as f64 / 1_000_000.0,
                "relative+exact"
            ])],
            MprisCommand::SetPosition { position_micros }
                if position_micros >= 0
                    && (self.duration() == 0
                        || position_micros as f64 / 1_000_000.0 <= self.duration() as f64) =>
            {
                vec![json!([
                    "seek",
                    position_micros as f64 / 1_000_000.0,
                    "absolute+exact"
                ])]
            }
            MprisCommand::SetPosition { .. } => Vec::new(),
            MprisCommand::SetVolume { volume } => vec![json!([
                "set_property",
                "volume",
                volume.clamp(0.0, 1.0) * 100.0
            ])],
            MprisCommand::SetLoopStatus(status) => vec![
                json!([
                    "set_property",
                    "loop-file",
                    if status == LoopStatus::Track {
                        "inf"
                    } else {
                        "no"
                    }
                ]),
                json!([
                    "set_property",
                    "loop-playlist",
                    if status == LoopStatus::Playlist {
                        "inf"
                    } else {
                        "no"
                    }
                ]),
            ],
            MprisCommand::SetShuffle(shuffle) => {
                self.shuffle = shuffle;
                vec![json!([if shuffle {
                    "playlist-shuffle"
                } else {
                    "playlist-unshuffle"
                }])]
            }
            MprisCommand::Quit => vec![json!(["quit"])],
        }
    }
}

async fn report_worker(
    mut reports: mpsc::Receiver<Report>,
    lastfm: Option<lastfm::Client>,
    history: Option<InnerTube>,
) {
    while let Some(report) = reports.recv().await {
        let result = match report {
            Report::NowPlaying { track, duration } => match &lastfm {
                Some(client) => client.now_playing(&track, duration).await,
                None => Ok(()),
            },
            Report::Scrobble {
                track,
                duration,
                timestamp,
            } => match &lastfm {
                Some(client) => client.scrobble(&track, duration, timestamp).await,
                None => Ok(()),
            },
            Report::History(id) => match &history {
                Some(api) => api.add_history_item(&id).await,
                None => Ok(()),
            },
        };
        if let Err(error) = result {
            eprintln!("Playback reporting failed: {error:#}");
        }
    }
}

pub async fn run(
    socket_path: &Path,
    tracks: Vec<Track>,
    config: &Config,
    closed: tokio::sync::oneshot::Sender<()>,
) -> Result<()> {
    let lastfm = if config.lastfm_scrobbling {
        auth::load_lastfm()
            .and_then(|auth| auth.map(lastfm::Client::new).transpose())
            .unwrap_or_else(|error| {
                eprintln!("Last.fm disabled for this session: {error:#}");
                None
            })
    } else {
        None
    };
    let history = if config.report_history {
        InnerTube::configured()
            .ok()
            .filter(InnerTube::is_authenticated)
    } else {
        None
    };
    let mut state = Playback::new(
        tracks,
        lastfm.is_some(),
        history
            .as_ref()
            .map(|_| config.report_history_after_seconds),
    );
    let socket = UnixStream::connect(socket_path)
        .await
        .context("Cannot observe headless playback")?;
    let (read, mut write) = socket.into_split();
    for (id, name) in [
        "media-title",
        "duration",
        "pause",
        "time-pos",
        "volume",
        "playlist-pos",
        "playlist-count",
        "loop-file",
        "loop-playlist",
    ]
    .iter()
    .enumerate()
    {
        write
            .write_all(
                format!("{}\n", json!({"command":["observe_property", id, name]})).as_bytes(),
            )
            .await?;
    }
    let mut mpris = Mpris::spawn_named("dymus.headless", "Dymus (headless)");
    let (reports, receiver) = mpsc::channel(32);
    let reporter = tokio::spawn(report_worker(receiver, lastfm, history));
    let mut commands_open = true;
    let mut lines = BufReader::new(read).lines();
    let result = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break; };
                    let mut event: Value = serde_json::from_str(&line)?;
                    // mpv may keep an observed title unchanged when returning to or
                    // repeating a file. Refresh the stable queue tag after each load.
                    if event["event"] == "file-loaded" {
                        write.write_all(b"{\"command\":[\"get_property\",\"media-title\"],\"request_id\":100}\n").await?;
                    }
                    if event["request_id"] == 100 && event["error"] == "success" {
                        event = json!({"event":"property-change", "name":"media-title", "data":event["data"]});
                    }
                    let effects = state.event(&event, Instant::now(), auth::unix_timestamp().unwrap_or(0));
                    for update in effects.updates { mpris.update(update); }
                    for report in effects.reports {
                        if let Err(error) = reports.try_send(report) { eprintln!("Playback report queue is full: {error}"); }
                    }
                }
                command = mpris.next_command(), if commands_open => {
                    let Some(command) = command else { commands_open = false; continue; };
                    for value in state.commands(command) {
                        write.write_all(format!("{}\n", json!({"command":value})).as_bytes()).await?;
                    }
                    mpris.update(Update::Status(state.status()));
                    mpris.update(Update::Shuffle(state.shuffle));
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }.await;
    let _ = closed.send(());
    mpris.shutdown();
    drop(reports);
    // Finish queued reports after releasing mpv and D-Bus. Each HTTP client
    // bounds individual requests, and this never blocks the playback process.
    reporter.await.context("Playback reporting worker failed")?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: id.into(),
            artist: "Artist".into(),
            album: String::new(),
            duration: "1:00".into(),
        }
    }
    fn property(name: &str, value: Value) -> Value {
        json!({"event":"property-change", "name":name,"data":value})
    }
    #[test]
    fn listening_excludes_pauses_and_forward_seeks() {
        let now = Instant::now();
        let mut listening = Listening::default();
        listening.sample(0.0, now, true);
        listening.sample(10.0, now + Duration::from_secs(10), true);
        listening.sample(100.0, now + Duration::from_secs(11), true);
        assert_eq!(listening.seconds, 11.0);
        listening.sample(100.0, now + Duration::from_secs(100), false);
        listening.discontinuity();
        listening.sample(100.0, now + Duration::from_secs(101), true);
        listening.sample(110.0, now + Duration::from_secs(111), true);
        assert_eq!(listening.seconds, 21.0);
    }
    #[test]
    fn repeated_tracks_have_fresh_reporting_and_shuffle_uses_stable_tags() {
        let now = Instant::now();
        let mut state = Playback::new(vec![track("first"), track("second")], true, Some(10));
        let start = state.event(&property("media-title", json!(media_tag(1))), now, 123);
        assert!(matches!(&start.reports[0], Report::NowPlaying {track,..} if track.id == "second"));
        state.listening.previous = Some((0.0, now));
        let heard = state.event(
            &property("time-pos", json!(30)),
            now + Duration::from_secs(30),
            153,
        );
        assert_eq!(heard.reports.len(), 2);
        assert!(
            state
                .event(
                    &property("time-pos", json!(31)),
                    now + Duration::from_secs(31),
                    154
                )
                .reports
                .is_empty()
        );
        state.event(&json!({"event":"start-file"}), now, 200);
        state.event(&property("media-title", json!(media_tag(1))), now, 200);
        let repeated = state.event(&json!({"event":"file-loaded"}), now, 200);
        assert!(matches!(&repeated.reports[0], Report::NowPlaying { .. }));
        assert!(!state.scrobbled && !state.history_reported);
    }
    #[test]
    fn pause_and_seek_do_not_qualify_a_scrobble_and_actual_duration_wins() {
        let now = Instant::now();
        let mut state = Playback::new(vec![track("first")], true, None);
        state.event(&property("media-title", json!(media_tag(0))), now, 100);
        state.event(
            &property("time-pos", json!(10)),
            now + Duration::from_secs(10),
            110,
        );
        state.event(
            &property("pause", json!(true)),
            now + Duration::from_secs(10),
            110,
        );
        state.event(
            &property("time-pos", json!(10)),
            now + Duration::from_secs(100),
            200,
        );
        state.event(
            &property("pause", json!(false)),
            now + Duration::from_secs(100),
            200,
        );
        state.event(
            &json!({"event":"seek"}),
            now + Duration::from_secs(101),
            201,
        );
        assert!(
            state
                .event(
                    &property("time-pos", json!(50)),
                    now + Duration::from_secs(102),
                    202
                )
                .reports
                .is_empty()
        );
        state.event(
            &json!({"event":"playback-restart"}),
            now + Duration::from_secs(102),
            202,
        );
        state.event(
            &property("time-pos", json!(50)),
            now + Duration::from_secs(103),
            203,
        );
        assert!(
            state
                .event(
                    &property("time-pos", json!(55)),
                    now + Duration::from_secs(108),
                    208
                )
                .reports
                .is_empty()
        );
        assert_eq!(state.listening.seconds, 15.0);
        state.event(&property("duration", json!(40)), now, 208);
        assert_eq!(state.duration(), 40);
    }
    #[test]
    fn unknown_duration_waits_for_real_duration_and_radio_never_reports() {
        let now = Instant::now();
        let mut unknown = track("unknown");
        unknown.duration.clear();
        let mut state = Playback::new(vec![unknown], true, None);
        state.event(&property("media-title", json!(media_tag(0))), now, 100);
        state.listening.previous = Some((0.0, now));
        assert!(
            state
                .event(
                    &property("time-pos", json!(30)),
                    now + Duration::from_secs(30),
                    130
                )
                .reports
                .is_empty()
        );
        state.event(&property("duration", json!(60)), now, 130);
        assert!(matches!(
            &state
                .event(
                    &property("time-pos", json!(31)),
                    now + Duration::from_secs(31),
                    131
                )
                .reports[0],
            Report::Scrobble { .. }
        ));
        let mut radio = Playback::new(vec![track("radio:https://example.com")], true, Some(1));
        assert!(
            radio
                .event(&property("media-title", json!(media_tag(0))), now, 0)
                .reports
                .is_empty()
        );
    }
}
