//! Versioned, portable metadata for the future local library reader.
use crate::{config::DownloadConfig, model::Track};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Library {
    pub(crate) schema_version: u32,
    pub(crate) collections: Vec<Collection>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Collection {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) media_type: String,
    pub(crate) title: String,
    pub(crate) source_id: String,
    pub(crate) source_url: Option<String>,
    pub(crate) partial: bool,
    pub(crate) playlist_file: Option<String>,
    pub(crate) tracks: Vec<LocalTrack>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct LocalTrack {
    pub(crate) id: String,
    pub(crate) position: usize,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_seconds: Option<f64>,
    pub(crate) source_url: String,
    pub(crate) file: String,
    pub(crate) thumbnail_file: Option<String>,
    pub(crate) thumbnail_url: Option<String>,
}

pub(crate) struct DownloadContext {
    pub kind: &'static str,
    pub video: bool,
    pub title: String,
    pub source_id: String,
    pub source_url: Option<String>,
    pub position: usize,
    pub fallback: Option<Track>,
}
pub(crate) struct Recorder {
    root: PathBuf,
    library: Library,
    manifests: bool,
    playlists: bool,
    reconcile: bool,
    seen: BTreeMap<String, BTreeSet<usize>>,
}
impl Recorder {
    pub fn open(root: &Path, settings: &DownloadConfig) -> Result<Self> {
        let library = match std::fs::read(root.join("library.json")) {
            Ok(bytes) => {
                let library: Library = serde_json::from_slice(&bytes).context(
                    "Cannot read local library.json; existing metadata was left unchanged",
                )?;
                ensure!(
                    library.schema_version == 1,
                    "Unsupported local library schema; existing metadata was left unchanged"
                );
                library
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Library {
                schema_version: 1,
                ..Default::default()
            },
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            root: root.into(),
            library,
            manifests: settings.write_manifest,
            playlists: settings.write_playlist,
            reconcile: !settings.skip_downloaded,
            seen: BTreeMap::new(),
        })
    }
    pub fn record(&mut self, info: &Value, context: &DownloadContext) -> Result<()> {
        if !self.manifests && !self.playlists {
            return Ok(());
        }
        let file = Path::new(
            info["filepath"]
                .as_str()
                .context("Download result has no filepath")?,
        );
        ensure!(
            file.is_file(),
            "Completed download file is missing: {}",
            file.display()
        );
        let relative = file
            .strip_prefix(&self.root)
            .context("Download result is outside its destination")?
            .to_str()
            .context("Download path is not UTF-8")?
            .to_owned();
        ensure!(
            !relative.contains(['\r', '\n']),
            "Download filename cannot be represented in an M3U playlist"
        );
        let source_id = if context.kind == "singles" {
            "singles".into()
        } else {
            text(info, "playlist_id").unwrap_or_else(|| context.source_id.clone())
        };
        let title = if context.kind == "singles" {
            "Singles".into()
        } else {
            text(info, "playlist_title").unwrap_or_else(|| context.title.clone())
        };
        let media = if context.video { "video" } else { "audio" };
        let id = format!(
            "{:x}",
            md5::compute(format!("{}:{media}:{source_id}", context.kind))
        );
        let index = match self.library.collections.iter().position(|c| c.id == id) {
            Some(index) => index,
            None => {
                self.library.collections.push(Collection {
                    id: id.clone(),
                    kind: context.kind.into(),
                    media_type: media.into(),
                    title: title.clone(),
                    source_id,
                    source_url: context.source_url.clone(),
                    partial: true,
                    playlist_file: None,
                    tracks: Vec::new(),
                });
                self.library.collections.len() - 1
            }
        };
        let collection = &mut self.library.collections[index];
        collection.title = title;
        collection.partial = true;
        let track_id = text(info, "id").context("Downloaded track has no source ID")?;
        let position = if context.kind == "singles" {
            collection
                .tracks
                .iter()
                .find(|t| t.id == track_id)
                .map(|t| t.position)
                .unwrap_or_else(|| {
                    collection
                        .tracks
                        .iter()
                        .map(|t| t.position)
                        .max()
                        .unwrap_or(0)
                        + 1
                })
        } else {
            info["playlist_index"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or(context.position)
        };
        let fallback = context.fallback.as_ref();
        let track = LocalTrack {
            id: track_id,
            position,
            title: text(info, "title")
                .or_else(|| fallback.map(|t| t.title.clone()))
                .unwrap_or_default(),
            artist: text(info, "artist")
                .or_else(|| {
                    fallback
                        .filter(|t| !t.artist.is_empty())
                        .map(|t| t.artist.clone())
                })
                .or_else(|| text(info, "uploader"))
                .unwrap_or_else(|| "Unknown Artist".into()),
            album: text(info, "album")
                .or_else(|| {
                    fallback
                        .filter(|t| !t.album.is_empty())
                        .map(|t| t.album.clone())
                })
                .unwrap_or_else(|| {
                    if context.kind == "album" {
                        collection.title.clone()
                    } else {
                        String::new()
                    }
                }),
            duration_seconds: info["duration"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0),
            source_url: text(info, "webpage_url").unwrap_or_default(),
            file: relative,
            thumbnail_file: ["jpg", "jpeg", "png", "webp", "avif"]
                .into_iter()
                .map(|ext| file.with_extension(ext))
                .find(|path| path.is_file())
                .and_then(|path| {
                    path.strip_prefix(&self.root)
                        .ok()
                        .and_then(Path::to_str)
                        .map(str::to_owned)
                }),
            thumbnail_url: text(info, "thumbnail"),
        };
        collection.tracks.retain(|t| t.position != position);
        collection.tracks.push(track);
        collection.tracks.sort_by_key(|t| t.position);
        self.seen.entry(id).or_default().insert(position);
        self.persist()
    }
    pub fn finish(&mut self, successful: bool) -> Result<()> {
        for collection in &mut self.library.collections {
            if let Some(seen) = self.seen.get(&collection.id) {
                collection.partial = !successful;
                if successful && self.reconcile && collection.kind != "singles" {
                    collection
                        .tracks
                        .retain(|track| seen.contains(&track.position));
                }
            }
        }
        self.persist()
    }
    fn persist(&mut self) -> Result<()> {
        if self.playlists {
            for collection in &mut self.library.collections {
                if !self.seen.contains_key(&collection.id) {
                    continue;
                }
                let relative = format!(".dymus/playlists/{}.m3u8", collection.id);
                let mut playlist = String::from("#EXTM3U\n");
                for track in &collection.tracks {
                    // Playlist files live two directories below the library root.
                    playlist.push_str(&format!(
                        "#EXTINF:{},{} - {}\n../../{}\n",
                        track.duration_seconds.map(|n| n as i64).unwrap_or(-1),
                        clean(&track.artist),
                        clean(&track.title),
                        track.file
                    ));
                }
                atomic_write(&self.root.join(&relative), playlist.as_bytes())?;
                collection.playlist_file = Some(relative);
            }
        }
        if self.manifests {
            atomic_write(
                &self.root.join("library.json"),
                &serde_json::to_vec_pretty(&self.library)?,
            )?;
        }
        Ok(())
    }
}
fn text(info: &Value, field: &str) -> Option<String> {
    info[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing metadata directory")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .with_context(|| format!("Cannot save {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn portable_collections_preserve_order_repeats_and_partial_results() {
        let root = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::open(root.path(), &DownloadConfig::default()).unwrap();
        let context = DownloadContext {
            kind: "playlist",
            video: false,
            title: "Mix".into(),
            source_id: "mix-id".into(),
            source_url: None,
            position: 1,
            fallback: None,
        };
        for position in [3, 1, 2] {
            let path = root.path().join(format!("{position}.opus"));
            std::fs::write(&path, b"audio").unwrap();
            recorder.record(&json!({"id":"same-song", "title":"Song", "artist":"Artist", "filepath":path, "playlist_index":position, "duration":60}), &context).unwrap();
        }
        recorder.finish(false).unwrap();
        let library: Library =
            serde_json::from_slice(&std::fs::read(root.path().join("library.json")).unwrap())
                .unwrap();
        assert!(library.collections[0].partial);
        assert_eq!(
            library.collections[0]
                .tracks
                .iter()
                .map(|t| t.position)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(library.collections[0].tracks[0].file, "1.opus");
        let playlist = std::fs::read_to_string(
            root.path()
                .join(library.collections[0].playlist_file.as_ref().unwrap()),
        )
        .unwrap();
        assert!(playlist.contains("../../1.opus\n"));
        recorder.finish(true).unwrap();
        let mut recorder = Recorder::open(root.path(), &DownloadConfig::default()).unwrap();
        let path = root.path().join("2.opus");
        recorder
            .record(
                &json!({"id":"same-song", "title":"Song", "filepath":path, "playlist_index":2}),
                &context,
            )
            .unwrap();
        recorder.finish(true).unwrap();
        assert_eq!(recorder.library.collections[0].tracks.len(), 1);
    }
}
