use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://lrclib.net/api/get";

#[derive(Clone, Debug, Deserialize)]
pub struct Lyrics {
    #[serde(rename = "plainLyrics")]
    pub plain: Option<String>,
    #[serde(rename = "syncedLyrics")]
    synced: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedLine {
    pub at: f64,
    pub text: String,
}

impl Lyrics {
    pub fn synced_lines(&self) -> Vec<TimedLine> {
        self.synced.as_deref().map(parse_lrc).unwrap_or_default()
    }

    pub fn has_synced(&self) -> bool {
        self.synced
            .as_deref()
            .is_some_and(|s| !parse_lrc(s).is_empty())
    }
}

pub async fn fetch(
    title: &str,
    artist: &str,
    album: &str,
    duration: f64,
) -> Result<Option<Lyrics>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("dymus/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut request = client
        .get(API)
        .query(&[("track_name", title), ("artist_name", artist)]);
    if !album.is_empty() {
        request = request.query(&[("album_name", album)]);
    }
    if duration > 0.0 {
        request = request.query(&[("duration", &duration.round().to_string())]);
    }
    let response = request.send().await.context("requesting LRCLIB lyrics")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .context("LRCLIB returned an error")?;
    let lyrics = response
        .json::<Lyrics>()
        .await
        .context("reading LRCLIB response")?;
    if lyrics.plain.as_deref().is_none_or(str::is_empty) && !lyrics.has_synced() {
        bail!("LRCLIB returned an empty lyrics record");
    }
    Ok(Some(lyrics))
}

pub fn parse_lrc(source: &str) -> Vec<TimedLine> {
    let mut lines = Vec::new();
    for raw in source.lines() {
        let mut rest = raw.trim();
        let mut timestamps = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            if let Some(timestamp) = parse_timestamp(tag) {
                timestamps.push(timestamp);
            } else {
                break;
            }
            rest = rest[end + 1..].trim_start();
        }
        for at in timestamps {
            lines.push(TimedLine {
                at,
                text: rest.to_owned(),
            });
        }
    }
    lines.sort_by(|a, b| a.at.total_cmp(&b.at));
    lines
}

fn parse_timestamp(tag: &str) -> Option<f64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<f64>().ok()?;
    if !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(minutes as f64 * 60.0 + seconds)
}

pub fn active_line(lines: &[TimedLine], position: f64) -> Option<usize> {
    lines
        .partition_point(|line| line.at <= position)
        .checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamped_lines_and_multiple_timestamps() {
        assert_eq!(
            parse_lrc("[ar:artist]\n[00:01.20]First\n[00:03.5][00:04.00]Repeat"),
            vec![
                TimedLine {
                    at: 1.2,
                    text: "First".into()
                },
                TimedLine {
                    at: 3.5,
                    text: "Repeat".into()
                },
                TimedLine {
                    at: 4.0,
                    text: "Repeat".into()
                },
            ]
        );
    }

    #[test]
    fn finds_current_synced_line() {
        let lines = parse_lrc("[00:01.00]One\n[00:02.00]Two");
        assert_eq!(active_line(&lines, 0.5), None);
        assert_eq!(active_line(&lines, 1.5), Some(0));
        assert_eq!(active_line(&lines, 2.0), Some(1));
    }
}
