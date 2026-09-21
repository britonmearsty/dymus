//! Best-effort local cache for views that are useful before the network responds.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::innertube::LibraryItem;

const VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
struct DiscoveryCache {
    version: u8,
    items: Vec<LibraryItem>,
}

/// Reads the most recently successful Home or Explore response. Cache failures
/// are intentionally invisible: the normal network request remains authoritative.
pub fn load_discovery(explore: bool) -> Option<Vec<LibraryItem>> {
    let path = cache_path(explore).ok()?;
    load_from(&path)
}

/// Persists a successful response so the next launch can render it immediately.
pub fn store_discovery(explore: bool, items: &[LibraryItem]) {
    let Ok(path) = cache_path(explore) else {
        return;
    };
    let _ = store_at(&path, items);
}

fn load_from(path: &Path) -> Option<Vec<LibraryItem>> {
    let cache: DiscoveryCache = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (cache.version == VERSION).then_some(cache.items)
}

fn store_at(path: &Path, items: &[LibraryItem]) -> anyhow::Result<()> {
    let parent = path.parent().expect("cache path has a parent");
    fs::create_dir_all(parent)?;
    let body = serde_json::to_vec(&DiscoveryCache {
        version: VERSION,
        items: items.to_vec(),
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&body)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn cache_path(explore: bool) -> anyhow::Result<PathBuf> {
    let base = if let Some(xdg) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        PathBuf::from(xdg)
    } else {
        PathBuf::from(
            env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("Cannot locate cache directory"))?,
        )
        .join(".cache")
    };
    Ok(base
        .join("dymus")
        .join(if explore { "explore.json" } else { "home.json" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{innertube::LibraryItem, model::track};

    #[test]
    fn cached_discovery_round_trips_and_ignores_old_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("home.json");
        let items = vec![LibraryItem {
            title: "Quick picks".into(),
            detail: "For you".into(),
            browse_id: String::new(),
            playlist_id: String::new(),
            track: Some(track("song")),
        }];
        store_at(&path, &items).unwrap();
        assert_eq!(load_from(&path).unwrap()[0].title, "Quick picks");

        fs::write(&path, r#"{"version":0,"items":[]}"#).unwrap();
        assert!(load_from(&path).is_none());
    }
}
