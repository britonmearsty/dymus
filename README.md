# Dymus

Dymus is a keyboard-driven YouTube Music streaming TUI for Linux. It searches
YouTube Music through InnerTube, resolves streams with yt-dlp, and controls mpv
for audio playback. It uses the terminal's own background and defaults to the
Tokyo Night palette.

## Features

- Home and Explore feeds, public search, and a separate playback queue.
- Signed-in Playlists, Albums, Artists, and Podcasts views.
- Bulk playback and queue operations for marked songs.
- Generated YouTube Music radio from any focused song.
- World Radio via Radio Browser, with station, country, language, genre, and
  sorting filters.
- Playback progress, seeking, volume, cover art, and automatic queue advancement.
- A Now Playing view with queue, lyrics, and nine visualizer styles.
- Plain and synced lyrics from LRCLIB.
- Theme presets, color overrides, configurable startup view, and keybindings.

## Install and run

Dymus targets 64-bit Linux and needs `mpv` and `yt-dlp` on `PATH`. Public
search and World Radio do not require an account. Playback availability depends
on the track and region.

PipeWire's `pw-dump` and `pw-record` enable audio-reactive visualizers.
Music playback works without them, using animated fallback visuals.

```sh
curl -fsSL https://raw.githubusercontent.com/britonmearsty/dymus/master/install.sh | sh
```

The installer verifies the release checksum and installs `dymus` to
`~/.local/bin` (or `BIN_DIR` if set). Restart your shell or add that directory
to `PATH`, then run:

```sh
dymus doctor
dymus
```

To install a specific release, set its tag first:

```sh
curl -fsSL https://raw.githubusercontent.com/britonmearsty/dymus/master/install.sh | DYMUS_VERSION=v0.1.0 sh
```

For a development build, install Rust 1.88 or newer and use:

```sh
git clone https://github.com/britonmearsty/dymus.git
cd dymus
cargo run -- doctor
cargo run
```

For an optimized build:

```sh
cargo build --release
./target/release/dymus
```

`dymus doctor` checks the external playback and PipeWire tools. Normal startup
does not wait for those checks. Image-protocol detection uses a short probe and
falls back to halfblocks when the terminal does not respond.

Search can run without opening the TUI and returns JSON:

```sh
cargo run -- search "Nujabes Feather"
```

## Headless playback

Play without opening the TUI by choosing the kind of result to resolve. Song
searches show the first five matching tracks; album and playlist searches show
the first five matching collections. Choose the numbered result to play it.
Set `headless_results` in `~/.config/dymus/config.toml` to show between 1 and
25 matches instead; the default is 5:

```toml
headless_results = 10
```

To add authenticated tracks to YouTube Music history after they have genuinely
played, opt in explicitly (it is off by default):

```toml
report_history = true
report_history_after_seconds = 30
```

The threshold accepts 1 through 3600 seconds. Reporting failures never stop
playback.

## Last.fm scrobbling

Create a Last.fm API application, then run `dymus lastfm login` and approve the
printed authorization URL. Alternatively, `dymus lastfm paste` accepts an
existing API key, shared secret, and session key privately. Dymus verifies the
session before saving it. Every eligible non-radio track updates Last.fm Now Playing
when playback loads and is scrobbled after the service's rule: more than 30
seconds long and played for half its duration or four minutes, whichever comes
first. This is enabled by default whenever credentials exist; set
`lastfm_scrobbling = false` in the configuration to disable it. Use
`dymus lastfm status` to recheck the connection or `dymus lastfm logout` to
remove only those credentials.

```sh
dymus play song "Nujabes Feather"
dymus play album "Modal Soul"
dymus play playlist "Lo-fi beats" --detach --volume 65
dymus play library playlists --detach
```

Without `--detach`, the command stays attached until mpv exits. Detached
playback remains available after the shell command ends and can be controlled
from any terminal in the same user session:

```sh
dymus control status
dymus control toggle
dymus control next
dymus control volume 50
dymus control stop
```

Attached playback keeps one terminal line updated with the active track,
play/pause state, and elapsed time. Use `--detach` when that display is not
needed.

The control socket is local to your user session and only one headless Dymus
player can run at a time. Headless album and playlist playback may need YouTube
Music credentials; configure them with `dymus auth paste` when required.
Only the first selected track is resolved before playback starts. Remaining
album or playlist tracks are resolved and added to mpv in the background.

## How it works

Home, Explore, search, library, and generated radio requests use YouTube Music's
unofficial InnerTube endpoints. A successful Home or Explore feed is stored in
`$XDG_CACHE_HOME/dymus` (usually `~/.cache/dymus`). On the next launch, Dymus
shows that cached feed immediately, refreshes it in the background, and replaces
it when the fresh response arrives.

When playback starts, yt-dlp resolves the selected YouTube URL to an audio
stream. Dymus passes that stream to mpv and monitors mpv through its JSON IPC
socket for progress, pause state, failures, and end-of-track events. Each
attempt receives a generation ID, so late events from a cancelled attempt cannot
change the active queue. Helper processes stop when playback is cancelled or the
application exits.

Cover images use the best terminal image protocol detected at launch. Lyrics are
looked up from LRCLIB using the active track metadata. The visualizer attempts
to isolate Dymus's mpv stream through PipeWire and analyze its audio frames; if
capture is unavailable, it uses its fallback animation.

## Views

Home and Explore contain browseable YouTube Music collections. Press Enter on a
collection to open its tracks and Esc to return. Rows retain their source shelf,
so recommendations, mixes, and quick picks remain visibly separated in the
minimal table. Press `L` on a row to load the next page for that shelf when
YouTube Music provides one. Search contains public tracks; the queue contains
upcoming playback. Playlists, Albums, Artists, and Podcasts are separate
top-level signed-in views.

Now Playing presents the active track, cover art, and a minimal progress bar. Its
right panel switches between queue, visualizer, and lyrics. Synced lyrics
distinguish elapsed, current, and upcoming lines; plain lyrics can be scrolled.

## Authentication

Signing in enables library and account endpoints. Public search, generated
radio, and World Radio work without an account.

1. Sign in to YouTube Music in a browser. In Developer Tools → Network, copy
   the **Cookie request header value** from a successful `browse` request,
   without the `Cookie:` label.
2. Paste it into Dymus's hidden-input prompt:

   ```sh
   cargo run -- auth paste
   ```

   To use another Google account in that browser session, provide its
   `X-Goog-AuthUser` index: `cargo run -- auth paste --auth-user 1`.
3. Dymus signs and validates an account request before saving credentials. Use
   `dymus auth status` to check the saved session and `dymus auth logout` to
   remove it.

Credentials live in `$XDG_CONFIG_HOME/dymus/auth.json` (normally
`~/.config/dymus/auth.json`). Dymus creates the directory with `0700` and the
file with `0600`. For authenticated stream resolution it creates a private,
short-lived yt-dlp cookie jar and removes it as soon as the resolver exits; it
never reads browser profiles or logs cookie values. The file is protected by
filesystem permissions but is not encrypted.

Browser cookies are bearer credentials. Do not put them in issues, chat, shell
history, or source files. If exposed, revoke that Google session and use a fresh
cookie.

## Configuration

On first launch Dymus creates `$XDG_CONFIG_HOME/dymus/config.toml` (normally
`~/.config/dymus/config.toml`). Press `s` to open Settings: Up/Down selects a
theme or startup view, Left/Right changes it, and the selection is saved
immediately. The default startup view is Home.

Built-in themes are Tokyo Night, Catppuccin Mocha, Gruvbox Dark, and Nord. The
terminal background stays untouched for transparency. The TOML file accepts
six-digit color overrides and custom keys. Key values accept one character or
names such as `Tab`, `Space`, `Enter`, `Esc`, `Left`, and `Ctrl+U`.

```toml
theme = "nord"
start_view = "home" # home, explore, playlists, albums, artists, podcasts, radio, search, queue

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
radio = "o"
next_track = "n"
```

Restart after manually editing keys or colors. Settings changes made in the app
are applied and saved immediately.

## Controls

The footer shows actions available in the current view. `/` focuses Search;
type a query and press Enter. While editing, ordinary keys enter text and Esc
returns to browsing.

| Key | Action |
| --- | --- |
| `h` / `e` | Home / Explore |
| `1` / `2` / `3` / `4` | Playlists / Albums / Artists / Podcasts |
| `o` | World Radio |
| `/` | Search or edit the World Radio station filter |
| Tab | Switch Search and queue, or cycle Now Playing panels |
| `t` | Now Playing |
| `s` / `?` / `q` | Settings / help / quit |
| `j` / `k`, arrows | Move selection |
| `g` / `G`, Home / End | First / last row |
| Enter | Search, open a collection, or play the focused/marked song |
| Esc | Return from a collection, clear marks, or dismiss an error |
| `x` / `v` | Mark focused song / select or deselect all |
| `.` | Open contextual actions |
| `a` / `A` | Append to queue / put next |
| `P` / `Q` | Play all / queue all loaded songs |
| `R` | Start generated radio from focused song |
| `f` | Cycle search results: songs, artists, albums, playlists |
| `L` | Load more results from Search, a focused Home or Explore shelf, or Library |
| `d`, Delete | Remove focused or marked queue entries |
| `J` / `K` | Move focused or marked queue entries down / up |
| `C` | Clear upcoming tracks without stopping the current track |
| Space / `n` / `r` | Pause or resume / skip / restart or retry |
| Left / Right | Seek backward or forward five seconds |
| `-` / `+` | Lower or raise volume |
| Ctrl+C / Ctrl+U | Quit from any mode / clear search while editing |

Bulk actions use marked songs when marks exist; otherwise they use the focused
row. `P` replaces the upcoming queue with the loaded list, while `Q` appends
it. Enter starts the first chosen song and puts the remaining songs next while
preserving order. Queue marks follow their entries when moved or advanced.

Inside Now Playing, `q`, `v`, and `y` choose the queue, visualizer, and
lyrics panels. Tab and Shift+Tab cycle between those panels. In the visualizer,
`m` or `]` selects the next style and `[` selects the previous style. In
the lyrics panel, `p` switches between plain and synced lyrics when both are
available, and `j`/`k` scroll plain lyrics.

## Generated radio and World Radio

`R` builds a YouTube Music mix from the focused song. It starts with that song,
then queues related tracks returned by InnerTube while skipping duplicates and
unavailable entries. Existing playback and the queue remain intact if loading
fails. Esc, another radio request, or a manual playback/queue change cancels a
pending request.

World Radio uses Radio Browser's public directory and plays station URLs
directly through mpv. The initial list contains popular working stations. Use
`/`, `c`, `l`, and `g` to filter by station name, country, language, and
genre. `f` selects a filter, `i` edits it, `z` cycles popularity, trending,
votes, bitrate, and alphabetical sort, and `x` clears filters. Enter plays the
selected station.

## Limits and troubleshooting

Search can load additional pages with `L`; use `f` to choose songs, artists,
albums, or playlists, then Enter to open a collection. Generated radio loads
one mix and does not continue endlessly. MPRIS and background playback are not
included. Queue and search state stay in memory; exiting stops playback, though
the upcoming queue is saved and restored on the next launch.
Account-restricted tracks are not supported.

InnerTube is unofficial and may change. Stream extraction relies on a current
yt-dlp installation. Dymus deliberately ignores user yt-dlp and mpv configuration
so unrelated player options cannot alter its behavior. If playback fails, run
`dymus doctor`, update yt-dlp through your package manager, and try another
track. Dymus targets Linux and uses Unix sockets for mpv IPC.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# Optional mpv integration test: generated silence and null audio output.
cargo test mpv_reports_playback_and_eof_with_local_audio -- --ignored
# Optional live search → stream test: public query, null audio output, network required.
cargo test live_search_resolve_and_stream_audio -- --ignored --nocapture
# Optional generated-radio request check.
cargo test live_radio_returns_related_tracks -- --ignored --nocapture
```

Source layout:

- `innertube.rs`: YouTube Music HTTP client and response parsing.
- `model.rs`: track and queue data.
- `player.rs`: stream extraction, mpv lifecycle, and JSON IPC.
- `visualizer.rs`: PipeWire discovery and audio analysis.
- `lyrics.rs`: LRCLIB lookup and synced-lyric parsing.
- `radio.rs`: Radio Browser client and station filtering.
- `cache.rs`: local Home and Explore cache.
- `config.rs`: TOML preferences, palettes, and keybindings.
- `app.rs`: application state, actions, and asynchronous jobs.
- `ui.rs`: terminal layout and rendering.

## Inspiration

- [ytermusic](https://github.com/ccgauche/ytermusic): simplicity and playlist workflow.
- [youtui](https://github.com/nick42d/youtui): music discovery and keyboard navigation.
- [ytmusic-tui](https://github.com/WakaTaira/ytmusic-tui): browsing and queue interaction.
- [ytmusicapi](https://github.com/sigma67/ytmusicapi): InnerTube request and response reference.

Dymus is not affiliated with or endorsed by YouTube or Google.
