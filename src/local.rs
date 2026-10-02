//! Offline library reader shared by headless and TUI playback.
use crate::{config::Config, download::expand_path, download_library, model::Track};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

#[derive(Clone, Default, Serialize)]
pub struct Library {
    pub collections: Vec<Collection>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Serialize)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub media_type: String,
    pub partial: bool,
    pub missing: usize,
    pub tracks: Vec<Track>,
}
impl Library {
    pub fn filtered(mut self, query: &str) -> Self {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self;
        }
        self.collections.retain_mut(|collection| {
            if collection.title.to_lowercase().contains(&query) || collection.id == query {
                return true;
            }
            collection.tracks.retain(|track| {
                format!("{} {} {}", track.title, track.artist, track.album)
                    .to_lowercase()
                    .contains(&query)
            });
            !collection.tracks.is_empty()
        });
        self
    }
}
struct LoadGuard(Arc<AtomicBool>);
impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
pub async fn load(config: Config, path: Option<PathBuf>) -> Result<Library> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = LoadGuard(cancelled.clone());
    tokio::task::spawn_blocking(move || load_sync(&config, path.as_deref(), cancelled))
        .await
        .context("Local library task failed")?
}

pub fn path(id: &str) -> Option<&Path> {
    id.strip_prefix("local:").map(Path::new)
}
pub fn is_video(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "mp4" | "mkv" | "webm" | "mov" | "m4v" | "avi" | "mpeg" | "mpg" | "ogv"
    )
}
fn is_media(path: &Path) -> bool {
    is_video(path)
        || matches!(
            extension(path).as_str(),
            "mp3"
                | "m4a"
                | "aac"
                | "flac"
                | "wav"
                | "opus"
                | "ogg"
                | "oga"
                | "wma"
                | "aiff"
                | "aif"
                | "alac"
                | "ape"
        )
}
fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_lowercase()
}
pub fn artwork(path: &Path) -> Option<PathBuf> {
    ["jpg", "jpeg", "png", "webp", "avif"]
        .iter()
        .map(|ext| path.with_extension(ext))
        .chain(
            ["cover.jpg", "cover.png", "folder.jpg", "folder.png"]
                .iter()
                .map(|name| path.parent().unwrap_or(Path::new(".")).join(name)),
        )
        .find(|candidate| candidate.is_file())
}
pub fn file_uri(path: &Path) -> Option<String> {
    reqwest::Url::from_file_path(path).ok().map(Into::into)
}
fn track(
    path: &Path,
    title: String,
    artist: String,
    album: String,
    duration: Option<f64>,
) -> Result<Track> {
    let path = path
        .canonicalize()
        .with_context(|| format!("Missing local media: {}", path.display()))?;
    ensure!(path.is_file(), "Local media is not a regular file");
    let path = path.to_str().context("Local media path is not UTF-8")?;
    Ok(Track {
        id: format!("local:{path}"),
        title,
        artist,
        album,
        duration: duration
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| {
                let n = n as u64;
                format!("{}:{:02}", n / 60, n % 60)
            })
            .unwrap_or_default(),
    })
}
fn key(root: &Path, id: &str) -> String {
    format!(
        "local-{:x}",
        md5::compute(format!("{}:{id}", root.display()))
    )
}
fn load_sync(
    config: &Config,
    override_path: Option<&Path>,
    cancelled: Arc<AtomicBool>,
) -> Result<Library> {
    let mut library = Library::default();
    let mut visited = HashSet::new();
    let mut indexed = HashSet::new();
    let mut cache = Tags::open(config.local.read_tags, cancelled.clone());
    let roots = if let Some(path) = override_path {
        vec![path.to_owned()]
    } else {
        let mut roots: Vec<_> = config.local.roots.iter().map(PathBuf::from).collect();
        if config.local.include_downloads {
            roots.extend([
                PathBuf::from(&config.downloads.audio_path),
                PathBuf::from(&config.downloads.video_path),
            ]);
        }
        roots
    };
    let mut roots = roots
        .into_iter()
        .map(|root| expand_path(&root))
        .collect::<Result<Vec<_>>>()?;
    // Prefer specific libraries before scanning their parent import folders.
    roots.sort_by_key(|root| std::cmp::Reverse(root.components().count()));
    for root in roots {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let root = expand_path(&root)?;
        if !root.exists() {
            if override_path.is_some() {
                anyhow::bail!("Local path does not exist: {}", root.display());
            }
            continue;
        }
        let root = root.canonicalize()?;
        if !visited.insert(root.clone()) {
            continue;
        }
        if root.is_file() {
            if matches!(extension(&root).as_str(), "m3u" | "m3u8") {
                library.collections.push(read_playlist(&root, &mut cache)?);
            } else {
                ensure!(is_media(&root), "Unsupported local media file");
                let item = cache.track(&root)?;
                library.collections.push(Collection {
                    id: key(&root, "file"),
                    title: item.title.clone(),
                    kind: "file".into(),
                    media_type: if is_video(&root) { "video" } else { "audio" }.into(),
                    partial: false,
                    missing: 0,
                    tracks: vec![item],
                });
            }
            continue;
        }
        let manifest = root.join("library.json");
        if manifest.is_file() {
            match read_index(&root, &mut indexed) {
                Ok(collections) => library.collections.extend(collections),
                Err(error) => library
                    .warnings
                    .push(format!("{}: {error:#}", manifest.display())),
            }
        }
        if config.local.scan_unindexed {
            let mut files = Vec::new();
            scan(
                &root,
                config.local.recursive,
                &mut files,
                &mut library.warnings,
                &cancelled,
            );
            let mut groups: BTreeMap<PathBuf, Vec<Track>> = BTreeMap::new();
            for file in files {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                if indexed.contains(&file) {
                    continue;
                }
                match cache.track(&file) {
                    Ok(item) => {
                        indexed.insert(file.clone());
                        groups
                            .entry(file.parent().unwrap_or(&root).to_owned())
                            .or_default()
                            .push(item);
                    }
                    Err(error) => library
                        .warnings
                        .push(format!("{}: {error:#}", file.display())),
                }
            }
            for (folder, tracks) in groups {
                let video = tracks.iter().any(|t| path(&t.id).is_some_and(is_video));
                let audio = tracks
                    .iter()
                    .any(|t| path(&t.id).is_some_and(|p| !is_video(p)));
                library.collections.push(Collection {
                    id: key(&folder, "folder"),
                    title: folder
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into(),
                    kind: "folder".into(),
                    media_type: if video && audio {
                        "mixed"
                    } else if video {
                        "video"
                    } else {
                        "audio"
                    }
                    .into(),
                    partial: false,
                    missing: 0,
                    tracks,
                });
            }
        }
    }
    cache.save();
    ensure!(
        !cancelled.load(Ordering::Relaxed),
        "Local library load cancelled"
    );
    library.collections.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(library)
}
fn read_index(root: &Path, indexed: &mut HashSet<PathBuf>) -> Result<Vec<Collection>> {
    let library: download_library::Library =
        serde_json::from_slice(&std::fs::read(root.join("library.json"))?)?;
    ensure!(
        library.schema_version == 1,
        "Unsupported library schema {}",
        library.schema_version
    );
    let mut collections = Vec::new();
    for mut entry in library.collections {
        entry.tracks.sort_by_key(|t| t.position);
        let mut missing = 0;
        let mut tracks = Vec::new();
        for item in entry.tracks {
            let relative = Path::new(&item.file);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                missing += 1;
                continue;
            }
            let file = root.join(relative);
            match file
                .canonicalize()
                .ok()
                .filter(|file| file.starts_with(root))
                .and_then(|file| {
                    track(
                        &file,
                        item.title,
                        item.artist,
                        item.album,
                        item.duration_seconds,
                    )
                    .ok()
                }) {
                Some(item) => {
                    indexed.insert(path(&item.id).unwrap().to_owned());
                    tracks.push(item);
                }
                None => missing += 1,
            }
        }
        collections.push(Collection {
            id: key(root, &entry.id),
            title: entry.title,
            kind: entry.kind,
            media_type: entry.media_type,
            partial: entry.partial,
            missing,
            tracks,
        });
    }
    Ok(collections)
}
fn scan(
    root: &Path,
    recursive: bool,
    files: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
    cancelled: &AtomicBool,
) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("{}: {error}", root.display()));
            return;
        }
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| natural_key(&e.file_name().to_string_lossy()));
    for entry in entries {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && recursive && !entry.file_name().to_string_lossy().starts_with('.') {
            scan(&entry.path(), recursive, files, warnings, cancelled);
        } else if kind.is_file() && is_media(&entry.path()) {
            files.push(entry.path());
        }
    }
}
fn natural_key(value: &str) -> String {
    let mut key = String::new();
    let mut digits = String::new();
    for c in value.to_lowercase().chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push_str(&format!(
                    "{:020}",
                    digits.parse::<u64>().unwrap_or(u64::MAX)
                ));
                digits.clear();
            }
            key.push(c);
        }
    }
    key
}
fn read_playlist(file: &Path, cache: &mut Tags) -> Result<Collection> {
    let source = std::fs::read_to_string(file)?;
    let mut tracks = Vec::new();
    let mut missing = 0;
    let mut title = None;
    let mut duration = None;
    for line in source.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            if let Some((seconds, name)) = info.split_once(',') {
                duration = seconds.parse::<f64>().ok();
                title = Some(name.to_owned());
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let candidate = if line.starts_with("file:") {
            reqwest::Url::parse(line)
                .ok()
                .and_then(|url| url.to_file_path().ok())
        } else if line.contains("://") {
            None
        } else {
            Some(file.parent().unwrap().join(line))
        };
        if let Some(item) = candidate.and_then(|candidate| cache.track(&candidate).ok()) {
            let mut item = item;
            if let Some(title) = title.take() {
                item.title = title;
            }
            if let Some(seconds) = duration.take().filter(|n| *n > 0.0) {
                let seconds = seconds as u64;
                item.duration = format!("{}:{:02}", seconds / 60, seconds % 60);
            }
            tracks.push(item);
        } else {
            missing += 1;
            title = None;
            duration = None;
        }
    }
    Ok(Collection {
        id: key(file, "m3u"),
        title: file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        kind: "playlist".into(),
        media_type: "mixed".into(),
        partial: missing > 0,
        missing,
        tracks,
    })
}
#[derive(Default, Serialize, Deserialize)]
struct TagCache {
    entries: BTreeMap<String, CachedTag>,
}
#[derive(Serialize, Deserialize)]
struct CachedTag {
    modified: u64,
    length: u64,
    track: Track,
}
struct Tags {
    cancelled: Arc<AtomicBool>,
    enabled: bool,
    filename: Option<PathBuf>,
    cache: TagCache,
    dirty: bool,
}
impl Tags {
    fn open(enabled: bool, cancelled: Arc<AtomicBool>) -> Self {
        let filename = if cfg!(test) {
            None
        } else {
            crate::config::config_path()
                .ok()
                .map(|p| p.with_file_name("local-tags.json"))
        };
        let cache = filename
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            cancelled,
            enabled,
            filename,
            cache,
            dirty: false,
        }
    }
    fn track(&mut self, file: &Path) -> Result<Track> {
        ensure!(
            !self.cancelled.load(Ordering::Relaxed),
            "Local library load cancelled"
        );
        let file = file.canonicalize()?;
        ensure!(
            file.is_file() && is_media(&file),
            "Missing or unsupported local media"
        );
        let name = file.to_str().context("Local path is not UTF-8")?.to_owned();
        let metadata = file.metadata()?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        if self.enabled
            && let Some(entry) = self.cache.entries.get(&name)
            && entry.modified == modified
            && entry.length == metadata.len()
        {
            return Ok(entry.track.clone());
        }
        let info = self
            .enabled
            .then(|| probe(&file, &self.cancelled))
            .flatten()
            .unwrap_or_default();
        let tags = &info["format"]["tags"];
        let text = |field: &str| {
            tags.as_object()
                .and_then(|tags| {
                    tags.iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(field))
                        .map(|(_, value)| value)
                })
                .or_else(|| {
                    info["streams"].as_array().and_then(|streams| {
                        streams.iter().find_map(|stream| {
                            stream["tags"].as_object().and_then(|tags| {
                                tags.iter()
                                    .find(|(key, _)| key.eq_ignore_ascii_case(field))
                                    .map(|(_, value)| value)
                            })
                        })
                    })
                })
                .unwrap_or(&serde_json::Value::Null)
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        };
        let item = track(
            &file,
            text("title").unwrap_or_else(|| {
                file.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            }),
            text("artist").unwrap_or_else(|| {
                file.parent()
                    .and_then(Path::file_name)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            }),
            text("album").unwrap_or_default(),
            info["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse().ok()),
        )?;
        if self.enabled {
            self.cache.entries.insert(
                name,
                CachedTag {
                    modified,
                    length: metadata.len(),
                    track: item.clone(),
                },
            );
            self.dirty = true;
        }
        Ok(item)
    }
    fn save(&self) {
        if self.dirty
            && let Some(filename) = &self.filename
            && let Some(parent) = filename.parent()
        {
            let _ = (|| -> Result<()> {
                use std::io::Write;
                std::fs::create_dir_all(parent)?;
                let mut temp = tempfile::NamedTempFile::new_in(parent)?;
                temp.write_all(&serde_json::to_vec(&self.cache)?)?;
                temp.persist(filename)?;
                Ok(())
            })();
        }
    }
}
fn probe(file: &Path, cancelled: &AtomicBool) -> Option<serde_json::Value> {
    let mut child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:format_tags=title,artist,album,track:stream_tags=title,artist,album,track",
            "-of",
            "json",
            "-i",
        ])
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .ok()
                    .and_then(|output| serde_json::from_slice(&output.stdout).ok());
            }
            Ok(None) if Instant::now() < deadline && !cancelled.load(Ordering::Relaxed) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn overlapping_import_roots_do_not_duplicate_folder_tracks() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("song.mp3"), b"audio").unwrap();
        let mut config = Config::default();
        config.local.include_downloads = false;
        config.local.read_tags = false;
        config.local.roots = vec![
            root.path().to_string_lossy().into(),
            child.to_string_lossy().into(),
        ];
        let library = load(config, None).await.unwrap();
        assert_eq!(library.collections.len(), 1);
        assert_eq!(library.collections[0].tracks.len(), 1);
    }

    #[tokio::test]
    async fn indexes_keep_repeats_skip_missing_and_filter_offline() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("song.opus"), b"media").unwrap();
        let item = json!({"id":"song","position":1,"title":"Title","artist":"Artist","album":"Album","duration_seconds":60,"source_url":"","file":"song.opus","thumbnail_file":null,"thumbnail_url":null});
        let mut repeated = item.clone();
        repeated["position"] = 2.into();
        let mut missing = item.clone();
        missing["file"] = "missing.opus".into();
        std::fs::write(root.path().join("library.json"),serde_json::to_vec(&json!({"schema_version":1,"collections":[{"id":"mix","kind":"playlist","media_type":"audio","title":"Mix","source_id":"mix","source_url":null,"partial":false,"playlist_file":null,"tracks":[repeated,item,missing]}]})).unwrap()).unwrap();
        let mut config = Config::default();
        config.local.read_tags = false;
        let library = load(config, Some(root.path().into())).await.unwrap();
        assert_eq!(library.collections.len(), 1);
        assert_eq!(library.collections[0].tracks.len(), 2);
        assert_eq!(library.collections[0].missing, 1);
        assert_eq!(library.filtered("artist").collections[0].tracks.len(), 2);
    }
    #[tokio::test]
    async fn directories_and_m3u_preserve_natural_order_and_repeats() {
        let root = tempfile::tempdir().unwrap();
        for name in ["10.mp3", "2.mp3"] {
            std::fs::write(root.path().join(name), b"audio").unwrap();
        }
        let mut config = Config::default();
        config.local.read_tags = false;
        let library = load(config.clone(), Some(root.path().into()))
            .await
            .unwrap();
        assert_eq!(library.collections[0].tracks[0].title, "2");
        let file = root.path().join("mix.m3u8");
        std::fs::write(
            &file,
            "#EXTM3U\n#EXTINF:60,Custom title\n10.mp3\n2.mp3\n10.mp3\nhttps://example.com/stream\n",
        )
        .unwrap();
        let library = load(config, Some(file)).await.unwrap();
        assert_eq!(library.collections[0].tracks.len(), 3);
        assert_eq!(library.collections[0].tracks[0].title, "Custom title");
        assert_eq!(library.collections[0].missing, 1);
    }
}
