# Dymus

A keyboard-driven YouTube Music streaming TUI, written in Rust. Dymus uses
InnerTube for song search, yt-dlp to resolve audio streams, and mpv for playback.

Phase 1 is complete: **discover, search, play, queue, and browse a signed-in
library**, with lyrics and audio-reactive visualizers in the Now Playing view.

## Run

Requires Linux, Rust, `mpv`, and `yt-dlp` on your `PATH`. No API key or account is
required for public song search. Playback availability depends on YouTube and
may vary by track and region.

Audio-reactive visualizers also use the PipeWire command-line tools `pw-dump` and
`pw-record`. Playback continues without them, with animated fallback visuals.

```sh
cargo run -- doctor
cargo run
```

For an optimized binary:

```sh
cargo build --release
./target/release/dymus
```

Search without opening the TUI (returns JSON):

```sh
cargo run -- search "Nujabes Feather"
```

## Authentication

Public search and radio work without an account. Add browser-session
authentication to use library and account endpoints:

1. Sign in to YouTube Music in your browser, open Developer Tools → Network,
   and copy the **Cookie request header value** from a successful `browse`
   request. Do not copy the `Cookie:` label.
2. Enter it into Dymus's hidden-input prompt:

   ```sh
   cargo run -- auth paste
   ```

   For another signed-in Google account, use its `X-Goog-AuthUser` index:
   `cargo run -- auth paste --auth-user 1`.
3. Dymus signs and checks an account request before saving. On Linux, it stores
   credentials in `$XDG_CONFIG_HOME/dymus/auth.json` (or
   `~/.config/dymus/auth.json`), restricts the directory to `0700` and file to
   `0600`, and attaches the session only to `music.youtube.com` requests. The
   file is permission-protected but not encrypted. An expired or logged-out
   session is reported by `dymus auth status`; paste a fresh cookie to replace
   it. `dymus auth logout` removes the local copy.

Browser cookies are bearer credentials for your Google session. Never paste
them into an issue, chat, shell command, or source file. If you accidentally
expose one, revoke that Google session and create a fresh cookie before using
Dymus. Dymus does not read your browser profile or print or log cookie values.

## Controls

Dymus starts on Home by default. Choose another startup view in Settings or set
`start_view` in the TOML configuration. Press `/` to focus Search, type a query,
and press Enter. While editing, ordinary keys enter text; press Esc to return
to browsing.

| Key | Action |
| --- | --- |
| `/` | Edit search |
| Enter | Submit search, or play marked songs / the focused song |
| `j` / `k`, arrows | Move selection |
| `g` / `G`, Home / End | First / last item |
| Tab | Switch search results / queue |
| `.` | Open contextual actions (j/k to choose, Enter to apply) |
| `x` | Mark / unmark the focused song |
| `v` | Select / deselect all songs in the current view |
| Esc / `X` | Clear marks (Esc also cancels a pending radio request while browsing) |
| `a` | Append marked search results, or the focused result, to queue |
| `A` | Put marked search results, or the focused result, next in queue |
| `P` | Play all songs in the current view, replacing the upcoming queue |
| `Q` | Append all loaded search results to queue |
| `R` | Start radio from the focused song, replacing the queue after loading |
| `d`, Delete | Remove marked queued songs, or the focused entry |
| `J` / `K` | Move marked queued songs, or the focused entry, down / up |
| `C` | Clear the upcoming queue without interrupting the current song |
| Space | Pause / resume |
| `n` | Skip to next queued song |
| `r` | Restart or retry current song |
| Left / Right | Seek backward / forward five seconds |
| `-` / `+` | Adjust volume |
| `?` | Show keyboard help (j/k scroll on small terminals) |
| `q` | Quit from browsing mode |
| Ctrl+C | Quit from any mode |
| Ctrl+U | Clear the search field while editing |
| `h` / `e` | Open Home / Explore |
| `s` | Open Settings to switch themes and choose a startup view |
| `1`–`4` | Open Playlists / Albums / Artists / Podcasts as top-level views |
| Enter / Esc | Open a collection item / return from its detail |
| `t` | Open Now Playing for the current song |
| `q` / `v` / `y` / Tab | In Now Playing: queue / visualizer / lyrics / cycle panels |
| Shift+Tab | In Now Playing: cycle panels backward |
| `m` / `[` / `]` | In the visualizer: cycle styles / previous / next |
| `p` | In lyrics: switch plain and synced lyrics when both exist |
| `j` / `k` | In plain lyrics: scroll down / up |

Bulk actions use marked songs when there are any, otherwise the focused row.
Marks are independent between search and queue, and follow queued entries when
reordered or when playback advances. Selection markers appear only while a
selection exists; Esc clears them.

Enter plays the first chosen song and puts the rest next, preserving their list
order and any unselected upcoming tracks. Selected queue entries are moved, not
duplicated. `P` replaces the queue with the whole current list, while `Q` appends
all search results. “All” means every loaded result, including rows off-screen;
search pagination is not implemented yet. Completed bulk actions clear their
selection.

`R` always uses the focused song, even when other songs are marked. Dymus fetches
a YouTube Music radio mix through InnerTube, starts with that song, then plays
the returned related tracks. Duplicate and unavailable entries are skipped.
Current playback and the queue remain intact if fetching fails. Esc, a new radio
request, or a manual playback/queue change cancels a pending request, preventing
late results from overwriting your edits. Radio currently loads one generated
mix; it does not fetch an endless stream of recommendations.

Adding to an idle queue does not start playback; use Enter or `n`. Failed tracks
remain selected for `r` to retry or `n` to skip. Exiting stops playback.

The default theme uses [Tokyo Night](https://github.com/folke/tokyonight.nvim)
foreground colors and the terminal's own background, preserving transparency.
The borderless layout shows one view at a time: Search or queue, switched with
Tab. A pointer and blue text indicate the selected song. Playback information
appears only when a track is active; shortcuts are available under `?`.
No special font or terminal image support is needed. Esc dismisses an error
while browsing.

## Configuration

Phase 2 starts with editable appearance and core shortcuts. On first launch,
Dymus creates `$XDG_CONFIG_HOME/dymus/config.toml` (normally
`~/.config/dymus/config.toml`). Press `s` to open Settings. Up/Down selects the
theme or startup view; Left/Right changes it, and the selection is saved
immediately. The default startup view is Home. Choose Home, Explore, Playlists,
Albums, Artists, Podcasts, Search, or Queue. Built-in themes are Tokyo Night,
Catppuccin Mocha, Gruvbox Dark, and Nord. The app keeps the terminal's default
background for transparency.

The TOML file also accepts optional six-digit hex color overrides and custom
keys for settings, search, help, Home, Explore, Playlists, Albums, Artists,
Podcasts, queue, Now Playing, pause, next, retry, volume, and quit. Key values
accept a character or names such as `Tab`, `Space`, `Enter`, `Esc`, `Left`, and
`Ctrl+U`. For example:

```toml
theme = "nord"
start_view = "home"

[colors]
accent = "#88c0d0"
text = "#eceff4"

[keybindings]
settings = ";"
search = "/"
playlists = "1"
albums = "2"
artists = "3"
podcasts = "4"
next_track = "n"
```

For custom colors, remove the leading `#` comment from entries in the generated
file's `[colors]` example. Restart Dymus after editing keybindings or colors.

## Phase 1 features and boundaries

- Search and open public YouTube Music results; search currently loads one page.
- Home, Explore, Search, Queue, and four separate authenticated collection views:
  Playlists, Albums, Artists, and Podcasts.
- Background stream resolution, queue editing and bulk actions, radio, and
  automatic track advancement.
- Playback progress, pause, seeking, volume, cover art, and a Now Playing view.
- Phase 2: TOML configuration, switchable theme presets, color overrides, and
  configurable core navigation and playback shortcuts.
- LRCLIB plain and synced lyrics, plus nine visualizer styles. PipeWire-backed
  visualizers analyze Dymus's own mpv stream; without `pw-dump`, `pw-record`, or
  a reachable PipeWire session, animated fallback visuals are shown.
- Browser credentials are entered privately by the user; Dymus does not read
  browser profiles. No audio or music files are saved by Dymus.
- The queue and search state are in memory only. Search pagination, endless
  radio continuation, shuffle/repeat, MPRIS, and background playback are not
  included in Phase 1. Exiting stops playback.

This version uses Unix sockets and targets Linux.

InnerTube is an unofficial API and can change. Stream extraction depends on a
working yt-dlp installation. Dymus deliberately ignores user yt-dlp/mpv config
so unrelated player options cannot change its behavior. If playback fails, check
`dymus doctor`, update yt-dlp through your package manager, and try another track.
Account-restricted tracks are not supported yet.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# Optional real mpv integration test: uses generated silence and null audio output.
cargo test mpv_reports_playback_and_eof_with_local_audio -- --ignored
# Optional live search → stream test: public query, null audio output, network required.
cargo test live_search_resolve_and_stream_audio -- --ignored --nocapture
# Optional live radio check (fetches tracks without playing audio).
cargo test live_radio_returns_related_tracks -- --ignored --nocapture
```

Unit tests cover bulk queue behavior, radio cancellation and failure recovery,
stale playback events, keyboard modes, search/radio parsing, and terminal layouts. `tests/fixtures/search.json` is a
synthetic InnerTube-shaped response containing duplicate, unavailable, malformed,
and multi-artist entries. `tests/fixtures/radio.json` covers wrapped tracks,
alternate counterparts, duplicates and unavailable tracks. The mpv test needs mpv and access to local Unix sockets;
it does not need the network or an audio device.

Source layout:

- `innertube.rs`: HTTP client and conversion of search renderers into tracks.
- `model.rs`: track and queue data.
- `player.rs`: asynchronous extraction, mpv lifecycle and JSON IPC.
- `visualizer.rs`: PipeWire stream discovery and real-time audio analysis.
- `config.rs`: TOML preferences, theme palettes, and keybindings.
- `app.rs`: actions, search jobs and playback state.
- `ui.rs`: terminal layout and rendering.

Every playback attempt gets a generation ID. Events from cancelled attempts are
ignored, preventing a delayed lookup or old end-of-track event from changing the
current queue. Extraction and audio subprocesses are terminated on cancellation.

## Inspiration

- [ytermusic](https://github.com/ccgauche/ytermusic): simplicity and playlist workflow.
- [youtui](https://github.com/nick42d/youtui): music discovery and keyboard navigation.
- [ytmusic-tui](https://github.com/WakaTaira/ytmusic-tui): browsing and queue interaction.
- [ytmusicapi](https://github.com/sigma67/ytmusicapi): reference for InnerTube request and response structures.

Dymus is not affiliated with or endorsed by YouTube or Google.
