use std::{
    collections::{BTreeSet, VecDeque},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::{DefaultTerminal, widgets::TableState};
use serde_json::json;
use tokio::task::JoinHandle;

use crate::{
    cache,
    config::{self, Config},
    innertube::{
        DiscoveryContinuation, DiscoveryPage, InnerTube, LibraryItem, LibraryKind, LibraryPage,
        SearchFilter, SearchPage, TrackPage,
    },
    model::{Queue, Track},
    player::{Event, Player},
    radio::{RadioBrowser, Station, StationFilter},
    ui,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback {
    Idle,
    Loading,
    Playing,
    Paused,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepeatMode {
    Off,
    Track,
    Queue,
}

impl RepeatMode {
    fn next(self) -> Self {
        match self {
            Self::Off => Self::Track,
            Self::Track => Self::Queue,
            Self::Queue => Self::Off,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Track => "track",
            Self::Queue => "queue",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NowPanel {
    Queue,
    Visualizer,
    Lyrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VisualizerMode {
    #[default]
    Spectrum,
    Waveform,
    Orbit,
    Pulse,
    Spectrogram,
    Vectorscope,
    Constellation,
    Bars,
    BrailleBars,
}

impl VisualizerMode {
    pub fn next(self) -> Self {
        match self {
            Self::Spectrum => Self::Waveform,
            Self::Waveform => Self::Orbit,
            Self::Orbit => Self::Pulse,
            Self::Pulse => Self::Spectrogram,
            Self::Spectrogram => Self::Vectorscope,
            Self::Vectorscope => Self::Constellation,
            Self::Constellation => Self::Bars,
            Self::Bars => Self::BrailleBars,
            Self::BrailleBars => Self::Spectrum,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Spectrum => Self::BrailleBars,
            Self::Waveform => Self::Spectrum,
            Self::Orbit => Self::Waveform,
            Self::Pulse => Self::Orbit,
            Self::Spectrogram => Self::Pulse,
            Self::Vectorscope => Self::Spectrogram,
            Self::Constellation => Self::Vectorscope,
            Self::Bars => Self::Constellation,
            Self::BrailleBars => Self::Bars,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Spectrum => "spectrum",
            Self::Waveform => "waveform",
            Self::Orbit => "orbit",
            Self::Pulse => "pulse",
            Self::Spectrogram => "spectrogram",
            Self::Vectorscope => "vectorscope",
            Self::Constellation => "constellation",
            Self::Bars => "bars",
            Self::BrailleBars => "braille bars",
        }
    }
}

pub struct CoverArt {
    pub image: image::DynamicImage,
    pub protocol: Option<ratatui_image::protocol::Protocol>,
    pub protocol_area: Option<ratatui::layout::Size>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Play,
    PlayAll,
    Add,
    PlayNext,
    QueueAll,
    Radio,
    Remove,
    ClearQueue,
    SelectAll,
    ClearSelection,
}

pub struct MenuItem {
    pub action: Action,
    pub key: &'static str,
    pub label: &'static str,
}

pub struct App {
    pub input: String,
    pub editing: bool,
    pub query: String,
    pub results: Vec<Track>,
    pub search_items: Vec<LibraryItem>,
    pub search_filter: SearchFilter,
    pub search_continuation: Option<String>,
    pub results_state: TableState,
    pub queue: Queue,
    pub queue_state: TableState,
    pub queue_focused: bool,
    pub library_focused: bool,
    pub library_detail: bool,
    search_detail: bool,
    pub library_kind: LibraryKind,
    pub library_items: Vec<LibraryItem>,
    pub library_continuation: Option<String>,
    pub library_state: TableState,
    pub library_loading: bool,
    pub library_detail_continuation: Option<String>,
    pub home_focused: bool,
    pub explore_focused: bool,
    pub radio_focused: bool,
    pub radio_editing: bool,
    pub radio_filter_field: usize,
    pub radio_sort_index: usize,
    pub radio_input: String,
    pub radio_filter: StationFilter,
    pub radio_stations: Vec<Station>,
    pub radio_state: TableState,
    pub radio_loading: bool,
    pub discovery_items: Vec<LibraryItem>,
    pub discovery_continuations: Vec<DiscoveryContinuation>,
    pub discovery_state: TableState,
    pub discovery_loading: bool,
    pub content_detail: bool,
    pub now_playing_view: bool,
    pub now_panel: NowPanel,
    pub visualizer_mode: VisualizerMode,
    pub visualizer_phase: f64,
    pub audio_bands: Vec<f32>,
    pub audio_waveform: Vec<f32>,
    pub audio_scope: Vec<(f32, f32)>,
    pub audio_rms: f32,
    pub audio_available: bool,
    pub audio_capture_error: Option<String>,
    pub spectrogram_history: VecDeque<Vec<f32>>,
    pub cover_art: Option<CoverArt>,
    pub cover_loading: bool,
    pub lyrics: Option<crate::lyrics::Lyrics>,
    pub lyrics_loading: bool,
    pub lyrics_error: Option<String>,
    pub lyrics_plain: bool,
    pub lyrics_scroll: u16,
    pub image_picker: ratatui_image::picker::Picker,
    pub help: bool,
    pub help_scroll: usize,
    pub settings_view: bool,
    pub settings_selected: usize,
    pub config: Config,
    pub menu: bool,
    pub menu_state: TableState,
    pub result_marks: BTreeSet<usize>,
    pub queue_marks: BTreeSet<usize>,
    pub status: String,
    pub searching: bool,
    pub playback: Playback,
    pub position: f64,
    history_reported: bool,
    repeat_mode: RepeatMode,
    repeat_history: Vec<Track>,
    lastfm_reported: bool,
    lastfm_started_at: Option<u64>,
    pub duration: f64,
    pub volume: u8,
    api: InnerTube,
    player: Player,
    radio_api: RadioBrowser,
    generation: u64,
    search_task: Option<JoinHandle<Result<SearchPage>>>,
    search_appending: bool,
    radio_task: Option<JoinHandle<Result<Vec<Track>>>>,
    library_task: Option<JoinHandle<Result<LibraryPage>>>,
    library_appending: bool,
    detail_task: Option<JoinHandle<Result<TrackPage>>>,
    detail_appending: bool,
    discovery_task: Option<JoinHandle<Result<DiscoveryPage>>>,
    discovery_appending: bool,
    discovery_append_section: Option<String>,
    stations_task: Option<JoinHandle<Result<Vec<Station>>>>,
    cover_task: Option<JoinHandle<(u64, Option<CoverArt>)>>,
    lyrics_task: Option<JoinHandle<(u64, Result<Option<crate::lyrics::Lyrics>>)>>,
    auth_task: Option<JoinHandle<Result<String>>>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let config = Config::load()?;
        let api = InnerTube::configured()?;
        let auth_task = api.is_authenticated().then(|| {
            let api = api.clone();
            tokio::spawn(async move { api.validate_session().await })
        });
        Ok(Self {
            input: String::new(),
            editing: true,
            query: String::new(),
            results: Vec::new(),
            search_items: Vec::new(),
            search_filter: SearchFilter::Songs,
            search_continuation: None,
            results_state: TableState::default(),
            queue: Queue {
                current: None,
                upcoming: cache::load_queue().unwrap_or_default().into(),
            },
            queue_state: TableState::default(),
            queue_focused: false,
            library_focused: false,
            library_detail: false,
            search_detail: false,
            library_kind: LibraryKind::Playlists,
            library_items: Vec::new(),
            library_continuation: None,
            library_state: TableState::default(),
            library_loading: false,
            library_detail_continuation: None,
            home_focused: false,
            explore_focused: false,
            radio_focused: false,
            radio_editing: false,
            radio_filter_field: 0,
            radio_sort_index: 0,
            radio_input: String::new(),
            radio_filter: StationFilter::default(),
            radio_stations: Vec::new(),
            radio_state: TableState::default(),
            radio_loading: false,
            discovery_items: Vec::new(),
            discovery_continuations: Vec::new(),
            discovery_state: TableState::default(),
            discovery_loading: false,
            content_detail: false,
            now_playing_view: false,
            now_panel: NowPanel::Queue,
            visualizer_mode: VisualizerMode::Spectrum,
            visualizer_phase: 0.0,
            audio_bands: Vec::new(),
            audio_waveform: Vec::new(),
            audio_scope: Vec::new(),
            audio_rms: 0.0,
            audio_available: false,
            audio_capture_error: None,
            spectrogram_history: VecDeque::with_capacity(24),
            cover_art: None,
            cover_loading: false,
            lyrics: None,
            lyrics_loading: false,
            lyrics_error: None,
            lyrics_plain: false,
            lyrics_scroll: 0,
            image_picker: ratatui_image::picker::Picker::halfblocks(),
            help: false,
            help_scroll: 0,
            settings_view: false,
            settings_selected: 0,
            config,
            menu: false,
            menu_state: TableState::default(),
            result_marks: BTreeSet::new(),
            queue_marks: BTreeSet::new(),
            status: String::new(),
            searching: false,
            playback: Playback::Idle,
            position: 0.0,
            history_reported: false,
            repeat_mode: RepeatMode::Off,
            repeat_history: Vec::new(),
            lastfm_reported: false,
            lastfm_started_at: None,
            duration: 0.0,
            volume: 70,
            api,
            player: Player::new(),
            radio_api: RadioBrowser::new()?,
            generation: 0,
            search_task: None,
            search_appending: false,
            radio_task: None,
            library_task: None,
            library_appending: false,
            detail_task: None,
            detail_appending: false,
            discovery_task: None,
            discovery_appending: false,
            discovery_append_section: None,
            stations_task: None,
            cover_task: None,
            lyrics_task: None,
            auth_task,
        })
    }

    pub fn start(&mut self) {
        if self.auth_task.is_none() {
            self.status = "Not signed in — public music works; run `dymus auth paste` for library and restricted playback".into();
        }
        self.editing = self.config.start_view == "search";
        match self.config.start_view.as_str() {
            "home" => {
                #[cfg(test)]
                {
                    self.home_focused = true;
                }
                #[cfg(not(test))]
                self.load_discovery(false);
            }
            "explore" => {
                #[cfg(test)]
                {
                    self.explore_focused = true;
                }
                #[cfg(not(test))]
                self.load_discovery(true);
            }
            "library" | "playlists" => {
                #[cfg(test)]
                {
                    self.library_focused = true;
                }
                #[cfg(not(test))]
                self.load_library(LibraryKind::Playlists);
            }
            "albums" => {
                #[cfg(test)]
                {
                    self.library_focused = true;
                    self.library_kind = LibraryKind::Albums;
                }
                #[cfg(not(test))]
                self.load_library(LibraryKind::Albums);
            }
            "artists" => {
                #[cfg(test)]
                {
                    self.library_focused = true;
                    self.library_kind = LibraryKind::Artists;
                }
                #[cfg(not(test))]
                self.load_library(LibraryKind::Artists);
            }
            "podcasts" => {
                #[cfg(test)]
                {
                    self.library_focused = true;
                    self.library_kind = LibraryKind::Podcasts;
                }
                #[cfg(not(test))]
                self.load_library(LibraryKind::Podcasts);
            }
            "radio" => {
                #[cfg(test)]
                {
                    self.radio_focused = true;
                }
                #[cfg(not(test))]
                self.load_radio();
            }
            "queue" => {
                self.editing = false;
                self.queue_focused = true;
            }
            _ => self.editing = true,
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tick.tick().await;
            self.poll_auth().await;
            if self.playback == Playback::Playing && self.now_panel == NowPanel::Visualizer {
                self.visualizer_phase += 0.04;
            }
            if self
                .search_task
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                let result = self.search_task.take().unwrap().await;
                self.searching = false;
                match result {
                    Ok(Ok(page)) => {
                        self.status.clear();
                        if self.search_appending {
                            if self.search_filter == SearchFilter::Songs {
                                let mut ids = self
                                    .results
                                    .iter()
                                    .map(|track| track.id.clone())
                                    .collect::<BTreeSet<_>>();
                                self.results.extend(
                                    page.tracks
                                        .into_iter()
                                        .filter(|track| ids.insert(track.id.clone())),
                                );
                            } else {
                                let mut ids = self
                                    .search_items
                                    .iter()
                                    .map(library_item_key)
                                    .collect::<BTreeSet<_>>();
                                self.search_items.extend(
                                    page.items
                                        .into_iter()
                                        .filter(|item| ids.insert(library_item_key(item))),
                                );
                            }
                        } else {
                            self.results = page.tracks;
                            self.search_items = page.items;
                        }
                        self.search_continuation = page.continuation;
                        self.search_appending = false;
                        let count = if self.search_filter == SearchFilter::Songs {
                            self.results.len()
                        } else {
                            self.search_items.len()
                        };
                        self.results_state
                            .select(if count == 0 { None } else { Some(0) });
                    }
                    Ok(Err(error)) => self.status = format!("Search failed: {error:#}"),
                    Err(error) => self.status = format!("Search task failed: {error}"),
                }
            }
            self.poll_radio().await;
            self.poll_stations().await;
            self.poll_library().await;
            self.poll_discovery().await;
            self.poll_cover().await;
            self.poll_lyrics().await;
            while let Ok((generation, event)) = self.player.events.try_recv() {
                self.on_player_event(generation, event);
            }
            self.clamp_queue_selection();
            terminal.draw(|frame| ui::draw(frame, self))?;
            // Poll without blocking Tokio: network and player jobs keep running.
            while event::poll(Duration::ZERO)? {
                if let TerminalEvent::Key(key) = event::read()?
                    && key.kind != KeyEventKind::Release
                    && self.handle_key(key)
                {
                    return Ok(());
                }
            }
        }
    }

    async fn poll_auth(&mut self) {
        if self
            .auth_task
            .as_ref()
            .is_none_or(|task| !task.is_finished())
        {
            return;
        }
        let task = self.auth_task.take().expect("checked above");
        match task.await {
            Ok(Ok(account)) => self.status = format!("Signed in to YouTube Music as {account}"),
            Ok(Err(error)) => {
                self.status = format!(
                    "YouTube Music sign-in is expired or invalid — run `dymus auth paste` with a fresh Cookie header ({error:#})"
                )
            }
            Err(error) => {
                self.status = format!(
                    "Could not check YouTube Music sign-in — run `dymus auth status` ({error})"
                )
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.help = false,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll =
                        (self.help_scroll + 1).min(ui::SHORTCUTS.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                _ => {}
            }
            return false;
        }
        if self.settings_view {
            match key.code {
                KeyCode::Esc => self.settings_view = false,
                KeyCode::Up | KeyCode::Char('k') => self.settings_selected = 0,
                KeyCode::Down | KeyCode::Char('j') => self.settings_selected = 1,
                KeyCode::Left
                | KeyCode::Char('h')
                | KeyCode::Right
                | KeyCode::Char('l')
                | KeyCode::Enter => {
                    let delta: isize = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                        -1
                    } else {
                        1
                    };
                    if self.settings_selected == 0 {
                        let current = config::THEMES
                            .iter()
                            .position(|theme| *theme == self.config.theme)
                            .unwrap_or(0);
                        let next = (current as isize + delta)
                            .rem_euclid(config::THEMES.len() as isize)
                            as usize;
                        self.config.theme = config::THEMES[next].into();
                    } else {
                        let current = config::START_VIEWS
                            .iter()
                            .position(|view| *view == self.config.start_view)
                            .unwrap_or(0);
                        let next = (current as isize + delta)
                            .rem_euclid(config::START_VIEWS.len() as isize)
                            as usize;
                        self.config.start_view = config::START_VIEWS[next].into();
                    }
                    if let Err(error) = self.config.save() {
                        self.status = format!("Could not save settings: {error:#}");
                    }
                }
                _ => {}
            }
            return false;
        }
        if self.menu {
            match key.code {
                KeyCode::Esc | KeyCode::Char('.') | KeyCode::Char('q') => self.menu = false,
                KeyCode::Down | KeyCode::Char('j') => self.menu_state.select(moved(
                    self.menu_state.selected(),
                    self.menu_items().len(),
                    1,
                )),
                KeyCode::Up | KeyCode::Char('k') => self.menu_state.select(moved(
                    self.menu_state.selected(),
                    self.menu_items().len(),
                    -1,
                )),
                KeyCode::Enter => {
                    if let Some(item) = self
                        .menu_state
                        .selected()
                        .and_then(|i| self.menu_items().get(i).map(|item| item.action))
                    {
                        self.menu = false;
                        self.execute(item);
                    }
                }
                _ => {
                    if let Some(action) = self.action_for_key(key.code) {
                        self.menu = false;
                        self.execute(action);
                    }
                }
            }
            return false;
        }
        if self.now_playing_view {
            match key.code {
                KeyCode::Esc => {
                    self.now_playing_view = false;
                    self.cover_art = None;
                    self.cover_loading = false;
                    if let Some(task) = self.cover_task.take() {
                        task.abort();
                    }
                    return false;
                }
                KeyCode::Char('q') => {
                    self.now_panel = NowPanel::Queue;
                    return false;
                }
                KeyCode::Char('v') => {
                    self.now_panel = NowPanel::Visualizer;
                    return false;
                }
                KeyCode::Char('y') => {
                    self.now_panel = NowPanel::Lyrics;
                    self.request_lyrics();
                    return false;
                }
                KeyCode::Char('p') if self.now_panel == NowPanel::Lyrics => {
                    if self
                        .lyrics
                        .as_ref()
                        .is_some_and(|lyrics| lyrics.has_synced() && lyrics.plain.is_some())
                    {
                        self.lyrics_plain = !self.lyrics_plain;
                        self.lyrics_scroll = 0;
                    }
                    return false;
                }
                KeyCode::Char(']') | KeyCode::Char('m')
                    if self.now_panel == NowPanel::Visualizer =>
                {
                    self.visualizer_mode = self.visualizer_mode.next();
                    return false;
                }
                KeyCode::Char('[') if self.now_panel == NowPanel::Visualizer => {
                    self.visualizer_mode = self.visualizer_mode.previous();
                    return false;
                }
                KeyCode::Down | KeyCode::Char('j') if self.now_panel == NowPanel::Lyrics => {
                    if self.lyrics_plain {
                        self.lyrics_scroll = self.lyrics_scroll.saturating_add(1);
                    }
                    return false;
                }
                KeyCode::Up | KeyCode::Char('k') if self.now_panel == NowPanel::Lyrics => {
                    if self.lyrics_plain {
                        self.lyrics_scroll = self.lyrics_scroll.saturating_sub(1);
                    }
                    return false;
                }
                KeyCode::Tab => {
                    self.now_panel = match self.now_panel {
                        NowPanel::Queue => NowPanel::Visualizer,
                        NowPanel::Visualizer => NowPanel::Lyrics,
                        NowPanel::Lyrics => NowPanel::Queue,
                    };
                    if self.now_panel == NowPanel::Lyrics {
                        self.request_lyrics();
                    }
                    return false;
                }
                KeyCode::BackTab => {
                    self.now_panel = match self.now_panel {
                        NowPanel::Queue => NowPanel::Lyrics,
                        NowPanel::Visualizer => NowPanel::Queue,
                        NowPanel::Lyrics => NowPanel::Visualizer,
                    };
                    if self.now_panel == NowPanel::Lyrics {
                        self.request_lyrics();
                    }
                    return false;
                }
                KeyCode::Down | KeyCode::Up | KeyCode::Char('j' | 'k' | 'g' | 'G' | 'x')
                    if self.now_panel != NowPanel::Queue =>
                {
                    return false;
                }
                KeyCode::Char('h' | 'e' | 'l' | 't') => return false,
                _ => {}
            }
        }
        if self.radio_editing {
            match key.code {
                KeyCode::Esc => {
                    self.radio_editing = false;
                    self.radio_input.clear();
                }
                KeyCode::Enter => self.commit_radio_filter(),
                KeyCode::Backspace => {
                    self.radio_input.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.radio_input.push(c);
                }
                _ => {}
            }
            return false;
        }
        if self.editing {
            match key.code {
                KeyCode::Esc => self.editing = false,
                KeyCode::Enter => self.search(),
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.clear()
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.push(c)
                }
                _ => {}
            }
            return false;
        }
        if binding_matches(self.config.keybindings.get("quit"), key) {
            return true;
        }
        if binding_matches(self.config.keybindings.get("settings"), key) {
            self.settings_view = true;
            self.settings_selected = 0;
            return false;
        }
        if binding_matches(self.config.keybindings.get("search"), key) {
            if self.radio_focused {
                self.edit_radio_filter(0);
                return false;
            }
            self.input = self.query.clone();
            self.editing = true;
            return false;
        }
        if binding_matches(self.config.keybindings.get("help"), key) {
            self.help = true;
            self.help_scroll = 0;
            return false;
        }
        if !self.now_playing_view && binding_matches(self.config.keybindings.get("home"), key) {
            self.load_discovery(false);
            return false;
        }
        if !self.now_playing_view && binding_matches(self.config.keybindings.get("explore"), key) {
            self.load_discovery(true);
            return false;
        }
        if !self.now_playing_view {
            let destinations = [
                ("playlists", LibraryKind::Playlists),
                ("albums", LibraryKind::Albums),
                ("artists", LibraryKind::Artists),
                ("podcasts", LibraryKind::Podcasts),
            ];
            if let Some((_, kind)) = destinations
                .iter()
                .find(|(action, _)| binding_matches(self.config.keybindings.get(action), key))
            {
                self.load_library(*kind);
                return false;
            }
        }
        if !self.now_playing_view && binding_matches(self.config.keybindings.get("radio"), key) {
            self.load_radio();
            return false;
        }
        if binding_matches(self.config.keybindings.get("queue"), key) {
            self.queue_focused = !self.queue_focused;
            self.library_focused = false;
            self.home_focused = false;
            self.explore_focused = false;
            self.radio_focused = false;
            return false;
        }
        if binding_matches(self.config.keybindings.get("now_playing"), key)
            && self.queue.current.is_some()
        {
            self.open_now_playing();
            return false;
        }
        if binding_matches(self.config.keybindings.get("pause"), key)
            && matches!(self.playback, Playback::Playing | Playback::Paused)
        {
            self.player.command(json!(["cycle", "pause"]));
            return false;
        }
        if binding_matches(self.config.keybindings.get("next_track"), key) {
            self.cancel_radio();
            self.next();
            return false;
        }
        if binding_matches(self.config.keybindings.get("retry_track"), key) {
            self.cancel_radio();
            if let Some(track) = self.queue.current.clone() {
                self.play(track);
            }
            return false;
        }
        if binding_matches(self.config.keybindings.get("volume_up"), key) {
            self.set_volume(self.volume.saturating_add(5).min(100));
            return false;
        }
        if binding_matches(self.config.keybindings.get("volume_down"), key) {
            self.set_volume(self.volume.saturating_sub(5));
            return false;
        }
        if key.code == KeyCode::Char('Z') && !self.radio_focused {
            self.player.clear_preloaded();
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos() as u64);
            self.queue.shuffle(seed);
            self.preload_next();
            self.status = "Shuffled upcoming tracks".into();
            return false;
        }
        if key.code == KeyCode::Char('T') {
            self.repeat_mode = self.repeat_mode.next();
            self.player.command(json!([
                "set_property",
                "loop-file",
                if self.repeat_mode == RepeatMode::Track {
                    "inf"
                } else {
                    "no"
                }
            ]));
            self.status = format!("Repeat: {}", self.repeat_mode.label());
            return false;
        }
        if self.radio_focused {
            match key.code {
                KeyCode::Esc => self.status.clear(),
                KeyCode::Char('/') => self.edit_radio_filter(0),
                KeyCode::Char('c') => self.edit_radio_filter(1),
                KeyCode::Char('l') => self.edit_radio_filter(2),
                KeyCode::Char('g') => self.edit_radio_filter(3),
                KeyCode::Char('f') => self.radio_filter_field = (self.radio_filter_field + 1) % 4,
                KeyCode::Char('i') => self.edit_radio_filter(self.radio_filter_field),
                KeyCode::Char('z') => self.cycle_radio_sort(),
                KeyCode::Char('x') => {
                    self.radio_filter = StationFilter::default();
                    self.radio_filter_field = 0;
                    self.radio_sort_index = 0;
                    self.refresh_stations();
                }
                KeyCode::Down | KeyCode::Char('j') => self.radio_state.select(moved(
                    self.radio_state.selected(),
                    self.radio_stations.len(),
                    1,
                )),
                KeyCode::Up | KeyCode::Char('k') => self.radio_state.select(moved(
                    self.radio_state.selected(),
                    self.radio_stations.len(),
                    -1,
                )),
                KeyCode::Enter => self.play_radio_station(),
                _ => {}
            }
            return false;
        }
        if ((!self.library_focused || self.library_detail)
            && (!self.home_focused && !self.explore_focused || self.content_detail))
            && (!self.now_playing_view || self.now_panel == NowPanel::Queue)
            && (self.queue_context()
                || self.library_focused
                || self.home_focused
                || self.explore_focused
                || self.search_filter == SearchFilter::Songs)
            && let Some(action) = self.action_for_key(key.code)
        {
            self.execute(action);
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('t') if self.queue.current.is_some() => {
                self.open_now_playing();
            }
            KeyCode::Esc => {
                if self.library_focused && self.library_detail {
                    if self.search_detail {
                        self.library_focused = false;
                        self.search_detail = false;
                    }
                    self.library_detail = false;
                    self.results.clear();
                    self.status.clear();
                    return false;
                }
                if (self.home_focused || self.explore_focused) && self.content_detail {
                    self.content_detail = false;
                    self.results.clear();
                    self.status.clear();
                    return false;
                }
                self.marks_mut().clear();
                self.cancel_radio();
                self.status.clear();
            }
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('.') => {
                if ((!self.library_focused && !self.home_focused && !self.explore_focused)
                    || self.library_detail
                    || self.content_detail)
                    && (!self.now_playing_view || self.now_panel == NowPanel::Queue)
                {
                    self.menu = true;
                    self.menu_state.select(Some(0));
                }
            }
            KeyCode::Char('x')
                if !((self.home_focused || self.explore_focused) && !self.content_detail)
                    && !(self.library_focused && !self.library_detail)
                    && (!self.now_playing_view || self.now_panel == NowPanel::Queue) =>
            {
                self.toggle_mark()
            }
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.queue_focused = !self.queue_focused;
                self.library_focused = false;
                self.home_focused = false;
                self.explore_focused = false;
                self.radio_focused = false;
            }
            KeyCode::Char('l') => {
                self.home_focused = false;
                self.explore_focused = false;
                self.content_detail = false;
                self.queue_focused = false;
                self.library_focused = true;
                self.load_library(LibraryKind::Playlists);
            }
            KeyCode::Char('h') => self.load_discovery(false),
            KeyCode::Char('e') => self.load_discovery(true),
            KeyCode::Char('L')
                if (self.home_focused || self.explore_focused) && !self.content_detail =>
            {
                self.load_more_discovery()
            }
            KeyCode::Char('L') if self.library_focused && !self.library_detail => {
                self.load_more_library()
            }
            KeyCode::Char('L') if self.library_focused && self.library_detail => {
                self.load_more_library_tracks()
            }
            KeyCode::Char('L')
                if !self.queue_focused
                    && !self.library_focused
                    && !self.home_focused
                    && !self.explore_focused =>
            {
                self.load_more_search()
            }
            KeyCode::Char('f')
                if !self.queue_focused
                    && !self.library_focused
                    && !self.home_focused
                    && !self.explore_focused =>
            {
                self.search_filter = self.search_filter.next();
                if self.query.is_empty() {
                    self.status = format!("Search filter: {}", self.search_filter.label());
                } else {
                    self.input = self.query.clone();
                    self.search();
                }
            }
            KeyCode::Enter
                if !self.queue_focused
                    && !self.library_focused
                    && !self.home_focused
                    && !self.explore_focused
                    && self.search_filter != SearchFilter::Songs =>
            {
                self.open_search_item()
            }
            KeyCode::Enter
                if (self.home_focused || self.explore_focused) && !self.content_detail =>
            {
                self.open_discovery_item()
            }
            KeyCode::Enter if self.library_focused && !self.library_detail => {
                self.open_library_item()
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.select_edge(true),
            KeyCode::Char('J') | KeyCode::Char('K') if self.queue_context() => {
                self.cancel_radio();
                let had_marks = !self.queue_marks.is_empty();
                let indices = self.target_indices();
                let moved = self
                    .queue
                    .move_indices(&indices, key.code == KeyCode::Char('J'));
                self.queue_state.select(moved.first().copied());
                if had_marks {
                    self.queue_marks = moved;
                }
            }
            KeyCode::Char('n') => {
                self.cancel_radio();
                self.next();
            }
            KeyCode::Char('r') => {
                self.cancel_radio();
                if let Some(track) = self.queue.current.clone() {
                    self.play(track);
                }
            }
            KeyCode::Char(' ') if matches!(self.playback, Playback::Playing | Playback::Paused) => {
                self.player.command(json!(["cycle", "pause"]))
            }
            KeyCode::Left | KeyCode::Right
                if matches!(self.playback, Playback::Playing | Playback::Paused) =>
            {
                let seconds = if key.code == KeyCode::Left { -5 } else { 5 };
                self.player.command(json!(["seek", seconds, "relative"]));
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.set_volume(self.volume.saturating_add(5).min(100))
            }
            KeyCode::Char('-') => self.set_volume(self.volume.saturating_sub(5)),
            _ => {}
        }
        false
    }

    pub fn marks(&self) -> &BTreeSet<usize> {
        if self.queue_context() {
            &self.queue_marks
        } else {
            &self.result_marks
        }
    }

    fn marks_mut(&mut self) -> &mut BTreeSet<usize> {
        if self.queue_context() {
            &mut self.queue_marks
        } else {
            &mut self.result_marks
        }
    }

    fn focused_index(&self) -> Option<usize> {
        if (self.home_focused || self.explore_focused) && !self.content_detail {
            self.discovery_state.selected()
        } else if self.library_focused && !self.library_detail {
            self.library_state.selected()
        } else if self.queue_context() {
            self.queue_state.selected()
        } else {
            self.results_state.selected()
        }
    }

    fn visible_tracks(&self) -> Vec<Track> {
        if self.queue_context() {
            self.queue.upcoming.iter().cloned().collect()
        } else {
            self.results.clone()
        }
    }

    fn queue_context(&self) -> bool {
        self.queue_focused || (self.now_playing_view && self.now_panel == NowPanel::Queue)
    }

    fn target_indices(&self) -> BTreeSet<usize> {
        if self.marks().is_empty() {
            self.focused_index().into_iter().collect()
        } else {
            self.marks().clone()
        }
    }

    fn toggle_mark(&mut self) {
        if let Some(index) = self.focused_index() {
            if index >= self.visible_tracks().len() {
                return;
            }
            if !self.marks_mut().remove(&index) {
                self.marks_mut().insert(index);
            }
        }
    }

    pub fn menu_items(&self) -> Vec<MenuItem> {
        use Action::*;
        let mut items = vec![
            MenuItem {
                action: Play,
                key: "Enter",
                label: if self.marks().is_empty() {
                    "Play focused song"
                } else {
                    "Play marked songs"
                },
            },
            MenuItem {
                action: PlayAll,
                key: "P",
                label: "Play all · replace queue",
            },
        ];
        if !self.queue_context() {
            items.extend([
                MenuItem {
                    action: Add,
                    key: "a",
                    label: "Add to queue",
                },
                MenuItem {
                    action: PlayNext,
                    key: "A",
                    label: "Play next",
                },
                MenuItem {
                    action: QueueAll,
                    key: "Q",
                    label: "Add all to queue",
                },
            ]);
        }
        items.push(MenuItem {
            action: Radio,
            key: "R",
            label: "Radio · replace queue",
        });
        if self.queue_context() {
            items.extend([
                MenuItem {
                    action: Remove,
                    key: "d",
                    label: "Remove from queue",
                },
                MenuItem {
                    action: ClearQueue,
                    key: "C",
                    label: "Clear upcoming queue",
                },
            ]);
        }
        items.extend([
            MenuItem {
                action: SelectAll,
                key: "v",
                label: "Select / deselect all",
            },
            MenuItem {
                action: ClearSelection,
                key: "X",
                label: "Clear selection",
            },
        ]);
        items
    }

    fn action_for_key(&self, key: KeyCode) -> Option<Action> {
        use Action::*;
        match key {
            KeyCode::Enter => Some(Play),
            KeyCode::Char('P') => Some(PlayAll),
            KeyCode::Char('a') if !self.queue_context() => Some(Add),
            KeyCode::Char('A') if !self.queue_context() => Some(PlayNext),
            KeyCode::Char('Q') if !self.queue_context() => Some(QueueAll),
            KeyCode::Char('R') => Some(Radio),
            KeyCode::Char('d') | KeyCode::Delete if self.queue_context() => Some(Remove),
            KeyCode::Char('C') if self.queue_context() => Some(ClearQueue),
            KeyCode::Char('v') => Some(SelectAll),
            KeyCode::Char('X') => Some(ClearSelection),
            _ => None,
        }
    }

    fn execute(&mut self, action: Action) {
        use Action::*;
        let visible = self.visible_tracks();
        let indices = self.target_indices();
        let picked: Vec<Track> = indices
            .iter()
            .filter_map(|i| visible.get(*i).cloned())
            .collect();
        match action {
            SelectAll => {
                if self.marks().len() == visible.len() {
                    self.marks_mut().clear();
                } else {
                    *self.marks_mut() = (0..visible.len()).collect();
                }
            }
            ClearSelection => self.marks_mut().clear(),
            Radio => {
                if let Some(seed) = self.focused_index().and_then(|i| visible.get(i)).cloned() {
                    self.start_radio(seed);
                }
            }
            ClearQueue => {
                self.cancel_radio();
                self.queue.upcoming.clear();
                self.queue_marks.clear();
            }
            Remove => {
                self.cancel_radio();
                self.queue.remove_indices(&indices);
                self.queue_marks.clear();
            }
            PlayAll if !visible.is_empty() => {
                self.cancel_radio();
                self.replace_and_play(visible);
            }
            QueueAll if !visible.is_empty() => {
                self.cancel_radio();
                self.queue.upcoming.extend(visible);
                self.result_marks.clear();
            }
            Add | PlayNext if !picked.is_empty() => {
                self.cancel_radio();
                if action == Add {
                    self.queue.upcoming.extend(picked);
                } else {
                    self.queue_marks = self.queue_marks.iter().map(|i| i + picked.len()).collect();
                    self.queue.prepend(picked);
                }
                self.result_marks.clear();
            }
            Play if !picked.is_empty() => {
                self.cancel_radio();
                if self.queue_context() {
                    self.queue.remove_indices(&indices);
                    self.queue_marks.clear();
                } else {
                    self.result_marks.clear();
                }
                let mut picked = picked.into_iter();
                let first = picked.next().unwrap();
                let rest: Vec<Track> = picked.collect();
                self.queue_marks = self.queue_marks.iter().map(|i| i + rest.len()).collect();
                self.queue.prepend(rest);
                self.play(first);
            }
            _ => {}
        }
        self.clamp_queue_selection();
    }

    fn replace_and_play(&mut self, tracks: Vec<Track>) {
        if tracks.is_empty() {
            return;
        }
        self.queue.upcoming = tracks.into();
        self.queue_marks.clear();
        self.result_marks.clear();
        self.queue_state.select(None);
        self.next();
    }

    fn start_radio(&mut self, seed: Track) {
        self.cancel_radio();
        self.status.clear();
        let api = self.api.clone();
        self.radio_task = Some(tokio::spawn(async move {
            let tracks = api.radio(&seed.id).await?;
            let mut seen = std::collections::HashSet::from([seed.id.clone()]);
            let mut mix = vec![seed];
            mix.extend(
                tracks
                    .into_iter()
                    .filter(|track| seen.insert(track.id.clone())),
            );
            anyhow::ensure!(mix.len() > 1, "No related songs returned for this track");
            Ok(mix)
        }));
    }

    pub fn radio_loading(&self) -> bool {
        self.radio_task.is_some()
    }

    fn cancel_radio(&mut self) {
        if let Some(task) = self.radio_task.take() {
            task.abort();
        }
    }

    async fn poll_radio(&mut self) {
        if self
            .radio_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            match self.radio_task.take().unwrap().await {
                Ok(Ok(tracks)) => self.replace_and_play(tracks),
                Ok(Err(error)) => self.status = format!("Radio failed: {error:#}"),
                Err(error) => self.status = format!("Radio task failed: {error}"),
            }
        }
    }

    fn load_radio(&mut self) {
        self.editing = false;
        self.radio_editing = false;
        self.radio_focused = true;
        self.home_focused = false;
        self.explore_focused = false;
        self.library_focused = false;
        self.library_detail = false;
        self.content_detail = false;
        self.queue_focused = false;
        self.status.clear();
        if self.radio_stations.is_empty() && !self.radio_loading {
            self.refresh_stations();
        }
    }

    fn refresh_stations(&mut self) {
        if let Some(task) = self.stations_task.take() {
            task.abort();
        }
        let api = self.radio_api.clone();
        let filter = self.radio_filter.clone();
        self.radio_loading = true;
        self.status.clear();
        self.stations_task = Some(tokio::spawn(async move { api.search(&filter).await }));
    }

    fn edit_radio_filter(&mut self, field: usize) {
        self.radio_filter_field = field.min(3);
        self.radio_input = match self.radio_filter_field {
            0 => self.radio_filter.name.clone(),
            1 => self.radio_filter.country.clone(),
            2 => self.radio_filter.language.clone(),
            _ => self.radio_filter.tag.clone(),
        };
        self.radio_editing = true;
    }

    fn commit_radio_filter(&mut self) {
        let value = self.radio_input.trim().to_owned();
        match self.radio_filter_field {
            0 => self.radio_filter.name = value,
            1 => self.radio_filter.country = value,
            2 => self.radio_filter.language = value,
            _ => self.radio_filter.tag = value,
        }
        self.radio_input.clear();
        self.radio_editing = false;
        self.radio_state.select(None);
        self.refresh_stations();
    }

    fn cycle_radio_sort(&mut self) {
        const SORTS: [&str; 5] = ["clickcount", "clicktrend", "votes", "bitrate", "name"];
        self.radio_filter.order = SORTS[(self.radio_sort_index + 1) % SORTS.len()].into();
        self.radio_sort_index = (self.radio_sort_index + 1) % SORTS.len();
        self.radio_state.select(None);
        self.refresh_stations();
    }

    pub fn radio_sort_label(&self) -> &'static str {
        ["popular", "trending", "most voted", "bitrate", "A–Z"][self.radio_sort_index]
    }

    async fn poll_stations(&mut self) {
        if self
            .stations_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.radio_loading = false;
            match self.stations_task.take().unwrap().await {
                Ok(Ok(stations)) => {
                    self.radio_stations = stations;
                    self.radio_state
                        .select((!self.radio_stations.is_empty()).then_some(0));
                    self.status.clear();
                }
                Ok(Err(error)) => self.status = format!("Radio search failed: {error:#}"),
                Err(error) => self.status = format!("Radio search failed: {error}"),
            }
        }
    }

    fn play_radio_station(&mut self) {
        let Some(station) = self
            .radio_state
            .selected()
            .and_then(|index| self.radio_stations.get(index))
            .cloned()
        else {
            return;
        };
        if station.url_resolved.trim().is_empty() {
            self.status = "This station has no playable stream URL".into();
            return;
        }
        let api = self.radio_api.clone();
        let uuid = station.stationuuid.clone();
        tokio::spawn(async move { api.count_click(&uuid).await });
        self.queue.upcoming.clear();
        self.queue_marks.clear();
        self.result_marks.clear();
        self.queue.current = None;
        self.play(Track {
            id: format!("radio:{}", station.url_resolved),
            title: station.name,
            artist: if station.country.is_empty() {
                "Internet radio".into()
            } else {
                station.country
            },
            album: station.tags,
            duration: String::new(),
        });
    }

    fn search(&mut self) {
        let query = self.input.trim().to_owned();
        if query.is_empty() {
            return;
        }
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
        self.query = query.clone();
        self.editing = false;
        self.queue_focused = false;
        self.library_focused = false;
        self.home_focused = false;
        self.explore_focused = false;
        self.radio_focused = false;
        self.content_detail = false;
        self.searching = true;
        self.search_detail = false;
        self.results.clear();
        self.search_items.clear();
        self.search_continuation = None;
        self.search_appending = false;
        self.result_marks.clear();
        self.results_state.select(None);
        self.status.clear();
        let api = self.api.clone();
        let filter = self.search_filter;
        self.search_task = Some(tokio::spawn(
            async move { api.search(&query, filter).await },
        ));
    }

    fn load_more_search(&mut self) {
        let Some(token) = self.search_continuation.clone() else {
            self.status = "No more search results".into();
            return;
        };
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
        self.searching = true;
        self.search_appending = true;
        let api = self.api.clone();
        let filter = self.search_filter;
        self.search_task = Some(tokio::spawn(async move {
            api.search_more(&token, filter).await
        }));
    }

    fn open_search_item(&mut self) {
        let Some(item) = self
            .results_state
            .selected()
            .and_then(|index| self.search_items.get(index))
            .cloned()
        else {
            return;
        };
        self.library_focused = true;
        self.library_detail = true;
        self.search_detail = true;
        self.library_loading = true;
        self.library_detail_continuation = None;
        self.detail_appending = false;
        self.results.clear();
        self.result_marks.clear();
        let api = self.api.clone();
        self.detail_task = Some(tokio::spawn(async move { api.library_tracks(&item).await }));
    }

    fn load_library(&mut self, kind: LibraryKind) {
        if let Some(task) = self.library_task.take() {
            task.abort();
        }
        if let Some(task) = self.discovery_task.take() {
            task.abort();
        }
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        self.queue_focused = false;
        self.library_focused = true;
        self.home_focused = false;
        self.explore_focused = false;
        self.radio_focused = false;
        self.content_detail = false;
        self.library_kind = kind;
        self.library_detail = false;
        self.search_detail = false;
        self.library_items.clear();
        self.library_continuation = None;
        self.library_state.select(None);
        self.library_loading = true;
        self.status.clear();
        self.result_marks.clear();
        let api = self.api.clone();
        self.library_appending = false;
        self.library_task = Some(tokio::spawn(async move { api.library(kind).await }));
    }

    fn load_more_library(&mut self) {
        let Some(token) = self.library_continuation.clone() else {
            self.status = "No more items in this collection".into();
            return;
        };
        if let Some(task) = self.library_task.take() {
            task.abort();
        }
        self.library_loading = true;
        self.library_appending = true;
        let api = self.api.clone();
        self.library_task = Some(tokio::spawn(async move { api.library_more(&token).await }));
    }

    fn open_library_item(&mut self) {
        let Some(item) = self
            .library_state
            .selected()
            .and_then(|i| self.library_items.get(i))
            .cloned()
        else {
            return;
        };
        self.library_loading = true;
        self.search_detail = false;
        self.library_detail_continuation = None;
        self.detail_appending = false;
        self.status.clear();
        let api = self.api.clone();
        self.detail_task = Some(tokio::spawn(async move { api.library_tracks(&item).await }));
    }

    fn load_more_library_tracks(&mut self) {
        let Some(token) = self.library_detail_continuation.clone() else {
            self.status = "No more tracks in this collection".into();
            return;
        };
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        self.library_loading = true;
        self.detail_appending = true;
        let api = self.api.clone();
        self.detail_task = Some(tokio::spawn(async move {
            api.library_tracks_more(&token).await
        }));
    }

    fn load_discovery(&mut self, explore: bool) {
        if let Some(task) = self.discovery_task.take() {
            task.abort();
        }
        if let Some(task) = self.library_task.take() {
            task.abort();
        }
        if let Some(task) = self.detail_task.take() {
            task.abort();
        }
        self.queue_focused = false;
        self.library_focused = false;
        self.home_focused = !explore;
        self.explore_focused = explore;
        self.radio_focused = false;
        self.library_detail = false;
        self.content_detail = false;
        if let Some(page) = cache::load_discovery(explore) {
            self.discovery_items = page.items;
            self.discovery_continuations = page.continuations;
            self.discovery_state
                .select((!self.discovery_items.is_empty()).then_some(0));
        } else {
            self.discovery_items.clear();
            self.discovery_continuations.clear();
            self.discovery_state.select(None);
        }
        self.discovery_loading = true;
        self.result_marks.clear();
        self.status.clear();
        let api = self.api.clone();
        self.discovery_appending = false;
        self.discovery_append_section = None;
        self.discovery_task = Some(tokio::spawn(async move {
            let page = api.discover(explore).await?;
            cache::store_discovery(explore, &page);
            Ok(page)
        }));
    }

    fn load_more_discovery(&mut self) {
        let Some(section) = self
            .discovery_state
            .selected()
            .and_then(|index| self.discovery_items.get(index))
            .map(|item| item.section.clone())
        else {
            return;
        };
        let Some(continuation) = self
            .discovery_continuations
            .iter()
            .find(|continuation| continuation.section == section)
            .cloned()
        else {
            self.status = format!("No more items in {section}");
            return;
        };
        if let Some(task) = self.discovery_task.take() {
            task.abort();
        }
        self.discovery_loading = true;
        self.discovery_appending = true;
        self.discovery_append_section = Some(continuation.section.clone());
        let api = self.api.clone();
        self.discovery_task = Some(tokio::spawn(async move {
            api.discover_more(&continuation.token, &continuation.section)
                .await
        }));
    }

    fn open_discovery_item(&mut self) {
        let Some(item) = self
            .discovery_state
            .selected()
            .and_then(|i| self.discovery_items.get(i))
            .cloned()
        else {
            return;
        };
        if let Some(track) = item.track {
            self.results = vec![track];
            self.results_state.select(Some(0));
            self.result_marks.clear();
            self.content_detail = true;
            return;
        }
        self.library_loading = true;
        self.status.clear();
        let api = self.api.clone();
        self.detail_task = Some(tokio::spawn(async move { api.library_tracks(&item).await }));
    }

    async fn poll_discovery(&mut self) {
        if self
            .discovery_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.discovery_loading = false;
            match self.discovery_task.take().unwrap().await {
                Ok(Ok(page)) => {
                    if self.discovery_appending {
                        let section = self.discovery_append_section.take().unwrap_or_default();
                        let mut ids = self
                            .discovery_items
                            .iter()
                            .map(|item| {
                                if item.playlist_id.is_empty() {
                                    item.browse_id.clone()
                                } else {
                                    item.playlist_id.clone()
                                }
                            })
                            .collect::<std::collections::HashSet<String>>();
                        self.discovery_items
                            .extend(page.items.into_iter().filter(|item| {
                                let id = if item.playlist_id.is_empty() {
                                    item.browse_id.clone()
                                } else {
                                    item.playlist_id.clone()
                                };
                                !id.is_empty() && ids.insert(id)
                            }));
                        self.discovery_continuations
                            .retain(|continuation| continuation.section != section);
                        self.discovery_continuations.extend(page.continuations);
                    } else {
                        self.discovery_items = page.items;
                        self.discovery_continuations = page.continuations;
                    }
                    self.discovery_appending = false;
                    self.discovery_state
                        .select((!self.discovery_items.is_empty()).then_some(0));
                }
                Ok(Err(e)) => self.status = format!("Could not load view: {e:#}"),
                Err(e) => self.status = format!("View task failed: {e}"),
            }
        }
    }

    async fn poll_library(&mut self) {
        if self
            .library_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.library_loading = false;
            match self.library_task.take().unwrap().await {
                Ok(Ok(page)) => {
                    if self.library_appending {
                        let mut ids = self
                            .library_items
                            .iter()
                            .map(library_item_key)
                            .collect::<std::collections::HashSet<_>>();
                        self.library_items.extend(
                            page.items
                                .into_iter()
                                .filter(|item| ids.insert(library_item_key(item))),
                        );
                    } else {
                        self.library_items = page.items;
                    }
                    self.library_continuation = page.continuation;
                    self.library_appending = false;
                    self.library_state
                        .select((!self.library_items.is_empty()).then_some(0));
                }
                Ok(Err(e)) => self.status = format!("Library failed: {e:#}"),
                Err(e) => self.status = format!("Library task failed: {e}"),
            }
        }
        if self
            .detail_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.library_loading = false;
            match self.detail_task.take().unwrap().await {
                Ok(Ok(page)) => {
                    if self.detail_appending {
                        let mut ids = self
                            .results
                            .iter()
                            .map(|track| track.id.clone())
                            .collect::<std::collections::HashSet<_>>();
                        self.results.extend(
                            page.tracks
                                .into_iter()
                                .filter(|track| ids.insert(track.id.clone())),
                        );
                    } else {
                        self.results = page.tracks;
                    }
                    self.library_detail_continuation = page.continuation;
                    self.detail_appending = false;
                    self.results_state
                        .select((!self.results.is_empty()).then_some(0));
                    self.result_marks.clear();
                    if self.home_focused || self.explore_focused {
                        self.content_detail = true;
                    } else {
                        self.library_detail = true;
                    }
                }
                Ok(Err(e)) => self.status = format!("Could not open item: {e:#}"),
                Err(e) => self.status = format!("Library task failed: {e}"),
            }
        }
    }

    fn play(&mut self, track: Track) {
        self.generation += 1;
        self.position = 0.0;
        self.history_reported = false;
        self.lastfm_reported = false;
        self.lastfm_started_at = None;
        self.repeat_history.clear();
        self.duration = 0.0;
        self.playback = Playback::Loading;
        self.audio_bands.clear();
        self.audio_waveform.clear();
        self.audio_scope.clear();
        self.audio_rms = 0.0;
        self.audio_available = false;
        self.audio_capture_error = None;
        self.spectrogram_history.clear();
        self.status.clear();
        self.player
            .play(self.generation, track.id.clone(), self.volume);
        self.queue.current = Some(track);
        self.preload_next();
        if self.now_playing_view {
            self.request_cover();
            if self.now_panel == NowPanel::Lyrics {
                self.request_lyrics();
            }
        }
    }

    fn open_now_playing(&mut self) {
        self.now_playing_view = true;
        self.now_panel = NowPanel::Queue;
        if !self.queue.upcoming.is_empty() && self.queue_state.selected().is_none() {
            self.queue_state.select(Some(0));
        }
        self.request_cover();
    }

    fn request_lyrics(&mut self) {
        if let Some(task) = self.lyrics_task.take() {
            task.abort();
        }
        self.lyrics = None;
        self.lyrics_error = None;
        self.lyrics_loading = false;
        self.lyrics_plain = false;
        self.lyrics_scroll = 0;
        let Some(track) = self.queue.current.as_ref() else {
            return;
        };
        if track.id.starts_with("radio:") {
            return;
        }
        let (title, artist, album, duration) = (
            track.title.clone(),
            track.artist.clone(),
            track.album.clone(),
            if self.duration > 0.0 {
                self.duration
            } else {
                track
                    .duration
                    .split(':')
                    .try_fold(0.0, |total, part| {
                        part.parse::<f64>()
                            .ok()
                            .map(|seconds| total * 60.0 + seconds)
                    })
                    .unwrap_or_default()
            },
        );
        let generation = self.generation;
        self.lyrics_loading = true;
        self.lyrics_task = Some(tokio::spawn(async move {
            (
                generation,
                crate::lyrics::fetch(&title, &artist, &album, duration).await,
            )
        }));
    }

    async fn poll_lyrics(&mut self) {
        if self
            .lyrics_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.lyrics_task.take()
        {
            match task.await {
                Ok((generation, Ok(lyrics))) if generation == self.generation => {
                    self.lyrics = lyrics;
                }
                Ok((generation, Err(error))) if generation == self.generation => {
                    self.lyrics_error = Some(format!("{error:#}"));
                }
                _ => {}
            }
            self.lyrics_loading = false;
        }
    }

    fn request_cover(&mut self) {
        if let Some(task) = self.cover_task.take() {
            task.abort();
        }
        self.cover_art = None;
        self.cover_loading = false;
        let Some(track) = self.queue.current.as_ref() else {
            return;
        };
        if track.id.starts_with("radio:") {
            return;
        }
        self.cover_loading = true;
        let video_id = track.id.clone();
        let generation = self.generation;
        self.cover_task = Some(tokio::spawn(async move {
            let cover = async {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(8))
                    .build()
                    .ok()?;
                let mut fallback = None;
                for quality in ["maxresdefault", "hqdefault"] {
                    let response = match client
                        .get(format!("https://i.ytimg.com/vi/{video_id}/{quality}.jpg"))
                        .send()
                        .await
                    {
                        Ok(response) => response,
                        Err(_) => continue,
                    };
                    let response = match response.error_for_status() {
                        Ok(response) => response,
                        Err(_) => continue,
                    };
                    let bytes = match response.bytes().await {
                        Ok(bytes) => bytes,
                        Err(_) => continue,
                    };
                    let image = match image::load_from_memory(&bytes) {
                        Ok(image) => image,
                        Err(_) => continue,
                    };
                    if image.width() > 120 || quality == "hqdefault" {
                        return Some(CoverArt {
                            image,
                            protocol: None,
                            protocol_area: None,
                        });
                    }
                    fallback = Some(image);
                }
                fallback.map(|image| CoverArt {
                    image,
                    protocol: None,
                    protocol_area: None,
                })
            }
            .await;
            (generation, cover)
        }));
    }

    async fn poll_cover(&mut self) {
        if self
            .cover_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.cover_task.take()
            && let Ok((generation, cover)) = task.await
        {
            if generation == self.generation {
                self.cover_art = cover;
            }
            self.cover_loading = false;
        }
    }

    fn next(&mut self) {
        self.queue_marks = self
            .queue_marks
            .iter()
            .filter_map(|i| i.checked_sub(1))
            .collect();
        if let Some(track) = self.queue.advance() {
            self.play(track);
        } else {
            self.generation += 1;
            self.player.stop();
            self.playback = Playback::Idle;
            self.position = 0.0;
            self.duration = 0.0;
            self.status.clear();
            self.cover_art = None;
            self.cover_loading = false;
            if let Some(task) = self.cover_task.take() {
                task.abort();
            }
        }
    }

    /// Advances only Dymus's queue after mpv has already moved onto its
    /// preloaded playlist item. Starting another Player here would tear down
    /// mpv and reintroduce the gap we just avoided.
    fn advance_from_player(&mut self) {
        self.queue_marks = self
            .queue_marks
            .iter()
            .filter_map(|i| i.checked_sub(1))
            .collect();
        if self.repeat_mode == RepeatMode::Queue
            && let Some(current) = self.queue.current.clone()
        {
            self.repeat_history.push(current);
            if self.queue.upcoming.is_empty() {
                self.queue
                    .upcoming
                    .extend(std::mem::take(&mut self.repeat_history));
                if let Some(track) = self.queue.advance() {
                    self.play(track);
                    return;
                }
            }
        }
        if self.queue.advance().is_some() {
            self.position = 0.0;
            self.history_reported = false;
            self.lastfm_reported = false;
            self.lastfm_started_at = None;
            self.duration = 0.0;
            self.playback = Playback::Loading;
            self.audio_bands.clear();
            self.audio_waveform.clear();
            self.audio_scope.clear();
            self.audio_rms = 0.0;
            self.audio_available = false;
            self.audio_capture_error = None;
            self.spectrogram_history.clear();
            self.status.clear();
            self.preload_next();
            if self.now_playing_view {
                self.request_cover();
                if self.now_panel == NowPanel::Lyrics {
                    self.request_lyrics();
                }
            }
        } else {
            self.player.stop();
            self.playback = Playback::Idle;
            self.position = 0.0;
            self.duration = 0.0;
            self.status.clear();
            self.cover_art = None;
            self.cover_loading = false;
            if let Some(task) = self.cover_task.take() {
                task.abort();
            }
        }
    }

    fn preload_next(&mut self) {
        if let Some(track) = self.queue.upcoming.front() {
            self.player.preload(self.generation, track.id.clone());
        }
    }

    fn on_player_event(&mut self, generation: u64, event: Event) {
        // A cancelled lookup or old mpv process must never advance the new queue.
        if generation != self.generation {
            return;
        }
        match event {
            Event::Loaded => {
                self.playback = Playback::Playing;
                self.start_lastfm();
            }
            Event::Position(position) => {
                self.position = position;
                self.maybe_report_history();
                self.maybe_scrobble_lastfm();
            }
            Event::Duration(duration) => self.duration = duration,
            Event::AudioFrame(frame) => {
                if self.audio_bands.len() == frame.bands.len() {
                    for (current, target) in self.audio_bands.iter_mut().zip(frame.bands) {
                        let response = if target > *current { 0.72 } else { 0.34 };
                        *current += (target - *current) * response;
                    }
                } else {
                    self.audio_bands = frame.bands;
                }
                self.audio_waveform = frame.waveform;
                self.audio_scope = frame.scope;
                let response = if frame.rms > self.audio_rms {
                    0.72
                } else {
                    0.34
                };
                self.audio_rms += (frame.rms - self.audio_rms) * response;
                self.audio_available = true;
                self.audio_capture_error = None;
                self.spectrogram_history.push_back(self.audio_bands.clone());
                while self.spectrogram_history.len() > 24 {
                    self.spectrogram_history.pop_front();
                }
            }
            Event::AudioUnavailable(error) => {
                self.audio_available = false;
                self.audio_capture_error = Some(error);
            }
            Event::Paused(paused)
                if matches!(self.playback, Playback::Playing | Playback::Paused) =>
            {
                self.playback = if paused {
                    Playback::Paused
                } else {
                    Playback::Playing
                };
            }
            Event::Paused(_) => {}
            Event::Ended => self.advance_from_player(),
            Event::Notice(message) => self.status = message,
            Event::Error(error) => {
                self.playback = Playback::Failed;
                let lower = error.to_ascii_lowercase();
                self.status = if lower.contains("sign in to confirm")
                    || lower.contains("not a bot")
                    || lower.contains("authentication")
                {
                    "YouTube requires a fresh signed-in session — visit YouTube Music in your browser, then run `dymus auth paste` · r to retry, n to skip".into()
                } else {
                    format!("{error} · r to retry, n to skip")
                };
            }
        }
    }

    fn maybe_report_history(&mut self) {
        if self.history_reported
            || !self.config.report_history
            || self.position < self.config.report_history_after_seconds as f64
            || !self.api.is_authenticated()
        {
            return;
        }
        let Some(track) = &self.queue.current else {
            return;
        };
        self.history_reported = true;
        let api = self.api.clone();
        let video_id = track.id.clone();
        tokio::spawn(async move {
            // History is supplemental: reporting failure must never interrupt playback.
            if let Err(error) = api.add_history_item(&video_id).await {
                eprintln!("Could not report playback history: {error:#}");
            }
        });
    }

    fn start_lastfm(&mut self) {
        if !self.config.lastfm_scrobbling
            || self
                .queue
                .current
                .as_ref()
                .is_none_or(|track| track.id.starts_with("radio:"))
        {
            return;
        }
        let Some(credentials) = crate::auth::load_lastfm().ok().flatten() else {
            return;
        };
        let Ok(client) = crate::lastfm::Client::new(credentials) else {
            return;
        };
        let track = self.queue.current.clone().expect("checked above");
        let duration = self
            .duration
            .max(crate::lastfm::duration_seconds(&track) as f64) as u64;
        self.lastfm_started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
        tokio::spawn(async move {
            if let Err(error) = client.now_playing(&track, duration).await {
                eprintln!("Could not update Last.fm now playing: {error:#}");
            }
        });
    }

    fn maybe_scrobble_lastfm(&mut self) {
        if self.lastfm_reported || !self.config.lastfm_scrobbling {
            return;
        }
        let Some(track) = self
            .queue
            .current
            .clone()
            .filter(|track| !track.id.starts_with("radio:"))
        else {
            return;
        };
        let duration = self
            .duration
            .max(crate::lastfm::duration_seconds(&track) as f64) as u64;
        let Some(eligible_after) = crate::lastfm::eligible_after(duration) else {
            return;
        };
        if self.position < eligible_after as f64 {
            return;
        }
        let Some(started_at) = self.lastfm_started_at else {
            return;
        };
        let Some(credentials) = crate::auth::load_lastfm().ok().flatten() else {
            return;
        };
        let Ok(client) = crate::lastfm::Client::new(credentials) else {
            return;
        };
        self.lastfm_reported = true;
        tokio::spawn(async move {
            if let Err(error) = client.scrobble(&track, duration, started_at).await {
                eprintln!("Could not scrobble to Last.fm: {error:#}");
            }
        });
    }

    fn set_volume(&mut self, volume: u8) {
        self.volume = volume;
        self.player
            .command(json!(["set_property", "volume", volume]));
    }

    fn move_selection(&mut self, delta: isize) {
        if (self.home_focused || self.explore_focused) && !self.content_detail {
            self.discovery_state.select(moved(
                self.discovery_state.selected(),
                self.discovery_items.len(),
                delta,
            ));
        } else if self.library_focused && !self.library_detail {
            self.library_state.select(moved(
                self.library_state.selected(),
                self.library_items.len(),
                delta,
            ));
        } else if self.queue_context() {
            let index = moved(
                self.queue_state.selected(),
                self.queue.upcoming.len(),
                delta,
            );
            self.queue_state.select(index);
        } else {
            let index = moved(self.results_state.selected(), self.results.len(), delta);
            self.results_state.select(index);
        }
    }

    fn select_edge(&mut self, end: bool) {
        if (self.home_focused || self.explore_focused) && !self.content_detail {
            self.discovery_state
                .select(if self.discovery_items.is_empty() {
                    None
                } else {
                    Some(if end {
                        self.discovery_items.len() - 1
                    } else {
                        0
                    })
                });
            return;
        }
        if self.library_focused && !self.library_detail {
            self.library_state.select(if self.library_items.is_empty() {
                None
            } else {
                Some(if end { self.library_items.len() - 1 } else { 0 })
            });
            return;
        }
        let len = if self.queue_context() {
            self.queue.upcoming.len()
        } else {
            self.results.len()
        };
        let index = if len == 0 {
            None
        } else {
            Some(if end { len - 1 } else { 0 })
        };
        if self.queue_context() {
            self.queue_state.select(index);
        } else {
            self.results_state.select(index);
        }
    }

    fn clamp_queue_selection(&mut self) {
        self.queue_state.select(moved(
            self.queue_state.selected(),
            self.queue.upcoming.len(),
            0,
        ));
    }

    pub async fn shutdown(&mut self) {
        let mut saved_queue = Vec::new();
        if matches!(
            self.playback,
            Playback::Playing | Playback::Paused | Playback::Loading
        ) && let Some(current) = self.queue.current.clone()
        {
            saved_queue.push(current);
        }
        saved_queue.extend(self.queue.upcoming.iter().cloned());
        cache::store_queue(&saved_queue);
        if let Some(task) = self.search_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.cancel_radio();
        if let Some(task) = self.library_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.detail_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.discovery_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.cover_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.lyrics_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.player.shutdown().await;
    }
}

fn binding_matches(binding: Option<&str>, key: KeyEvent) -> bool {
    let Some(binding) = binding else {
        return false;
    };
    let normalized = binding.to_ascii_lowercase();
    match normalized.as_str() {
        "space" => key.code == KeyCode::Char(' '),
        "enter" => key.code == KeyCode::Enter,
        "esc" | "escape" => key.code == KeyCode::Esc,
        "tab" => key.code == KeyCode::Tab,
        "backtab" => key.code == KeyCode::BackTab,
        "up" => key.code == KeyCode::Up,
        "down" => key.code == KeyCode::Down,
        "left" => key.code == KeyCode::Left,
        "right" => key.code == KeyCode::Right,
        "ctrl+u" => key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL),
        value if value.starts_with("ctrl+") => value.chars().last().is_some_and(|ch| {
            key.code == KeyCode::Char(ch) && key.modifiers.contains(KeyModifiers::CONTROL)
        }),
        _ if binding.chars().count() == 1 => binding
            .chars()
            .next()
            .is_some_and(|ch| key.code == KeyCode::Char(ch)),
        _ => false,
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.cancel_radio();
        if let Some(task) = &self.search_task {
            task.abort();
        }
        if let Some(task) = &self.library_task {
            task.abort();
        }
        if let Some(task) = &self.detail_task {
            task.abort();
        }
        if let Some(task) = &self.cover_task {
            task.abort();
        }
        if let Some(task) = &self.discovery_task {
            task.abort();
        }
        if let Some(task) = &self.lyrics_task {
            task.abort();
        }
    }
}

fn moved(selected: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        None
    } else {
        Some(
            selected
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(len - 1),
        )
    }
}

fn library_item_key(item: &LibraryItem) -> String {
    if let Some(track) = &item.track {
        track.id.clone()
    } else if !item.playlist_id.is_empty() {
        item.playlist_id.clone()
    } else if !item.browse_id.is_empty() {
        item.browse_id.clone()
    } else {
        item.title.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::track;

    async fn populated_app() -> App {
        let mut app = App::new().await.unwrap();
        app.editing = false;
        app.results = vec![track("a"), track("b"), track("c")];
        app.results_state.select(Some(1));
        app.queue.current = Some(track("playing"));
        app.queue.upcoming.push_back(track("old"));
        app.playback = Playback::Playing;
        app
    }

    #[tokio::test]
    async fn now_playing_panel_switches_between_queue_visualizer_and_lyrics() {
        let mut app = populated_app().await;
        app.open_now_playing();
        assert!(app.now_playing_view);
        assert_eq!(app.now_panel, NowPanel::Queue);
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(app.now_panel, NowPanel::Visualizer);
        assert_eq!(app.visualizer_mode, VisualizerMode::Spectrum);
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(app.visualizer_mode, VisualizerMode::Waveform);
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.visualizer_mode, VisualizerMode::Spectrum);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.now_panel, NowPanel::Lyrics);
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(app.now_panel, NowPanel::Queue);
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.now_panel, NowPanel::Lyrics);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.now_panel, NowPanel::Queue);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.now_playing_view);
        assert!(!app.cover_loading);
    }

    #[tokio::test]
    async fn configured_shortcuts_open_settings_and_search() {
        let mut app = populated_app().await;
        app.config.keybindings.settings = ";".into();
        app.handle_key(KeyEvent::new(KeyCode::Char(';'), KeyModifiers::NONE));
        assert!(app.settings_view);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.settings_view);

        app.handle_key(KeyEvent::new(KeyCode::Char(';'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.config.theme, "catppuccin-mocha");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.config.start_view, "explore");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        app.config.keybindings.search = "f".into();
        app.query = "saved query".into();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(app.editing);
        assert_eq!(app.input, "saved query");
    }

    #[tokio::test]
    async fn library_continuation_starts_an_append_request() {
        let mut app = populated_app().await;
        app.library_focused = true;
        app.library_items = vec![LibraryItem {
            section: String::new(),
            title: "Album".into(),
            detail: String::new(),
            browse_id: "MPREalbum".into(),
            playlist_id: String::new(),
            track: None,
        }];
        app.library_state.select(Some(0));
        app.library_continuation = Some("next-library-page".into());
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert!(app.library_loading);
        assert!(app.library_appending);
        if let Some(task) = app.library_task.take() {
            task.abort();
        }
    }

    #[tokio::test]
    async fn search_filter_cycles_and_restarts_the_current_query() {
        let mut app = populated_app().await;
        app.query = "ambient".into();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.search_filter, SearchFilter::Artists);
        assert!(app.searching);
        if let Some(task) = app.search_task.take() {
            task.abort();
        }
    }

    #[tokio::test]
    async fn startup_uses_home_by_default_or_the_configured_view() {
        let mut app = App::new().await.unwrap();
        app.start();
        assert!(app.home_focused);
        assert!(!app.editing);

        let mut app = App::new().await.unwrap();
        app.config.start_view = "queue".into();
        app.start();
        assert!(app.queue_focused);
        assert!(!app.editing);

        let mut app = App::new().await.unwrap();
        app.config.start_view = "podcasts".into();
        app.start();
        assert!(app.library_focused);
        assert_eq!(app.library_kind, LibraryKind::Podcasts);
    }

    #[tokio::test]
    async fn number_shortcuts_open_four_independent_library_views() {
        let mut app = populated_app().await;
        for (key, kind) in [
            ('1', LibraryKind::Playlists),
            ('2', LibraryKind::Albums),
            ('3', LibraryKind::Artists),
            ('4', LibraryKind::Podcasts),
        ] {
            app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            assert!(app.library_focused);
            assert_eq!(app.library_kind, kind);
        }
    }

    #[tokio::test]
    async fn radio_filters_and_station_selection_are_independent_from_music_search() {
        let mut app = populated_app().await;
        app.radio_focused = true;
        app.radio_stations = vec![crate::radio::Station {
            stationuuid: "station".into(),
            name: "Global Jazz".into(),
            url_resolved: "https://radio.example/live".into(),
            country: "Kenya".into(),
            language: "English".into(),
            tags: "jazz".into(),
            codec: "MP3".into(),
            bitrate: 128,
            ..Default::default()
        }];
        app.radio_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.radio_editing);
        assert_eq!(app.radio_filter_field, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.radio_filter.country, "K");
        assert!(app.radio_loading);
        if let Some(task) = app.stations_task.take() {
            task.abort();
        }
        app.radio_loading = false;

        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.radio_sort_label(), "trending");
        if let Some(task) = app.stations_task.take() {
            task.abort();
        }
    }

    #[tokio::test]
    async fn radio_destination_loads_radio_browser_and_clears_filters() {
        let mut app = populated_app().await;
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert!(app.radio_focused);
        assert!(app.radio_loading);
        if let Some(task) = app.stations_task.take() {
            task.abort();
        }
        app.radio_loading = false;

        app.radio_filter.country = "kenya".into();
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.radio_filter.country.is_empty());
        assert!(app.radio_loading);
        if let Some(task) = app.stations_task.take() {
            task.abort();
        }
    }

    #[tokio::test]
    async fn lyrics_panel_toggles_formats_and_scrolls_plain_text() {
        let mut app = populated_app().await;
        app.now_playing_view = true;
        app.now_panel = NowPanel::Lyrics;
        app.lyrics = Some(
            serde_json::from_str(r#"{"plainLyrics":"plain","syncedLyrics":"[00:01.00]synced"}"#)
                .unwrap(),
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(app.lyrics_plain);
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.lyrics_scroll, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(!app.lyrics_plain);
        assert_eq!(app.lyrics_scroll, 0);
    }

    #[tokio::test]
    async fn audio_frames_feed_live_visualizer_state() {
        let mut app = populated_app().await;
        let frame = crate::visualizer::AudioFrame {
            bands: vec![0.5; 48],
            waveform: vec![0.25; 256],
            scope: vec![(0.1, -0.1); 192],
            rms: 0.4,
        };
        app.on_player_event(0, Event::AudioFrame(frame));
        assert!(app.audio_available);
        assert_eq!(app.audio_bands.len(), 48);
        assert_eq!(app.audio_waveform.len(), 256);
        assert_eq!(app.audio_scope.len(), 192);
        assert!((app.audio_rms - 0.288).abs() < 0.001);
        assert_eq!(app.spectrogram_history.len(), 1);
    }

    #[tokio::test]
    async fn now_playing_queue_panel_moves_and_removes_upcoming_tracks() {
        let mut app = populated_app().await;
        app.open_now_playing();
        app.queue.upcoming = [track("first"), track("second")].into();
        app.queue_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.queue_state.selected(), Some(1));
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.queue.upcoming, [track("first")]);
        assert_eq!(app.queue.current, Some(track("playing")));
    }

    #[tokio::test]
    async fn bulk_play_next_preserves_order_and_existing_queue_marks() {
        let mut app = populated_app().await;
        app.result_marks = BTreeSet::from([2, 0]);
        app.queue_marks.insert(0);
        app.execute(Action::PlayNext);
        assert_eq!(app.queue.upcoming, [track("a"), track("c"), track("old")]);
        assert_eq!(app.queue_marks, BTreeSet::from([2]));
        assert_eq!(app.queue.current, Some(track("playing")));
        assert!(app.result_marks.is_empty());
    }

    #[tokio::test]
    async fn play_marked_queue_entries_removes_only_selected_instances() {
        let mut app = populated_app().await;
        app.queue_focused = true;
        app.queue.upcoming = [track("a"), track("b"), track("a"), track("c")].into();
        app.queue_marks = BTreeSet::from([1, 2]);
        app.execute(Action::Play);
        assert_eq!(app.queue.current, Some(track("b")));
        assert_eq!(app.queue.upcoming, [track("a"), track("a"), track("c")]);
        assert!(app.queue_marks.is_empty());
    }

    #[tokio::test]
    async fn play_all_replaces_queue_and_ignores_partial_marks() {
        let mut app = populated_app().await;
        app.result_marks.insert(2);
        app.execute(Action::PlayAll);
        assert_eq!(app.queue.current, Some(track("a")));
        assert_eq!(app.queue.upcoming, [track("b"), track("c")]);
        assert!(app.result_marks.is_empty());
    }

    #[tokio::test]
    async fn queue_all_and_bulk_remove_leave_playback_untouched() {
        let mut app = populated_app().await;
        app.execute(Action::QueueAll);
        assert_eq!(
            app.queue.upcoming,
            [track("old"), track("a"), track("b"), track("c")]
        );
        app.queue_focused = true;
        app.queue_marks = BTreeSet::from([0, 2]);
        app.execute(Action::Remove);
        assert_eq!(app.queue.upcoming, [track("a"), track("c")]);
        app.execute(Action::ClearQueue);
        assert!(app.queue.upcoming.is_empty());
        assert_eq!(app.queue.current, Some(track("playing")));
        assert_eq!(app.playback, Playback::Playing);
    }

    #[tokio::test]
    async fn selection_is_independent_between_views_and_tracks_queue_advancement() {
        let mut app = populated_app().await;
        app.execute(Action::SelectAll);
        assert_eq!(app.result_marks, BTreeSet::from([0, 1, 2]));
        app.execute(Action::SelectAll);
        assert!(app.result_marks.is_empty());
        app.toggle_mark();
        app.queue_focused = true;
        app.queue.upcoming = [track("a"), track("b"), track("c")].into();
        app.queue_marks = BTreeSet::from([0, 2]);
        app.on_player_event(0, Event::Ended);
        assert_eq!(app.queue_marks, BTreeSet::from([1]));
        assert_eq!(app.queue.upcoming[1], track("c"));
        assert_eq!(app.result_marks, BTreeSet::from([1]));
    }

    #[tokio::test]
    async fn radio_failure_preserves_queue_and_playback() {
        let mut app = populated_app().await;
        app.radio_task = Some(tokio::spawn(async { anyhow::bail!("network unavailable") }));
        tokio::task::yield_now().await;
        app.poll_radio().await;
        assert!(!app.radio_loading());
        assert!(app.status.contains("network unavailable"));
        assert_eq!(app.queue.current, Some(track("playing")));
        assert_eq!(app.queue.upcoming, [track("old")]);
        assert_eq!(app.playback, Playback::Playing);
    }

    #[tokio::test]
    async fn manual_queue_change_discards_pending_radio_even_if_already_finished() {
        let mut app = populated_app().await;
        app.radio_task = Some(tokio::spawn(async {
            Ok(vec![track("radio"), track("related")])
        }));
        tokio::task::yield_now().await;
        app.execute(Action::Add);
        app.poll_radio().await;
        assert_eq!(app.queue.current, Some(track("playing")));
        assert_eq!(app.queue.upcoming, [track("old"), track("b")]);
        assert!(!app.radio_loading());
    }

    #[tokio::test]
    async fn radio_success_replaces_queue_only_after_completion() {
        let mut app = populated_app().await;
        app.radio_task = Some(tokio::spawn(async {
            Ok(vec![track("seed"), track("related")])
        }));
        assert_eq!(app.queue.upcoming, [track("old")]);
        tokio::task::yield_now().await;
        app.poll_radio().await;
        assert_eq!(app.queue.current, Some(track("seed")));
        assert_eq!(app.queue.upcoming, [track("related")]);
    }

    #[tokio::test]
    async fn action_menu_and_typing_do_not_trigger_unintended_playback() {
        let mut app = populated_app().await;
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE));
        assert!(app.menu);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.menu);
        assert_eq!(app.queue.current, Some(track("playing")));
        app.editing = true;
        for c in "xvPQR.".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input, "xvPQR.");
        assert!(!app.radio_loading());
        assert!(app.result_marks.is_empty());
    }

    #[tokio::test]
    async fn stale_playback_events_do_not_change_current_track_or_queue() {
        let mut app = App::new().await.unwrap();
        app.generation = 2;
        app.queue.current = Some(track("current"));
        app.queue.upcoming.push_back(track("next"));
        app.playback = Playback::Playing;
        app.on_player_event(1, Event::Ended);
        app.on_player_event(1, Event::Error("old failure".into()));
        assert_eq!(app.queue.current, Some(track("current")));
        assert_eq!(app.queue.upcoming.len(), 1);
        assert_eq!(app.playback, Playback::Playing);
    }

    #[tokio::test]
    async fn typing_shortcuts_edits_search_without_triggering_actions() {
        let mut app = App::new().await.unwrap();
        for c in "qna?".chars() {
            assert!(!app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        assert_eq!(app.input, "qna?");
        assert!(!app.help);
        assert_eq!(app.playback, Playback::Idle);
    }

    #[tokio::test]
    async fn eof_on_last_track_returns_to_idle() {
        let mut app = App::new().await.unwrap();
        app.queue.current = Some(track("last"));
        app.playback = Playback::Playing;
        app.on_player_event(0, Event::Ended);
        assert_eq!(app.playback, Playback::Idle);
        assert!(app.queue.current.is_none());
    }

    #[test]
    fn selection_handles_empty_lists_and_removal() {
        assert_eq!(moved(Some(4), 0, 1), None);
        assert_eq!(moved(Some(4), 2, 0), Some(1));
        assert_eq!(moved(Some(0), 2, -1), Some(0));
    }
}
