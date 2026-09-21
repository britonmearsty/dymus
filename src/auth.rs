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
    let auth: BrowserAuth = serde_json::from_reader(file)
        .context("Dymus auth file is invalid; run `dymus auth paste` again")?;
    BrowserAuth::from_cookie(auth.cookie, &auth.auth_user)
        .context("Dymus auth file contains invalid credentials")
        .map(Some)
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
    serde_json::to_writer(&mut temporary, auth).context("Cannot encode authentication data")?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(auth_path()?)
        .context("Cannot save Dymus credentials")?;
    Ok(())
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
}
