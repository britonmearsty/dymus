//! Best-effort local cache for views that are useful before the network responds.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{innertube::DiscoveryPage, model::Track};

const VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
struct DiscoveryCache {
    version: u8,
    page: DiscoveryPage,
}

/// Reads the most recently successful Home or Explore response. Cache failures
/// are intentionally invisible: the normal network request remains authoritative.
pub fn load_discovery(explore: bool) -> Option<DiscoveryPage> {
    let path = cache_path(explore).ok()?;
    load_from(&path)
}

/// Persists a successful response so the next launch can render it immediately.
pub fn store_discovery(explore: bool, page: &DiscoveryPage) {
    let Ok(path) = cache_path(explore) else {
        return;
    };
    let _ = store_at(&path, page);
}

/// The playback queue is small, private local state. It is restored as
/// upcoming items only, so launching Dymus never starts media unexpectedly.
pub fn load_queue() -> Option<Vec<Track>> {
    #[cfg(test)]
    return None;
    #[cfg(not(test))]
    {
        let path = cache_root().ok()?.join("queue.json");
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }
}

pub fn store_queue(queue: &[Track]) {
    #[cfg(test)]
    {
        let _ = queue;
    }
    #[cfg(not(test))]
    {
        let Ok(root) = cache_root() else { return };
        let _ = fs::create_dir_all(&root);
        let Ok(mut temporary) = tempfile::NamedTempFile::new_in(&root) else {
            return;
        };
        if serde_json::to_writer(&mut temporary, queue).is_ok() {
            let _ = temporary.persist(root.join("queue.json"));
        }
    }
}

fn load_from(path: &Path) -> Option<DiscoveryPage> {
    let cache: DiscoveryCache = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (cache.version == VERSION).then_some(cache.page)
}

fn store_at(path: &Path, page: &DiscoveryPage) -> anyhow::Result<()> {
    let parent = path.parent().expect("cache path has a parent");
    fs::create_dir_all(parent)?;
    let body = serde_json::to_vec(&DiscoveryCache {
        version: VERSION,
        page: page.clone(),
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&body)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn cache_path(explore: bool) -> anyhow::Result<PathBuf> {
    Ok(cache_root()?.join(if explore { "explore.json" } else { "home.json" }))
}

fn cache_root() -> anyhow::Result<PathBuf> {
    let base = if let Some(xdg) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        PathBuf::from(xdg)
    } else {
        PathBuf::from(
            env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("Cannot locate cache directory"))?,
        )
        .join(".cache")
    };
    Ok(base.join("dymus"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        innertube::{DiscoveryPage, LibraryItem},
        model::track,
    };

    #[test]
    fn cached_discovery_round_trips_and_ignores_old_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("home.json");
        let page = DiscoveryPage {
            items: vec![LibraryItem {
                section: "Quick picks".into(),
                title: "Quick picks".into(),
                detail: "For you".into(),
                browse_id: String::new(),
                playlist_id: String::new(),
                track: Some(track("song")),
            }],
            continuations: Vec::new(),
        };
        store_at(&path, &page).unwrap();
        assert_eq!(load_from(&path).unwrap().items[0].title, "Quick picks");

        fs::write(&path, r#"{"version":0,"items":[]}"#).unwrap();
        assert!(load_from(&path).is_none());
    }
}
