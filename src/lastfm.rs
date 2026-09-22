//! Last.fm protocol client.  Requests are signed locally; secrets never leave
//! this module except as the required Last.fm API signature.
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::{auth::LastFmAuth, model::Track};

const ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";

#[derive(Clone)]
pub struct Client {
    auth: LastFmAuth,
    http: reqwest::Client,
}

/// Complete Last.fm's desktop authorization flow. The URL is always printed
/// and is opened automatically when the desktop opener is available.
pub async fn authorize_interactively() -> Result<LastFmAuth> {
    ensure!(
        io::stdin().is_terminal(),
        "Last.fm setup must run in a terminal"
    );
    let api_key = rpassword::prompt_password("Last.fm API key: ")?;
    let shared_secret = rpassword::prompt_password("Last.fm shared secret: ")?;
    ensure!(
        !api_key.trim().is_empty(),
        "Last.fm API key cannot be empty"
    );
    ensure!(
        !shared_secret.trim().is_empty(),
        "Last.fm shared secret cannot be empty"
    );
    let http = reqwest::Client::new();
    let token = unsigned_call(
        &http,
        &api_key,
        &shared_secret,
        "auth.getToken",
        BTreeMap::new(),
    )
    .await?["token"]
        .as_str()
        .map(str::to_owned)
        .context("Last.fm did not return an authorization token")?;
    let url = format!("https://www.last.fm/api/auth/?api_key={api_key}&token={token}");
    println!("Authorize Dymus in your browser. If it does not open, use:");
    println!("{url}");
    match Command::new("xdg-open").arg(&url).spawn() {
        Ok(mut child) => {
            // xdg-open normally returns as soon as it hands off to the desktop;
            // do not let a broken opener delay or block terminal setup.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(_) => eprintln!("Could not open a browser automatically; use the URL above."),
    }
    print!("Waiting for approval — press Enter when finished (Ctrl-C cancels): ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let response = unsigned_call(
        &http,
        &api_key,
        &shared_secret,
        "auth.getSession",
        BTreeMap::from([("token".into(), token)]),
    )
    .await?;
    let session_key = response["session"]["key"]
        .as_str()
        .map(str::to_owned)
        .context("Last.fm did not return a session key; make sure access was approved")?;
    Ok(LastFmAuth {
        api_key,
        shared_secret,
        session_key,
    })
}

async fn unsigned_call(
    http: &reqwest::Client,
    api_key: &str,
    secret: &str,
    method: &str,
    mut params: BTreeMap<String, String>,
) -> Result<Value> {
    params.insert("api_key".into(), api_key.into());
    params.insert("method".into(), method.into());
    params.insert("api_sig".into(), signature(&params, secret));
    params.insert("format".into(), "json".into());
    let value: Value = http
        .post(ENDPOINT)
        .form(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(error) = value["error"].as_i64() {
        bail!(
            "Last.fm error {error}: {}",
            value["message"].as_str().unwrap_or("unknown error")
        );
    }
    Ok(value)
}

impl Client {
    pub fn new(auth: LastFmAuth) -> Result<Self> {
        ensure!(
            !auth.api_key.trim().is_empty(),
            "Last.fm API key cannot be empty"
        );
        ensure!(
            !auth.shared_secret.trim().is_empty(),
            "Last.fm shared secret cannot be empty"
        );
        ensure!(
            !auth.session_key.trim().is_empty(),
            "Last.fm session key cannot be empty"
        );
        Ok(Self {
            auth,
            http: reqwest::Client::builder().build()?,
        })
    }

    pub async fn verify(&self) -> Result<String> {
        let response = self.call("user.getInfo", BTreeMap::new()).await?;
        response["user"]["name"]
            .as_str()
            .map(str::to_owned)
            .context("Last.fm did not return an account name")
    }

    pub async fn now_playing(&self, track: &Track, duration: u64) -> Result<()> {
        let mut params = self.track_params(track, duration);
        self.call("track.updateNowPlaying", std::mem::take(&mut params))
            .await?;
        Ok(())
    }

    pub async fn scrobble(&self, track: &Track, duration: u64, started_at: u64) -> Result<()> {
        let mut params = self.track_params(track, duration);
        params.insert("timestamp".into(), started_at.to_string());
        self.call("track.scrobble", params).await?;
        Ok(())
    }

    fn track_params(&self, track: &Track, duration: u64) -> BTreeMap<String, String> {
        let mut params = BTreeMap::from([
            ("artist".into(), track.artist.clone()),
            ("track".into(), track.title.clone()),
        ]);
        if !track.album.trim().is_empty() {
            params.insert("album".into(), track.album.clone());
        }
        if duration > 0 {
            params.insert("duration".into(), duration.to_string());
        }
        params
    }

    async fn call(&self, method: &str, mut params: BTreeMap<String, String>) -> Result<Value> {
        params.insert("api_key".into(), self.auth.api_key.clone());
        params.insert("method".into(), method.into());
        params.insert("sk".into(), self.auth.session_key.clone());
        let signature = signature(&params, &self.auth.shared_secret);
        params.insert("api_sig".into(), signature);
        params.insert("format".into(), "json".into());
        let value: Value = self
            .http
            .post(ENDPOINT)
            .form(&params)
            .send()
            .await
            .context("Cannot reach Last.fm")?
            .error_for_status()
            .context("Last.fm rejected the HTTP request")?
            .json()
            .await
            .context("Last.fm returned invalid JSON")?;
        if let Some(error) = value["error"].as_i64() {
            bail!(
                "Last.fm error {error}: {}",
                value["message"].as_str().unwrap_or("unknown error")
            );
        }
        Ok(value)
    }
}

/// Last.fm's API signature is an MD5 of sorted key/value pairs plus the secret.
fn signature(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut source = String::new();
    for (key, value) in params {
        source.push_str(key);
        source.push_str(value);
    }
    source.push_str(secret);
    format!("{:x}", md5::compute(source))
}

pub fn duration_seconds(track: &Track) -> u64 {
    track
        .duration
        .split(':')
        .try_fold(0u64, |total, part| {
            part.parse::<u64>().ok().map(|seconds| total * 60 + seconds)
        })
        .unwrap_or_default()
}

/// Last.fm accepts tracks longer than 30s after half their duration or 4 min,
/// whichever comes first.
pub fn eligible_after(duration: u64) -> Option<u64> {
    (duration > 30).then(|| (duration / 2).min(240))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signs_sorted_parameters() {
        let params = BTreeMap::from([("b".into(), "two".into()), ("a".into(), "one".into())]);
        assert_eq!(
            signature(&params, "secret"),
            "e9e7274870c1332e02554d6b5715657c"
        );
    }
    #[test]
    fn eligibility_matches_lastfm_rules() {
        assert_eq!(eligible_after(30), None);
        assert_eq!(eligible_after(240), Some(120));
        assert_eq!(eligible_after(1000), Some(240));
    }
}
