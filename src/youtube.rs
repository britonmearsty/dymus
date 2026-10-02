//! Full YouTube search for the headless result picker.
use crate::{auth, model::Track, player};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::{collections::HashSet, process::Stdio, time::Duration};
use tokio::process::Command;

pub async fn search(query: &str, limit: usize) -> Result<Vec<Track>> {
    ensure!(!query.trim().is_empty(), "Enter a YouTube search query");
    ensure!(
        (1..=50).contains(&limit),
        "YouTube search limit must be between 1 and 50"
    );
    let cookies = auth::load()?
        .map(|auth| player::youtube_cookie_jar(auth.cookie()))
        .transpose()?;
    let mut command = Command::new("yt-dlp");
    command.as_std_mut().process_group(0);
    command.args([
        "--ignore-config",
        "--flat-playlist",
        "--skip-download",
        "--dump-single-json",
        "--no-warnings",
        "--no-mark-watched",
    ]);
    if let Some(cookies) = &cookies {
        command.arg("--cookies").arg(cookies.path());
    }
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        command
            .arg("--")
            .arg(format!("ytsearch{limit}:{}", query.trim()))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("YouTube search timed out; try again")?
    .context("Cannot start yt-dlp; run `dymus doctor`")?;
    if !output.status.success() {
        bail!(
            "YouTube search failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let response: Value = serde_json::from_slice(&output.stdout)
        .context("yt-dlp returned invalid YouTube search JSON")?;
    parse_results(&response, limit)
}

fn parse_results(response: &Value, limit: usize) -> Result<Vec<Track>> {
    let entries = response["entries"]
        .as_array()
        .context("YouTube search has no result list")?;
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    for entry in entries {
        let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(title) = entry["title"]
            .as_str()
            .filter(|title| !title.trim().is_empty())
        else {
            continue;
        };
        if matches!(
            entry["availability"].as_str(),
            Some("private" | "premium_only" | "subscriber_only" | "needs_auth")
        ) || !seen.insert(id.to_owned())
        {
            continue;
        }
        let duration = entry["duration"]
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| {
                let seconds = seconds as u64;
                format!("{}:{:02}", seconds / 60, seconds % 60)
            })
            .unwrap_or_default();
        tracks.push(Track {
            id: id.into(),
            title: title.into(),
            artist: entry["channel"]
                .as_str()
                .or_else(|| entry["uploader"].as_str())
                .unwrap_or("Unknown channel")
                .into(),
            album: String::new(),
            duration,
        });
        if tracks.len() == limit {
            break;
        }
    }
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_youtube_results_in_order_with_its_own_cap() {
        let response = json!({"entries": [
            null, {"id":"private", "title":"Hidden", "availability":"private"},
            {"id":"one", "title":"Tutorial", "channel":"Creator", "duration":125.5},
            {"id":"one", "title":"Duplicate"}, {"id":"missing-title"},
            {"id":"two", "title":"Interview", "uploader":"Host"},
            {"id":"three", "title":"Other"}
        ]});
        let tracks = parse_results(&response, 2).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "Tutorial");
        assert_eq!(tracks[0].artist, "Creator");
        assert_eq!(tracks[0].duration, "2:05");
        assert_eq!(tracks[1].id, "two");
        assert_eq!(tracks[1].artist, "Host");
        assert!(tracks[1].duration.is_empty());
    }

    #[test]
    fn distinguishes_empty_results_from_malformed_responses() {
        assert!(
            parse_results(&json!({"entries":[]}), 10)
                .unwrap()
                .is_empty()
        );
        assert!(parse_results(&json!({}), 10).is_err());
    }
}
