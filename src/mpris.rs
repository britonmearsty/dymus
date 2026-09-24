//! MPRIS (Media Player Remote Interfacing Specification) integration.
//!
//! Dymus exposes `org.mpris.MediaPlayer2.dymus` on the session bus so desktop
//! shells and media-key daemons can show the active track and control
//! playback. The service runs in its own thread with its own Tokio runtime so
//! that zbus dispatch never stalls the terminal event loop. The terminal and
//! MPRIS sides only ever talk through channels: the terminal pushes state
//! updates in, and media controllers' method calls come back as commands.

use std::thread;

use mpris_server::{Metadata, Player, Time, TrackId};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub use mpris_server::{LoopStatus, PlaybackStatus};

/// A method call received from an MPRIS client, to be handled by the terminal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MprisCommand {
    Play,
    PlayPause,
    Pause,
    Stop,
    Next,
    Previous,
    Seek { offset_micros: i64 },
    SetPosition { position_micros: i64 },
    SetVolume { volume: f64 },
    SetLoopStatus(LoopStatus),
    SetShuffle(bool),
    Quit,
}

/// A playback change the terminal wants MPRIS clients to see.
#[derive(Debug, Clone)]
pub enum Update {
    Status(PlaybackStatus),
    Track {
        title: String,
        artist: String,
        album: String,
        video_id: Option<String>,
        duration_secs: f64,
    },
    Position {
        seconds: f64,
    },
    Duration {
        seconds: f64,
    },
    Volume {
        ratio: f64,
    },
    LoopStatus(LoopStatus),
    Shuffle(bool),
    CanGoNext(bool),
    Seeked {
        seconds: f64,
    },
}

/// Sends playback state to the MPRIS thread and receives its commands.
pub struct Mpris {
    updates: Option<UnboundedSender<Update>>,
    commands: Option<UnboundedReceiver<MprisCommand>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Mpris {
    pub fn spawn() -> Self {
        let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
        let (commands_tx, commands_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = thread::Builder::new()
            .name("dymus-mpris".to_owned())
            .spawn(move || driver(updates_rx, commands_tx))
            .ok();
        Self {
            updates: Some(updates_tx),
            commands: Some(commands_rx),
            handle,
        }
    }

    pub fn update(&self, update: Update) {
        if let Some(sender) = &self.updates {
            let _ = sender.send(update);
        }
    }

    pub fn poll_commands(&mut self) -> Vec<MprisCommand> {
        let mut commands = Vec::new();
        if let Some(receiver) = &mut self.commands {
            while let Ok(command) = receiver.try_recv() {
                commands.push(command);
            }
        }
        commands
    }

    /// Stops the service and releases the bus name. Dropping the sender first
    /// makes the MPRIS thread exit, so joining never hangs.
    pub fn shutdown(&mut self) {
        self.updates = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// The service thread: in tests it merely fails to connect and exits.
fn driver(mut updates: UnboundedReceiver<Update>, commands: UnboundedSender<MprisCommand>) {
    // A multi-thread runtime keeps zbus's I/O driver alive while the terminal
    // event loop waits between state updates.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async {
        let player = match Player::builder("dymus")
            .identity("Dymus")
            .supported_uri_schemes(["http", "https"])
            .can_quit(true)
            .can_play(true)
            .can_pause(true)
            .can_seek(true)
            .can_control(true)
            .can_go_next(false)
            .can_go_previous(true)
            .volume(0.7)
            .build()
            .await
        {
            // No session bus (or the name is taken) simply disables MPRIS.
            Ok(player) => player,
            Err(_) => return,
        };

        let play = commands.clone();
        player.connect_play(move |_| {
            let _ = play.send(MprisCommand::Play);
        });
        let play_pause = commands.clone();
        player.connect_play_pause(move |_| {
            let _ = play_pause.send(MprisCommand::PlayPause);
        });
        let pause = commands.clone();
        player.connect_pause(move |_| {
            let _ = pause.send(MprisCommand::Pause);
        });
        let stop = commands.clone();
        player.connect_stop(move |_| {
            let _ = stop.send(MprisCommand::Stop);
        });
        let next = commands.clone();
        player.connect_next(move |_| {
            let _ = next.send(MprisCommand::Next);
        });
        let previous = commands.clone();
        player.connect_previous(move |_| {
            let _ = previous.send(MprisCommand::Previous);
        });
        let seek = commands.clone();
        player.connect_seek(move |_player, offset| {
            let _ = seek.send(MprisCommand::Seek {
                offset_micros: offset.as_micros(),
            });
        });
        let set_position = commands.clone();
        player.connect_set_position(move |_player, _track, position| {
            let _ = set_position.send(MprisCommand::SetPosition {
                position_micros: position.as_micros(),
            });
        });
        let set_volume = commands.clone();
        player.connect_set_volume(move |_player, volume| {
            let _ = set_volume.send(MprisCommand::SetVolume { volume });
        });
        let set_loop_status = commands.clone();
        player.connect_set_loop_status(move |_player, status| {
            let _ = set_loop_status.send(MprisCommand::SetLoopStatus(status));
        });
        let set_shuffle = commands.clone();
        player.connect_set_shuffle(move |_player, shuffle| {
            let _ = set_shuffle.send(MprisCommand::SetShuffle(shuffle));
        });
        let quit = commands.clone();
        player.connect_quit(move |_| {
            let _ = quit.send(MprisCommand::Quit);
        });

        let run = player.run();
        tokio::pin!(run);
        let mut state = TrackState::default();
        loop {
            tokio::select! {
                _ = &mut run => break,
                update = updates.recv() => match update {
                    None => break,
                    Some(update) => apply(&player, update, &mut state).await,
                }
            }
        }
        // Dropping the player and its run task releases the bus name.
    });
}

#[derive(Default)]
struct TrackState {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    video_id: Option<String>,
    duration_micros: i64,
    position_micros: i64,
}

async fn apply(player: &Player, update: Update, state: &mut TrackState) {
    match update {
        Update::Status(status) => {
            let _ = player.set_playback_status(status).await;
        }
        Update::Track {
            title,
            artist,
            album,
            video_id,
            duration_secs,
        } => {
            state.title = Some(title);
            state.artist = Some(artist);
            state.album = Some(album);
            state.video_id = video_id;
            if duration_secs > 0.0 {
                state.duration_micros = (duration_secs * 1_000_000.0) as i64;
            }
            state.position_micros = 0;
            player.set_position(Time::ZERO);
            let _ = player.set_metadata(metadata_for(state)).await;
            let _ = player.seeked(Time::ZERO).await;
        }
        Update::Position { seconds } => {
            state.position_micros = (seconds.max(0.0) * 1_000_000.0) as i64;
            player.set_position(Time::from_micros(state.position_micros));
        }
        Update::Duration { seconds } => {
            state.duration_micros = (seconds.max(0.0) * 1_000_000.0) as i64;
            if state.title.is_some() {
                let _ = player.set_metadata(metadata_for(state)).await;
            }
        }
        Update::Volume { ratio } => {
            let _ = player.set_volume(ratio.clamp(0.0, 1.0)).await;
        }
        Update::LoopStatus(status) => {
            let _ = player.set_loop_status(status).await;
        }
        Update::Shuffle(shuffle) => {
            let _ = player.set_shuffle(shuffle).await;
        }
        Update::CanGoNext(can) => {
            let _ = player.set_can_go_next(can).await;
        }
        Update::Seeked { seconds } => {
            state.position_micros = (seconds.max(0.0) * 1_000_000.0) as i64;
            player.set_position(Time::from_micros(state.position_micros));
            let _ = player
                .seeked(Time::from_micros(state.position_micros))
                .await;
        }
    }
}

fn metadata_for(state: &TrackState) -> Metadata {
    let mut builder = Metadata::builder();
    if state.video_id.is_none() && state.title.is_none() {
        return builder.trackid(TrackId::NO_TRACK).build();
    }
    if let Some(title) = &state.title {
        builder = builder.title(title.clone());
    }
    if let Some(artist) = &state.artist
        && !artist.is_empty()
    {
        // YouTube Music separates artists with " • " and datasets with commas.
        let artists: Vec<&str> = artist
            .split(" • ")
            .flat_map(|part| part.split(','))
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if !artists.is_empty() {
            builder = builder.artist(artists);
        }
    }
    if let Some(album) = &state.album
        && !album.is_empty()
    {
        builder = builder.album(album);
    }
    if state.duration_micros > 0 {
        builder = builder.length(Time::from_micros(state.duration_micros));
    }
    if let Some(video_id) = &state.video_id {
        let element: String = video_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if let Ok(track_id) = TrackId::try_from(format!("/dymus/{element}")) {
            builder = builder.trackid(track_id);
        } else {
            builder = builder.trackid(TrackId::NO_TRACK);
        }
        builder = builder.url(format!("https://music.youtube.com/watch?v={video_id}"));
        builder = builder.art_url(format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"));
    } else if let Some(title) = &state.title {
        let element: String = title
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if let Ok(track_id) = TrackId::try_from(format!("/dymus/radio/{element}")) {
            builder = builder.trackid(track_id);
        } else {
            builder = builder.trackid(TrackId::NO_TRACK);
        }
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        video_id: Option<&str>,
        duration_secs: f64,
    ) -> TrackState {
        TrackState {
            title: title.map(Into::into),
            artist: artist.map(Into::into),
            album: album.map(Into::into),
            video_id: video_id.map(Into::into),
            duration_micros: (duration_secs * 1_000_000.0) as i64,
            position_micros: 0,
        }
    }

    #[test]
    fn metadata_for_video_track() {
        let metadata = metadata_for(&state(
            Some("Lose Yourself"),
            Some("Eminem • D12"),
            Some("8 Mile"),
            Some("axf7kwx123A"),
            326.0,
        ));
        assert_eq!(metadata.title(), Some("Lose Yourself"));
        assert_eq!(metadata.album(), Some("8 Mile"));
        assert_eq!(metadata.artist(), Some(vec!["Eminem".into(), "D12".into()]));
        assert_eq!(
            metadata.trackid().unwrap().to_string(),
            "/dymus/axf7kwx123A"
        );
        assert_eq!(metadata.length().unwrap().as_micros(), 326_000_000);
        assert_eq!(
            metadata.url().unwrap().to_string(),
            "https://music.youtube.com/watch?v=axf7kwx123A"
        );
        assert_eq!(
            metadata.art_url().unwrap().to_string(),
            "https://i.ytimg.com/vi/axf7kwx123A/hqdefault.jpg"
        );
    }

    #[test]
    fn metadata_splits_artists_on_comma_and_trims() {
        let metadata = metadata_for(&state(
            Some("Song"),
            Some("Artist A, Artist B •  Artist C"),
            None,
            None,
            0.0,
        ));
        assert_eq!(
            metadata.artist(),
            Some(vec![
                "Artist A".into(),
                "Artist B".into(),
                "Artist C".into()
            ])
        );
        assert_eq!(metadata.url(), None);
        assert_eq!(metadata.art_url(), None);
    }

    #[test]
    fn metadata_for_radio_track() {
        let metadata = metadata_for(&state(
            Some("Weird / Song: Radio Mix"),
            Some("Some Artist"),
            None,
            None,
            180.0,
        ));
        assert_eq!(
            metadata.trackid().unwrap().to_string(),
            "/dymus/radio/Weird___Song__Radio_Mix"
        );
        assert_eq!(metadata.length().unwrap().as_micros(), 180_000_000);
    }

    #[test]
    fn metadata_for_empty_track_is_no_track() {
        let metadata = metadata_for(&state(None, Some("Ghost"), None, None, 0.0));
        assert_eq!(
            metadata.trackid().unwrap().to_string(),
            "/org/mpris/MediaPlayer2/TrackList/NoTrack"
        );
        assert_eq!(metadata.title(), None);
    }
}
