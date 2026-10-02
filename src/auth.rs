//! Local browser-session authentication. Cookie data is never printed or logged.
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

const ORIGIN: &str = "https://music.youtube.com";

#[derive(Clone, Deserialize, Serialize)]
pub struct BrowserAuth {
    cookie: String,
    #[serde(default = "default_auth_user")]
    auth_user: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct LastFmAuth {
    pub api_key: String,
    pub shared_secret: String,
    pub session_key: String,
}

#[derive(Deserialize, Serialize)]
struct AuthFile {
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default = "default_auth_user")]
    auth_user: String,
    #[serde(default)]
    lastfm: Option<LastFmAuth>,
}

fn default_auth_user() -> String {
    "0".into()
}

impl BrowserAuth {
    pub fn from_cookie(cookie: String, auth_user: &str) -> Result<Self> {
        ensure!(!cookie.trim().is_empty(), "Cookie cannot be empty");
        ensure!(
            cookie.len() <= 65_536,
            "Cookie header is unexpectedly large"
        );
        ensure!(
            auth_user.bytes().all(|byte| byte.is_ascii_digit()),
            "Account index must contain only digits"
        );
        ensure!(
            cookie.lines().count() == 1,
            "Paste only the Cookie header value on one line"
        );
        ensure!(
            cookie
                .bytes()
                .all(|byte| byte.is_ascii() && !byte.is_ascii_control()),
            "Cookie header contains invalid characters"
        );
        let has_sapisid = cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .any(|(name, value)| name.trim() == "__Secure-3PAPISID" && !value.trim().is_empty());
        ensure!(
            has_sapisid,
            "Cookie is missing __Secure-3PAPISID; copy it from a signed-in YouTube Music request"
        );
        Ok(Self {
            cookie,
            auth_user: auth_user.into(),
        })
    }

    pub fn authorization(&self, timestamp: u64) -> Result<String> {
        let sapisid = self
            .cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| name.trim() == "__Secure-3PAPISID")
            .map(|(_, value)| value.trim())
            .context("Cookie is missing __Secure-3PAPISID")?;
        let input = format!("{timestamp} {sapisid} {ORIGIN}");
        let digest = Sha1::digest(input.as_bytes());
        Ok(format!("SAPISIDHASH {timestamp}_{}", hex(&digest)))
    }

    pub fn cookie(&self) -> &str {
        &self.cookie
    }
    pub fn auth_user(&self) -> &str {
        &self.auth_user
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

fn config_dir() -> Result<PathBuf> {
    let base = if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        PathBuf::from(xdg)
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .context("Cannot locate your configuration directory")?
            .join(".config")
    };
    Ok(base.join("dymus"))
}

fn auth_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("auth.json"))
}

pub fn load() -> Result<Option<BrowserAuth>> {
    let path = auth_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}", path.display()));
        }
    };
    ensure!(
        metadata.file_type().is_file(),
        "Dymus auth file must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "Dymus auth file permissions are too open; run `chmod 600 {}`",
            path.display()
        );
    }
    let file = fs::File::open(path)?;
    let auth: AuthFile = serde_json::from_reader(file)
        .context("Dymus auth file is invalid; run `dymus auth paste` again")?;
    let cookie = auth
        .cookie
        .context("No YouTube Music credentials are configured")?;
    BrowserAuth::from_cookie(cookie, &auth.auth_user)
        .context("Dymus auth file contains invalid credentials")
        .map(Some)
}

pub fn load_lastfm() -> Result<Option<LastFmAuth>> {
    let path = auth_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}", path.display()));
        }
    };
    ensure!(
        metadata.file_type().is_file(),
        "Dymus auth file must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "Dymus auth file permissions are too open; run `chmod 600 {}`",
            path.display()
        );
    }
    Ok(serde_json::from_reader::<_, AuthFile>(fs::File::open(path)?)?.lastfm)
}

pub fn save(auth: &BrowserAuth) -> Result<()> {
    let directory = config_dir()?;
    fs::create_dir_all(&directory).context("Cannot create Dymus configuration directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.file_type().is_dir(),
            "Dymus configuration path must be a real directory"
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .context("Cannot create private credentials file")?;
    let lastfm = load_lastfm().unwrap_or(None);
    serde_json::to_writer(
        &mut temporary,
        &AuthFile {
            cookie: Some(auth.cookie.clone()),
            auth_user: auth.auth_user.clone(),
            lastfm,
        },
    )
    .context("Cannot encode authentication data")?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(auth_path()?)
        .context("Cannot save Dymus credentials")?;
    Ok(())
}

pub fn save_lastfm(lastfm: LastFmAuth) -> Result<()> {
    let directory = config_dir()?;
    fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let existing = load_auth_file()?.unwrap_or(AuthFile {
        cookie: None,
        auth_user: default_auth_user(),
        lastfm: None,
    });
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(
        &mut temporary,
        &AuthFile {
            cookie: existing.cookie,
            auth_user: existing.auth_user,
            lastfm: Some(lastfm),
        },
    )?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(auth_path()?)?;
    Ok(())
}

fn load_auth_file() -> Result<Option<AuthFile>> {
    let path = auth_path()?;
    match fs::File::open(path) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn logout_lastfm() -> Result<bool> {
    let Some(mut auth) = load_auth_file()? else {
        return Ok(false);
    };
    if auth.lastfm.take().is_none() {
        return Ok(false);
    }
    if auth.cookie.is_none() {
        fs::remove_file(auth_path()?).context("Cannot remove Last.fm credentials")?;
        return Ok(true);
    }
    let directory = config_dir()?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(&mut temporary, &auth)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(auth_path()?)?;
    Ok(true)
}

pub fn prompt_lastfm() -> Result<LastFmAuth> {
    ensure!(
        io::stdin().is_terminal(),
        "Last.fm setup must run in a terminal"
    );
    Ok(LastFmAuth {
        api_key: rpassword::prompt_password("Last.fm API key: ")?,
        shared_secret: rpassword::prompt_password("Last.fm shared secret: ")?,
        session_key: rpassword::prompt_password("Last.fm session key: ")?,
    })
}

pub fn logout() -> Result<bool> {
    let path = auth_path()?;
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("Cannot remove Dymus credentials"),
    }
}

pub fn prompt_cookie() -> Result<String> {
    ensure!(
        io::stdin().is_terminal(),
        "Authentication setup must run in a terminal so the cookie is not echoed"
    );
    let cookie = rpassword::prompt_password("Paste the Cookie header value (input hidden): ")
        .context("Cannot read the cookie from the terminal")?;
    Ok(cookie.trim().to_owned())
}

pub fn validate_input() -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "Authentication setup must run in a terminal so the cookie is not echoed"
    );
    Ok(())
}

/// Import only cookies applicable to YouTube Music from a yt-dlp browser export.
pub async fn from_browser(
    browser: &str,
    profile: Option<&str>,
    container: Option<&str>,
    keyring: Option<&str>,
    auth_user: &str,
) -> Result<BrowserAuth> {
    const BROWSERS: &[&str] = &[
        "brave", "chrome", "chromium", "edge", "firefox", "opera", "safari", "vivaldi", "whale",
    ];
    ensure!(BROWSERS.contains(&browser), "Unsupported browser name");
    ensure!(
        auth_user.bytes().all(|byte| byte.is_ascii_digit()),
        "Account index must contain only digits"
    );
    ensure!(
        container.is_none() || browser == "firefox",
        "Firefox containers can only be used with --browser firefox"
    );
    if let Some(profile) = profile {
        ensure!(
            !profile.contains(':') && !profile.is_empty(),
            "Browser profile cannot be empty or contain ':'"
        );
    }
    if let Some(container) = container {
        ensure!(
            !container.contains(':') && !container.is_empty(),
            "Firefox container cannot be empty or contain ':'"
        );
    }
    if let Some(keyring) = keyring {
        ensure!(
            [
                "basictext",
                "gnomekeyring",
                "kwallet",
                "kwallet5",
                "kwallet6"
            ]
            .contains(&keyring),
            "Unsupported yt-dlp keyring"
        );
    }
    let mut source = String::from(browser);
    if let Some(keyring) = keyring {
        source.push('+');
        source.push_str(keyring);
    }
    if let Some(profile) = profile {
        source.push(':');
        source.push_str(profile);
    }
    if let Some(container) = container {
        source.push_str("::");
        source.push_str(container);
    }

    let directory =
        tempfile::tempdir().context("Cannot create a private temporary cookie directory")?;
    let cookie_file = directory.path().join("cookies.txt");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        tokio::process::Command::new("yt-dlp")
            .args(["--ignore-config", "--no-warnings", "--cookies-from-browser"])
            .arg(source)
            .arg("--cookies")
            .arg(&cookie_file)
            .arg("--skip-download")
            .arg("--simulate")
            .arg("https://music.youtube.com/")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Timed out reading browser cookies; close the browser and try again")?
    .context("Cannot start yt-dlp; install yt-dlp to import browser authentication")?;
    ensure!(
        output.status.success(),
        "yt-dlp could not read browser cookies; close the browser, check the profile and keyring, and try again"
    );
    let jar = fs::read_to_string(&cookie_file)
        .context("yt-dlp did not create a browser cookie export")?;
    let cookie = youtube_cookie_header(&jar)?;
    BrowserAuth::from_cookie(cookie, auth_user)
}

fn youtube_cookie_header(jar: &str) -> Result<String> {
    let mut cookies = Vec::<(String, String)>::new();
    let now = unix_timestamp()?;
    let mut has_header = false;
    for line in jar.lines() {
        if line.starts_with('#') && !line.starts_with("#HttpOnly_") {
            if line.contains("Netscape HTTP Cookie File") || line.contains("Netscape cookie file") {
                has_header = true;
            }
            continue;
        }
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 7 {
            continue;
        }
        let domain = fields[0].trim_start_matches('.').to_ascii_lowercase();
        let include_subdomains = fields[1] == "TRUE";
        let path = fields[2];
        let secure = fields[3] == "TRUE";
        let expiry = fields[4].parse::<u64>().unwrap_or(0);
        let name = fields[5];
        let value = fields[6];
        let host_matches = "music.youtube.com" == domain
            || (include_subdomains && "music.youtube.com".ends_with(&format!(".{domain}")));
        let path_matches = path.starts_with('/') && "/".starts_with(path);
        if host_matches
            && path_matches
            && secure
            && (expiry == 0 || expiry > now)
            && !name.is_empty()
            && !name.contains([';', '='])
            && !value.contains([';', '\r', '\n'])
        {
            if let Some(existing) = cookies.iter_mut().find(|(existing, _)| existing == name) {
                existing.1 = value.to_owned();
            } else {
                cookies.push((name.to_owned(), value.to_owned()));
            }
        }
    }
    ensure!(
        has_header,
        "yt-dlp returned an invalid browser cookie export"
    );
    ensure!(
        !cookies.is_empty(),
        "No valid YouTube Music cookies were found in the selected browser"
    );
    Ok(cookies
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

pub fn unix_timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_with_the_secure_sapisid_cookie_and_music_origin() {
        let auth =
            BrowserAuth::from_cookie("SID=session; __Secure-3PAPISID=abc/def".into(), "0").unwrap();
        assert_eq!(
            auth.authorization(1_700_000_000).unwrap(),
            "SAPISIDHASH 1700000000_523b01f033dd8ed738175261f406736fe239c73d"
        );
    }

    #[test]
    fn rejects_missing_sapisid_multiline_input_and_invalid_account_index() {
        assert!(BrowserAuth::from_cookie("SID=ordinary".into(), "0").is_err());
        assert!(
            BrowserAuth::from_cookie("__Secure-3PAPISID=value\nInjected: header".into(), "0")
                .is_err()
        );
        assert!(
            BrowserAuth::from_cookie("__Secure-3PAPISID=value".into(), "0\r\nInjected").is_err()
        );
    }

    #[test]
    fn preserves_other_cookie_fields_and_selected_google_account() {
        let auth = BrowserAuth::from_cookie("YSC=abc; __Secure-3PAPISID=def".into(), "2").unwrap();
        assert_eq!(auth.cookie(), "YSC=abc; __Secure-3PAPISID=def");
        assert_eq!(auth.auth_user(), "2");
    }

    #[test]
    fn browser_export_keeps_only_secure_unexpired_youtube_cookies() {
        let now = unix_timestamp().unwrap();
        let jar = format!(
            "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tmain\n#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PAPISID\tsecret\n.other.test\tTRUE\t/\tTRUE\t0\tLEAK\tno\n.youtube.com\tFALSE\t/\tFALSE\t0\tINSECURE\tno\n.youtube.com\tTRUE\t/\tTRUE\t{}\tEXPIRED\tno\n",
            now - 1
        );
        let header = youtube_cookie_header(&jar).unwrap();
        assert!(header.contains("SID=main"));
        assert!(header.contains("__Secure-3PAPISID=secret"));
        assert!(!header.contains("LEAK"));
        assert!(!header.contains("INSECURE"));
        assert!(!header.contains("EXPIRED"));
        assert!(BrowserAuth::from_cookie(header, "0").is_ok());
    }

    #[test]
    fn browser_source_options_are_validated_before_launch() {
        assert!(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(from_browser(
                    "firefox",
                    Some("bad:profile"),
                    None,
                    None,
                    "0"
                ))
                .is_err()
        );
        assert!(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(from_browser(
                    "firefox",
                    None,
                    Some("bad:container"),
                    None,
                    "0"
                ))
                .is_err()
        );
        assert!(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(from_browser("chrome", None, Some("Music"), None, "0"))
                .is_err()
        );
        assert!(
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(from_browser("chrome", None, None, Some("unknown"), "0"))
                .is_err()
        );
    }
}
