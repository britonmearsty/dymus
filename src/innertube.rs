use crate::{
    auth::{self, BrowserAuth},
    model::Track,
};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct InnerTube {
    client: Client,
    auth: Option<BrowserAuth>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryKind {
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    Songs,
    Artists,
    Albums,
    Playlists,
}

impl SearchFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Songs => Self::Artists,
            Self::Artists => Self::Albums,
            Self::Albums => Self::Playlists,
            Self::Playlists => Self::Songs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Songs => "songs",
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Playlists => "playlists",
        }
    }

    fn params(self) -> &'static str {
        match self {
            Self::Songs => "EgWKAQIIAWoKEAMQBBAJEAoQBQ==",
            Self::Artists => "EgWKAQIgAWoKEAMQBBAJEAoQBQ==",
            Self::Albums => "EgWKAQIYAWoKEAMQBBAJEAoQBQ==",
            Self::Playlists => "EgWKAQJAAWoKEAMQBBAJEAoQBQ==",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LibraryItem {
    #[serde(default)]
    pub section: String,
    pub title: String,
    pub detail: String,
    pub browse_id: String,
    pub playlist_id: String,
    pub track: Option<Track>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscoveryContinuation {
    pub section: String,
    pub token: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DiscoveryPage {
    pub items: Vec<LibraryItem>,
    pub continuations: Vec<DiscoveryContinuation>,
}

#[derive(Clone, Debug, Default)]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub continuation: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TrackPage {
    pub tracks: Vec<Track>,
    pub continuation: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    pub tracks: Vec<Track>,
    pub items: Vec<LibraryItem>,
    pub continuation: Option<String>,
}

impl InnerTube {
    pub fn new() -> Result<Self> {
        Ok(Self { client: Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
            .build()?, auth: None })
    }

    pub fn configured() -> Result<Self> {
        let mut client = Self::new()?;
        client.auth = auth::load()?;
        Ok(client)
    }

    pub fn with_auth(mut self, auth: BrowserAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn is_authenticated(&self) -> bool {
        self.auth.is_some()
    }

    /// Records a genuinely played track using YouTube Music's authenticated
    /// playback-tracking URL. This intentionally obtains a fresh URL per track.
    pub async fn add_history_item(&self, video_id: &str) -> Result<()> {
        anyhow::ensure!(self.auth.is_some(), "History reporting requires sign-in");
        let player = self.request("player", json!({"videoId": video_id})).await?;
        let tracking_url = player
            .pointer("/playbackTracking/videostatsPlaybackUrl/baseUrl")
            .and_then(Value::as_str)
            .context("YouTube Music did not provide a playback tracking URL")?;
        let tracking_url = reqwest::Url::parse(tracking_url)
            .context("YouTube Music returned an invalid playback tracking URL")?;
        anyhow::ensure!(
            tracking_url.scheme() == "https" && tracking_url.host_str() == Some("s.youtube.com"),
            "YouTube Music returned an unexpected playback tracking host"
        );
        let nonce = format!(
            "{:016x}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64
        );
        let auth = self.auth.as_ref().expect("checked above");
        self.client
            .get(tracking_url)
            .query(&[("ver", "2"), ("c", "WEB_REMIX"), ("cpn", nonce.as_str())])
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header(reqwest::header::COOKIE, auth.cookie())
            .header("X-Goog-AuthUser", auth.auth_user())
            .header(
                reqwest::header::AUTHORIZATION,
                auth.authorization(auth::unix_timestamp()?)?,
            )
            .send()
            .await
            .context("Cannot report playback to YouTube Music")?
            .error_for_status()
            .context("YouTube Music rejected the playback report")?;
        Ok(())
    }

    pub async fn validate_session(&self) -> Result<String> {
        anyhow::ensure!(
            self.auth.is_some(),
            "No YouTube Music credentials are configured; run `dymus auth paste`"
        );
        let response = self
            .request("account/account_menu", json!({}))
            .await
            .context("Could not validate the YouTube Music session")?;
        let account = response.pointer("/actions/0/openPopupAction/popup/multiPageMenuRenderer/header/activeAccountHeaderRenderer/accountName")
            .and_then(|name| name.pointer("/runs/0/text").and_then(Value::as_str)
                .or_else(|| name.get("simpleText").and_then(Value::as_str)));
        account
            .map(ToOwned::to_owned)
            .context("YouTube Music did not return a signed-in account; the session may be expired or the account index may be wrong")
    }

    pub async fn search(&self, query: &str, filter: SearchFilter) -> Result<SearchPage> {
        if query.trim().is_empty() {
            bail!("Enter a song or artist to search");
        }
        let response = self
            .request(
                "search",
                json!({
                    "query": query.trim(),
                    "params": filter.params()
                }),
            )
            .await?;
        parse_search_page(&response, filter)
    }

    pub async fn search_more(&self, token: &str, filter: SearchFilter) -> Result<SearchPage> {
        let response = self
            .request("search", json!({"continuation": token}))
            .await?;
        parse_search_page(&response, filter)
    }

    pub async fn radio(&self, video_id: &str) -> Result<Vec<Track>> {
        let response = self
            .request(
                "next",
                json!({
                    "videoId": video_id,
                    "playlistId": format!("RDAMVM{video_id}"),
                    "params": "wAEB",
                    "isAudioOnly": true,
                    "enablePersistentPlaylistPanel": true,
                    "tunerSettingValue": "AUTOMIX_SETTING_NORMAL"
                }),
            )
            .await?;
        parse_radio(&response)
    }

    pub async fn library(&self, kind: LibraryKind) -> Result<LibraryPage> {
        anyhow::ensure!(
            self.auth.is_some(),
            "Library requires sign-in; run `dymus auth paste`"
        );
        let browse_id = match kind {
            LibraryKind::Playlists => "FEmusic_liked_playlists",
            LibraryKind::Albums => "FEmusic_liked_albums",
            LibraryKind::Artists => "FEmusic_library_corpus_track_artists",
            LibraryKind::Podcasts => "FEmusic_library_non_music_audio_list",
        };
        let response = self
            .request("browse", json!({"browseId": browse_id}))
            .await?;
        let mut page = parse_library(&response)?;
        if kind == LibraryKind::Podcasts {
            page.items
                .retain(|item| !item.title.eq_ignore_ascii_case("add podcast"));
        }
        Ok(page)
    }

    pub async fn library_more(&self, token: &str) -> Result<LibraryPage> {
        let response = self
            .request("browse", json!({"continuation": token}))
            .await?;
        parse_library(&response)
    }

    pub async fn discover(&self, explore: bool) -> Result<DiscoveryPage> {
        let browse_id = if explore {
            "FEmusic_explore"
        } else {
            "FEmusic_home"
        };
        let response = self
            .request("browse", json!({"browseId": browse_id}))
            .await?;
        parse_discovery(&response, None)
    }

    pub async fn discover_more(&self, token: &str, section: &str) -> Result<DiscoveryPage> {
        let response = self
            .request("browse", json!({"continuation": token}))
            .await?;
        parse_discovery(&response, Some(section))
    }

    pub async fn library_tracks(&self, item: &LibraryItem) -> Result<TrackPage> {
        if let Some(track) = &item.track {
            return Ok(TrackPage {
                tracks: vec![track.clone()],
                continuation: None,
            });
        }
        let body = if !item.playlist_id.is_empty() {
            json!({"playlistId": item.playlist_id})
        } else {
            json!({"browseId": item.browse_id})
        };
        let response = self.request("browse", body).await?;
        parse_track_page(&response)
    }

    pub async fn library_tracks_more(&self, token: &str) -> Result<TrackPage> {
        let response = self
            .request("browse", json!({"continuation": token}))
            .await?;
        parse_track_page(&response)
    }

    async fn request(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        body["context"] = json!({ "client": {
            "clientName": "WEB_REMIX", "clientVersion": "1.20250915.03.00",
            "hl": "en", "gl": "US"
        }});
        let mut request = self
            .client
            .post(format!(
                "https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"
            ))
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .json(&body);
        if let Some(auth) = &self.auth {
            request = request
                .header(reqwest::header::COOKIE, auth.cookie())
                .header("X-Goog-AuthUser", auth.auth_user())
                .header(
                    reqwest::header::AUTHORIZATION,
                    auth.authorization(auth::unix_timestamp()?)?,
                );
        }
        let response: Value = request
            .send()
            .await
            .context("Cannot reach YouTube Music")?
            .error_for_status()
            .context("YouTube Music rejected the request")?
            .json()
            .await
            .context("YouTube Music returned an invalid response")?;
        if let Some(error) = response.get("error") {
            bail!(
                "YouTube Music: {}",
                error["message"].as_str().unwrap_or("request failed")
            );
        }
        Ok(response)
    }
}

fn collect_library_items(value: &Value, items: &mut Vec<LibraryItem>, seen: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            for key in [
                "musicTwoRowItemRenderer",
                "musicResponsiveListItemRenderer",
                "musicMultiRowListItemRenderer",
                "musicPlaylistShelfRenderer",
                "musicNavigationButtonRenderer",
            ] {
                if let Some(item) = map.get(key) {
                    if key == "musicResponsiveListItemRenderer"
                        && let Some(track) = parse_track(item)
                        && seen.insert(track.id.clone())
                    {
                        items.push(LibraryItem {
                            section: String::new(),
                            title: track.title.clone(),
                            detail: track.artist.clone(),
                            browse_id: String::new(),
                            playlist_id: String::new(),
                            track: Some(track),
                        });
                        return;
                    }
                    if key == "musicMultiRowListItemRenderer"
                        && let Some(track) = parse_multi_row_track(item)
                        && seen.insert(track.id.clone())
                    {
                        items.push(LibraryItem {
                            section: String::new(),
                            title: track.title.clone(),
                            detail: track.artist.clone(),
                            browse_id: String::new(),
                            playlist_id: String::new(),
                            track: Some(track),
                        });
                        return;
                    }
                    let title = item.pointer("/title/runs/0/text").and_then(Value::as_str).or_else(|| item.pointer("/title/simpleText").and_then(Value::as_str)).or_else(|| item.pointer("/buttonText/runs/0/text").and_then(Value::as_str)).or_else(|| item.pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text").and_then(Value::as_str)).unwrap_or("");
                    let browse = item
                        .pointer("/navigationEndpoint/browseEndpoint/browseId")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            item.pointer("/title/runs/0/navigationEndpoint/browseEndpoint/browseId")
                                .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            item.pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/browseEndpoint/browseId")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("");
                    let playlist = item
                        .pointer("/navigationEndpoint/watchPlaylistEndpoint/playlistId")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            item.pointer(
                                "/title/runs/0/navigationEndpoint/watchEndpoint/playlistId",
                            )
                            .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            item.pointer("/buttonCommand/watchPlaylistEndpoint/playlistId")
                                .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            item.pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint/playlistId")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("");
                    let video_id = item
                        .pointer("/navigationEndpoint/watchEndpoint/videoId")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            item.pointer("/title/runs/0/navigationEndpoint/watchEndpoint/videoId")
                                .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            item.pointer("/buttonCommand/watchEndpoint/videoId")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("");
                    let id = if !playlist.is_empty() {
                        playlist
                    } else if !browse.is_empty() {
                        browse
                    } else {
                        video_id
                    };
                    if !title.is_empty() && !id.is_empty() && seen.insert(id.to_owned()) {
                        let detail = item.pointer("/subtitle/runs").and_then(Value::as_array).map(|runs| text_runs(runs)).or_else(|| item.pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs").and_then(Value::as_array).map(|runs| text_runs(runs))).unwrap_or_default();
                        items.push(LibraryItem {
                            section: String::new(),
                            title: title.into(),
                            detail,
                            browse_id: browse.into(),
                            playlist_id: playlist.into(),
                            track: if video_id.is_empty() {
                                None
                            } else {
                                Some(Track {
                                    id: video_id.into(),
                                    title: title.into(),
                                    artist: String::new(),
                                    album: String::new(),
                                    duration: String::new(),
                                })
                            },
                        });
                    }
                    return;
                }
            }
            for child in map.values() {
                collect_library_items(child, items, seen);
            }
        }
        Value::Array(children) => {
            for child in children {
                collect_library_items(child, items, seen);
            }
        }
        _ => {}
    }
}

fn parse_library(response: &Value) -> Result<LibraryPage> {
    let contents = response
        .pointer("/contents/singleColumnBrowseResultsRenderer/tabs")
        .and_then(Value::as_array)
        .and_then(|tabs| {
            tabs.iter()
                .find_map(|tab| tab.pointer("/tabRenderer/content"))
        })
        .or_else(|| response.pointer("/continuationContents/musicShelfContinuation/contents"))
        .or_else(|| response.pointer("/continuationContents/gridContinuation/items"))
        .context("Library response has no contents; the InnerTube API may have changed")?;
    let mut items = Vec::new();
    collect_library_items(contents, &mut items, &mut HashSet::new());
    let continuation = response
        .pointer("/contents/singleColumnBrowseResultsRenderer/tabs")
        .and_then(Value::as_array)
        .and_then(|tabs| {
            tabs.iter().find_map(|tab| {
                tab.pointer("/tabRenderer/content/sectionListRenderer")
                    .or_else(|| tab.pointer("/tabRenderer/content/musicShelfRenderer"))
            })
        })
        .or_else(|| response.pointer("/continuationContents/musicShelfContinuation"))
        .or_else(|| response.pointer("/continuationContents/gridContinuation"))
        .and_then(continuation_token);
    Ok(LibraryPage {
        items,
        continuation,
    })
}

fn parse_discovery(response: &Value, fallback_section: Option<&str>) -> Result<DiscoveryPage> {
    let shelf_continuation = response.pointer("/continuationContents/musicShelfContinuation");
    let sections = response
        .pointer("/contents/singleColumnBrowseResultsRenderer/tabs")
        .and_then(Value::as_array)
        .and_then(|tabs| {
            tabs.iter()
                .find_map(|tab| tab.pointer("/tabRenderer/content/sectionListRenderer/contents"))
        })
        .or_else(|| response.pointer("/continuationContents/sectionListContinuation/contents"))
        .or_else(|| response.pointer("/continuationContents/musicShelfContinuation/contents"))
        .and_then(Value::as_array)
        .context("Home response has no sections; the InnerTube API may have changed")?;
    let mut page = DiscoveryPage::default();
    let mut seen = HashSet::new();
    if shelf_continuation.is_some() {
        let section = fallback_section.unwrap_or("recommended");
        for content in sections {
            let mut items = Vec::new();
            collect_library_items(content, &mut items, &mut seen);
            for mut item in items {
                item.section = section.to_owned();
                page.items.push(item);
            }
        }
        if let Some(token) = shelf_continuation.and_then(continuation_token) {
            page.continuations.push(DiscoveryContinuation {
                section: section.to_owned(),
                token,
            });
        }
    } else {
        for section in sections {
            collect_discovery_section(
                section,
                fallback_section.unwrap_or("recommended"),
                &mut page,
                &mut seen,
            );
        }
        if let Some(token) = response
            .pointer("/contents/singleColumnBrowseResultsRenderer/tabs")
            .and_then(Value::as_array)
            .and_then(|tabs| {
                tabs.iter()
                    .find_map(|tab| tab.pointer("/tabRenderer/content/sectionListRenderer"))
            })
            .and_then(continuation_token)
            .or_else(|| {
                response
                    .pointer("/continuationContents/sectionListContinuation")
                    .and_then(continuation_token)
            })
        {
            let section = page
                .items
                .last()
                .map(|item| item.section.clone())
                .unwrap_or_else(|| fallback_section.unwrap_or("recommended").to_owned());
            page.continuations
                .push(DiscoveryContinuation { section, token });
        }
    }
    Ok(page)
}

fn collect_discovery_section(
    value: &Value,
    fallback_section: &str,
    page: &mut DiscoveryPage,
    seen: &mut HashSet<String>,
) {
    if let Some(renderer) = value.get("itemSectionRenderer") {
        if let Some(contents) = renderer.get("contents").and_then(Value::as_array) {
            for content in contents {
                collect_discovery_section(content, fallback_section, page, seen);
            }
        }
        return;
    }
    let renderer = [
        "musicCarouselShelfRenderer",
        "musicShelfRenderer",
        "musicImmersiveCarouselShelfRenderer",
        "musicCardShelfRenderer",
        "gridRenderer",
    ]
    .iter()
    .find_map(|key| value.get(*key))
    .or_else(|| value.get("musicDescriptionShelfRenderer"));
    let Some(renderer) = renderer else {
        return;
    };
    let title = shelf_title(renderer);
    let contents = renderer
        .get("contents")
        .or_else(|| renderer.get("items"))
        .or_else(|| renderer.pointer("/content/gridRenderer/items"));
    let title = if title.trim().is_empty() {
        fallback_section
    } else {
        &title
    };
    if let Some(contents) = contents.and_then(Value::as_array) {
        for content in contents {
            let mut items = Vec::new();
            collect_library_items(content, &mut items, seen);
            for mut item in items {
                item.section = title.to_owned();
                page.items.push(item);
            }
        }
    }
    if let Some(token) = continuation_token(renderer) {
        page.continuations.push(DiscoveryContinuation {
            section: title.to_owned(),
            token,
        });
    }
}

fn shelf_title(renderer: &Value) -> String {
    [
        "/header/musicCarouselShelfBasicHeaderRenderer/title",
        "/header/musicCarouselShelfBasicHeaderRenderer/strapline",
        "/title",
        "/header/title",
        "/header/musicResponsiveHeaderRenderer/title",
    ]
    .iter()
    .map(|pointer| rich_text(renderer.pointer(pointer).unwrap_or(&Value::Null)))
    .find(|title| !title.trim().is_empty())
    .unwrap_or_default()
}

fn continuation_token(value: &Value) -> Option<String> {
    value
        .pointer("/continuations/0/nextContinuationData/continuation")
        .or_else(|| value.pointer("/continuations/0/reloadContinuationData/continuation"))
        .or_else(|| value.pointer("/continuationEndpoint/continuationCommand/token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

pub fn parse_search(response: &Value) -> Result<Vec<Track>> {
    if let Some(error) = response.get("error") {
        bail!(
            "YouTube Music: {}",
            error["message"].as_str().unwrap_or("request failed")
        );
    }
    let contents = response
        .get("contents")
        .context("Search response has no contents; the InnerTube API may have changed")?;
    let mut tracks = Vec::new();
    let mut seen = HashSet::new();
    collect_tracks(contents, &mut tracks, &mut seen);
    Ok(tracks)
}

fn parse_search_page(response: &Value, filter: SearchFilter) -> Result<SearchPage> {
    if filter == SearchFilter::Songs {
        return Ok(SearchPage {
            tracks: parse_search(response)?,
            items: Vec::new(),
            continuation: search_continuation(response),
        });
    }
    if let Some(error) = response.get("error") {
        bail!(
            "YouTube Music: {}",
            error["message"].as_str().unwrap_or("request failed")
        );
    }
    let contents = response
        .get("contents")
        .or_else(|| response.pointer("/continuationContents"))
        .context("Search response has no contents; the InnerTube API may have changed")?;
    let mut items = Vec::new();
    collect_library_items(contents, &mut items, &mut HashSet::new());
    Ok(SearchPage {
        tracks: Vec::new(),
        items,
        continuation: search_continuation(response),
    })
}

fn search_continuation(response: &Value) -> Option<String> {
    response
        .pointer("/continuationContents/musicShelfContinuation")
        .or_else(|| response.pointer("/continuationContents/sectionListContinuation"))
        .or_else(|| {
            response.pointer(
                "/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer",
            )
        })
        .and_then(continuation_token)
        .or_else(|| response.get("contents").and_then(find_continuation))
}

fn find_continuation(value: &Value) -> Option<String> {
    if let Some(token) = continuation_token(value) {
        return Some(token);
    }
    match value {
        Value::Object(map) => map.values().find_map(find_continuation),
        Value::Array(values) => values.iter().find_map(find_continuation),
        _ => None,
    }
}

fn parse_track_page(response: &Value) -> Result<TrackPage> {
    let tracks = parse_search(response)?;
    let continuation = response
        .pointer("/continuationContents/musicShelfContinuation")
        .or_else(|| response.pointer("/continuationContents/sectionListContinuation"))
        .or_else(|| response.pointer("/contents/twoColumnBrowseResultsRenderer/secondaryContents"))
        .and_then(continuation_token);
    Ok(TrackPage {
        tracks,
        continuation,
    })
}

// Parse only the actual watch queue, not recommendations, menus, or alternate
// video counterparts. Preserve YouTube's order and skip unavailable entries.
fn parse_radio(response: &Value) -> Result<Vec<Track>> {
    let tabs = response.pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs")
        .and_then(Value::as_array).context("Radio response has no watch queue")?;
    let entries = tabs
        .iter()
        .find_map(|tab| {
            tab.pointer(
                "/tabRenderer/content/musicQueueRenderer/content/playlistPanelRenderer/contents",
            )
        })
        .and_then(Value::as_array)
        .context("Radio response has no tracks")?;
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    for entry in entries {
        let item = entry.get("playlistPanelVideoRenderer").or_else(|| {
            entry.pointer(
                "/playlistPanelVideoWrapperRenderer/primaryRenderer/playlistPanelVideoRenderer",
            )
        });
        let Some(item) = item else {
            continue;
        };
        if item.get("unplayableText").is_some() || item["isPlayable"] == false {
            continue;
        }
        let Some(id) = item["videoId"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        let title = rich_text(&item["title"]);
        if title.is_empty() || !seen.insert(id.to_owned()) {
            continue;
        }
        let byline = item
            .get("longBylineText")
            .or_else(|| item.get("shortBylineText"))
            .unwrap_or(&Value::Null);
        let mut artists = Vec::new();
        let mut album = String::new();
        if let Some(runs) = byline["runs"].as_array() {
            for run in runs {
                let browse = run
                    .pointer("/navigationEndpoint/browseEndpoint/browseId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = run["text"].as_str().unwrap_or_default();
                if browse.starts_with("UC") {
                    artists.push(text);
                }
                if browse.starts_with("MPRE") {
                    album = text.into();
                }
            }
        }
        let artist = if artists.is_empty() {
            rich_text(item.get("shortBylineText").unwrap_or(byline))
        } else {
            artists.join(", ")
        };
        tracks.push(Track {
            id: id.into(),
            title,
            artist,
            album,
            duration: rich_text(&item["lengthText"]),
        });
    }
    if tracks.is_empty() {
        bail!("YouTube Music returned no playable radio tracks");
    }
    Ok(tracks)
}

fn rich_text(value: &Value) -> String {
    value["runs"]
        .as_array()
        .map(|runs| text_runs(runs))
        .unwrap_or_else(|| value["simpleText"].as_str().unwrap_or_default().to_owned())
}

fn collect_tracks(value: &Value, tracks: &mut Vec<Track>, seen: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(track) = parse_track(item)
                    && seen.insert(track.id.clone())
                {
                    tracks.push(track);
                }
                return;
            }
            if let Some(item) = map.get("musicMultiRowListItemRenderer") {
                if let Some(track) = parse_multi_row_track(item)
                    && seen.insert(track.id.clone())
                {
                    tracks.push(track);
                }
                return;
            }
            for child in map.values() {
                collect_tracks(child, tracks, seen);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_tracks(child, tracks, seen);
            }
        }
        _ => {}
    }
}

fn parse_track(item: &Value) -> Option<Track> {
    if item["musicItemRendererDisplayPolicy"] == "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT" {
        return None;
    }
    let columns = item["flexColumns"].as_array()?;
    let title_runs = columns
        .first()?
        .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")?
        .as_array()?;
    let id = item.pointer("/playlistItemData/videoId")
        .or_else(|| title_runs.first()?.pointer("/navigationEndpoint/watchEndpoint/videoId"))
        .or_else(|| item.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId"))?.as_str()?;
    if id.is_empty() {
        return None;
    }
    let title = text_runs(title_runs);
    if title.trim().is_empty() {
        return None;
    }
    let runs: Vec<&Value> = columns
        .iter()
        .skip(1)
        .filter_map(|column| {
            column
                .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
                .and_then(Value::as_array)
        })
        .flatten()
        .collect();
    let mut artists = Vec::new();
    let mut album = String::new();
    let mut duration = String::new();
    for run in &runs {
        let text = run["text"].as_str().unwrap_or_default();
        let browse_id = run
            .pointer("/navigationEndpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if browse_id.starts_with("UC") {
            artists.push(text);
        }
        if browse_id.starts_with("MPRE") {
            album = text.to_owned();
        }
        if is_duration(text) {
            duration = text.to_owned();
        }
    }
    if duration.is_empty()
        && let Some(fixed) = item["fixedColumns"].as_array()
    {
        for column in fixed {
            if let Some(runs) = column
                .pointer("/musicResponsiveListItemFixedColumnRenderer/text/runs")
                .and_then(Value::as_array)
            {
                let text = text_runs(runs);
                if is_duration(&text) {
                    duration = text;
                }
            }
        }
    }
    let artist = if artists.is_empty() {
        runs.first()
            .and_then(|run| run["text"].as_str())
            .unwrap_or("Unknown artist")
            .to_owned()
    } else {
        artists.join(", ")
    };
    Some(Track {
        id: id.into(),
        title,
        artist,
        album,
        duration,
    })
}

fn parse_multi_row_track(item: &Value) -> Option<Track> {
    let id = item
        .pointer("/playNavigationEndpoint/watchEndpoint/videoId")
        .or_else(|| item.pointer("/title/runs/0/navigationEndpoint/watchEndpoint/videoId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let title = rich_text(&item["title"]);
    if title.trim().is_empty() {
        return None;
    }
    Some(Track {
        id: id.to_owned(),
        title,
        artist: rich_text(&item["subtitle"]),
        album: String::new(),
        duration: String::new(),
    })
}

fn text_runs(runs: &[Value]) -> String {
    runs.iter().filter_map(|run| run["text"].as_str()).collect()
}
fn is_duration(text: &str) -> bool {
    text.contains(':')
        && text
            .split(':')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_feed_cards_and_quick_pick_tracks() {
        let response = json!({"contents":{"items":[
            {"musicTwoRowItemRenderer":{"title":{"runs":[{"text":"Mix"}]},"subtitle":{"runs":[{"text":"Daily mix"}]},"navigationEndpoint":{"watchPlaylistEndpoint":{"playlistId":"RDAMVMmix"}}}},
            {"musicResponsiveListItemRenderer":{"flexColumns":[{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Song","navigationEndpoint":{"watchEndpoint":{"videoId":"song-id"}}}]}}},{"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Artist","navigationEndpoint":{"browseEndpoint":{"browseId":"UCartist"}}}]}}}],"playlistItemData":{"videoId":"song-id"}}}
        ]}});
        let mut items = Vec::new();
        collect_library_items(&response, &mut items, &mut HashSet::new());
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].playlist_id, "RDAMVMmix");
        assert_eq!(items[1].track.as_ref().unwrap().id, "song-id");
        assert_eq!(items[1].detail, "Artist");
    }

    #[test]
    fn preserves_discovery_shelves_and_continuations() {
        let response = json!({"contents":{"singleColumnBrowseResultsRenderer":{"tabs":[{
            "tabRenderer":{"content":{"sectionListRenderer":{"contents":[
                {"musicCarouselShelfRenderer":{
                    "header":{"musicCarouselShelfBasicHeaderRenderer":{"title":{"runs":[{"text":"Made for you"}]}}},
                    "contents":[{"musicTwoRowItemRenderer":{
                        "title":{"runs":[{"text":"Daily Mix"}]},
                        "navigationEndpoint":{"watchPlaylistEndpoint":{"playlistId":"RDmix"}}
                    }}],
                    "continuations":[{"nextContinuationData":{"continuation":"more-mixes"}}]
                }},
                {"musicShelfRenderer":{
                    "title":{"runs":[{"text":"Quick picks"}]},
                    "contents":[{"musicResponsiveListItemRenderer":{
                        "flexColumns":[
                            {"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Song","navigationEndpoint":{"watchEndpoint":{"videoId":"song"}}}]}}},
                            {"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Artist"}]}}}
                        ],
                        "playlistItemData":{"videoId":"song"}
                    }}]
                }}
            ]}}}
        }]}}});
        let page = parse_discovery(&response, None).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].section, "Made for you");
        assert_eq!(page.items[1].section, "Quick picks");
        assert_eq!(page.continuations[0].token, "more-mixes");

        let more = json!({"continuationContents":{"musicShelfContinuation":{
            "contents":[{"musicTwoRowItemRenderer":{
                "title":{"runs":[{"text":"Another mix"}]},
                "navigationEndpoint":{"watchPlaylistEndpoint":{"playlistId":"RDmore"}}
            }}],
            "continuations":[{"nextContinuationData":{"continuation":"even-more"}}]
        }}});
        let more_page = parse_discovery(&more, Some("Made for you")).unwrap();
        assert_eq!(more_page.items[0].section, "Made for you");
        assert_eq!(more_page.items[0].playlist_id, "RDmore");
        assert_eq!(more_page.continuations[0].token, "even-more");
    }

    #[test]
    fn parses_immersive_cards_and_page_continuations() {
        let response = json!({"contents":{"singleColumnBrowseResultsRenderer":{"tabs":[{
            "tabRenderer":{"content":{"sectionListRenderer":{
                "contents":[{"musicImmersiveCarouselShelfRenderer":{
                    "header":{"musicCarouselShelfBasicHeaderRenderer":{"title":{"runs":[{"text":"Genres"}]}}},
                    "contents":[{"musicNavigationButtonRenderer":{
                        "buttonText":{"runs":[{"text":"Jazz"}]},
                        "navigationEndpoint":{"browseEndpoint":{"browseId":"FEmusic_moods_and_genres_category_jazz"}}
                    }}]
                }}],
                "continuations":[{"nextContinuationData":{"continuation":"next-page"}}]
            }}}
        }]}}});
        let page = parse_discovery(&response, None).unwrap();
        assert_eq!(page.items[0].section, "Genres");
        assert_eq!(
            page.items[0].browse_id,
            "FEmusic_moods_and_genres_category_jazz"
        );
        assert_eq!(page.continuations[0].token, "next-page");
    }

    #[test]
    fn parses_podcast_library_items_as_browsable_shows() {
        let response = json!({"contents":{"gridRenderer":{"items":[
            {"musicTwoRowItemRenderer":{
                "title":{"runs":[{"text":"Show name"}]},
                "subtitle":{"runs":[{"text":"Podcast · Publisher"}]},
                "navigationEndpoint":{"browseEndpoint":{"browseId":"MPSPpodcast-show"}}
            }}
        ]}}});
        let mut items = Vec::new();
        collect_library_items(&response, &mut items, &mut HashSet::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Show name");
        assert_eq!(items[0].detail, "Podcast · Publisher");
        assert_eq!(items[0].browse_id, "MPSPpodcast-show");
        assert!(items[0].track.is_none());
    }

    #[test]
    fn parses_podcast_episode_rows_from_a_show_page() {
        let response = json!({"contents":{"singleColumnBrowseResultsRenderer":{"tabs":[{
            "tabRenderer":{"content":{"sectionListRenderer":{"contents":[{
                "musicShelfRenderer":{"contents":[{
                    "musicMultiRowListItemRenderer":{
                        "title":{"runs":[{"text":"Episode one", "navigationEndpoint":{"watchEndpoint":{"videoId":"episode-one"}}}]},
                        "subtitle":{"runs":[{"text":"Show name · Sep 21"}]},
                        "playNavigationEndpoint":{"watchEndpoint":{"videoId":"episode-one"}}
                    }
                }]}
            }]}}}
        }]}}});
        let page = parse_track_page(&response).unwrap();
        assert_eq!(page.tracks.len(), 1);
        assert_eq!(page.tracks[0].id, "episode-one");
        assert_eq!(page.tracks[0].title, "Episode one");
        assert_eq!(page.tracks[0].artist, "Show name · Sep 21");
    }

    #[test]
    fn preserves_library_order_and_continuation() {
        let response = json!({"contents":{"singleColumnBrowseResultsRenderer":{"tabs":[{
            "tabRenderer":{"content":{"sectionListRenderer":{
                "contents":[{"musicShelfRenderer":{"contents":[
                    {"musicTwoRowItemRenderer":{
                        "title":{"runs":[{"text":"First album"}]},
                        "navigationEndpoint":{"browseEndpoint":{"browseId":"MPREfirst"}}
                    }},
                    {"musicTwoRowItemRenderer":{
                        "title":{"runs":[{"text":"Second album"}]},
                        "navigationEndpoint":{"browseEndpoint":{"browseId":"MPREsecond"}}
                    }}
                ]}}],
                "continuations":[{"nextContinuationData":{"continuation":"next-library-page"}}]
            }}}
        }]}}});
        let page = parse_library(&response).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["First album", "Second album"]
        );
        assert_eq!(page.continuation.as_deref(), Some("next-library-page"));
    }

    #[test]
    fn radio_uses_primary_tracks_and_skips_duplicates_and_unavailable_items() {
        let fixture = serde_json::from_str(include_str!("../tests/fixtures/radio.json")).unwrap();
        let tracks = parse_radio(&fixture).unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "related"]
        );
        assert_eq!(tracks[1].artist, "Artist");
        assert_eq!(tracks[1].album, "Album");
        assert_eq!(tracks[1].duration, "3:45");
        assert!(parse_radio(&json!({"contents": {}})).is_err());
    }

    #[tokio::test]
    #[ignore = "requires YouTube Music network access"]
    async fn live_radio_returns_related_tracks() {
        let api = InnerTube::new().unwrap();
        let songs = api
            .search("Nujabes Feather", SearchFilter::Songs)
            .await
            .unwrap();
        let seed = songs
            .tracks
            .first()
            .expect("search should find a seed track");
        let tracks = api.radio(&seed.id).await.unwrap();
        assert!(tracks.iter().any(|track| track.id != seed.id));
        assert_eq!(
            tracks
                .iter()
                .map(|track| &track.id)
                .collect::<HashSet<_>>()
                .len(),
            tracks.len()
        );
        eprintln!("Radio for {}: {} playable tracks", seed.title, tracks.len());
    }

    #[tokio::test]
    #[ignore = "requires YouTube Music network access"]
    async fn live_home_has_labeled_shelves() {
        let page = InnerTube::new().unwrap().discover(false).await.unwrap();
        assert!(!page.items.is_empty());
        assert!(
            page.items
                .iter()
                .all(|item| !item.section.trim().is_empty())
        );
        eprintln!(
            "Home: {} items across {} shelves",
            page.items.len(),
            page.items
                .iter()
                .map(|item| item.section.as_str())
                .collect::<HashSet<_>>()
                .len()
        );
    }

    #[tokio::test]
    #[ignore = "requires YouTube Music network access"]
    async fn live_explore_has_labeled_shelves() {
        let page = InnerTube::new().unwrap().discover(true).await.unwrap();
        assert!(!page.items.is_empty());
        assert!(
            page.items
                .iter()
                .all(|item| !item.section.trim().is_empty())
        );
        eprintln!(
            "Explore: {} items across {} shelves",
            page.items.len(),
            page.items
                .iter()
                .map(|item| item.section.as_str())
                .collect::<HashSet<_>>()
                .len()
        );
    }

    #[tokio::test]
    #[ignore = "requires configured YouTube Music authentication and network access"]
    async fn live_authenticated_collections_parse() {
        let api = InnerTube::configured().unwrap();
        for kind in [
            LibraryKind::Playlists,
            LibraryKind::Albums,
            LibraryKind::Artists,
            LibraryKind::Podcasts,
        ] {
            let page = api.library(kind).await.unwrap();
            assert!(page.items.iter().all(|item| !item.title.trim().is_empty()));
            eprintln!("{kind:?}: {} items", page.items.len());
        }
    }

    #[test]
    fn parses_nested_results_and_deduplicates_without_menu_tracks() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/search.json")).unwrap();
        let tracks = parse_search(&fixture).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks[0],
            Track {
                id: "song-one".into(),
                title: "First song".into(),
                artist: "First artist, Guest".into(),
                album: "First album".into(),
                duration: "3:42".into()
            }
        );
        assert_eq!(tracks[1].id, "song-two");
        assert_eq!(tracks[1].duration, "4:02");
    }

    #[test]
    fn parses_artist_search_results_and_their_continuation() {
        let response = json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{
            "tabRenderer": {"content": {"sectionListRenderer": {
                "contents": [{"musicShelfRenderer": {"contents": [{
                    "musicResponsiveListItemRenderer": {
                        "flexColumns": [
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{
                                "text": "An artist",
                                "navigationEndpoint": {"browseEndpoint": {"browseId": "UCartist"}}
                            }]}}},
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Artist"}]}}}
                        ]
                    }
                }]}}],
                "continuations": [{"nextContinuationData": {"continuation": "more-artists"}}]
            }}}
        }]}}});
        let page = parse_search_page(&response, SearchFilter::Artists).unwrap();
        assert!(page.tracks.is_empty());
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "An artist");
        assert_eq!(page.items[0].browse_id, "UCartist");
        assert_eq!(page.continuation.as_deref(), Some("more-artists"));
    }

    #[test]
    fn search_filters_have_distinct_request_parameters() {
        assert_ne!(SearchFilter::Songs.params(), SearchFilter::Artists.params());
        assert_ne!(
            SearchFilter::Artists.params(),
            SearchFilter::Albums.params()
        );
        assert_ne!(
            SearchFilter::Albums.params(),
            SearchFilter::Playlists.params()
        );
    }
    #[test]
    fn distinguishes_empty_results_from_api_errors() {
        assert!(
            parse_search(&json!({"contents": {"sectionListRenderer": {"contents": []}}}))
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_search(&json!({"error": {"message": "Try again"}}))
                .unwrap_err()
                .to_string()
                .contains("Try again")
        );
        assert!(parse_search(&json!({"unexpected": []})).is_err());
    }
}
