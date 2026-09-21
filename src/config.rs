//! User-facing appearance and keyboard configuration.
#[cfg(not(test))]
use std::fs;
use std::{env, path::PathBuf};

use anyhow::{Context, Result, ensure};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub start_view: String,
    pub colors: Colors,
    pub keybindings: KeyBindings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "tokyo-night".into(),
            start_view: "home".into(),
            colors: Colors::default(),
            keybindings: KeyBindings::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Colors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBindings {
    pub quit: String,
    pub settings: String,
    pub search: String,
    pub help: String,
    pub home: String,
    pub explore: String,
    #[serde(alias = "library")]
    pub playlists: String,
    pub albums: String,
    pub artists: String,
    pub podcasts: String,
    pub queue: String,
    pub now_playing: String,
    pub pause: String,
    pub next_track: String,
    pub retry_track: String,
    pub volume_up: String,
    pub volume_down: String,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            quit: "q".into(),
            settings: "s".into(),
            search: "/".into(),
            help: "?".into(),
            home: "h".into(),
            explore: "e".into(),
            playlists: "1".into(),
            albums: "2".into(),
            artists: "3".into(),
            podcasts: "4".into(),
            queue: "Tab".into(),
            now_playing: "t".into(),
            pause: "Space".into(),
            next_track: "n".into(),
            retry_track: "r".into(),
            volume_up: "+".into(),
            volume_down: "-".into(),
        }
    }
}

impl KeyBindings {
    pub fn get(&self, action: &str) -> Option<&str> {
        Some(match action {
            "quit" => &self.quit,
            "settings" => &self.settings,
            "search" => &self.search,
            "help" => &self.help,
            "home" => &self.home,
            "explore" => &self.explore,
            "library" | "playlists" => &self.playlists,
            "albums" => &self.albums,
            "artists" => &self.artists,
            "podcasts" => &self.podcasts,
            "queue" => &self.queue,
            "now_playing" => &self.now_playing,
            "pause" => &self.pause,
            "next_track" => &self.next_track,
            "retry_track" => &self.retry_track,
            "volume_up" => &self.volume_up,
            "volume_down" => &self.volume_down,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub secondary: Color,
    pub muted: Color,
    pub accent: Color,
    pub good: Color,
    pub warning: Color,
    pub error: Color,
}

pub const THEMES: &[&str] = &["tokyo-night", "catppuccin-mocha", "gruvbox-dark", "nord"];
pub const START_VIEWS: &[&str] = &[
    "home",
    "explore",
    "playlists",
    "albums",
    "artists",
    "podcasts",
    "search",
    "queue",
];

impl Config {
    pub fn palette(&self) -> Palette {
        let base = match self.theme.as_str() {
            "catppuccin-mocha" => Palette::rgb(
                "cdd6f4", "bac2de", "6c7086", "cba6f7", "a6e3a1", "f9e2af", "f38ba8",
            ),
            "gruvbox-dark" => Palette::rgb(
                "ebdbb2", "d5c4a1", "928374", "83a598", "b8bb26", "fabd2f", "fb4934",
            ),
            "nord" => Palette::rgb(
                "eceff4", "d8dee9", "616e88", "88c0d0", "a3be8c", "ebcb8b", "bf616a",
            ),
            _ => Palette::rgb(
                "c0caf5", "a9b1d6", "737aa2", "7aa2f7", "9ece6a", "e0af68", "f7768e",
            ),
        };
        Palette {
            text: parse_color(self.colors.text.as_deref()).unwrap_or(base.text),
            secondary: parse_color(self.colors.secondary.as_deref()).unwrap_or(base.secondary),
            muted: parse_color(self.colors.muted.as_deref()).unwrap_or(base.muted),
            accent: parse_color(self.colors.accent.as_deref()).unwrap_or(base.accent),
            good: parse_color(self.colors.good.as_deref()).unwrap_or(base.good),
            warning: parse_color(self.colors.warning.as_deref()).unwrap_or(base.warning),
            error: parse_color(self.colors.error.as_deref()).unwrap_or(base.error),
        }
    }

    #[cfg(not(test))]
    pub fn normalize(&mut self) {
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = "tokyo-night".into();
        }
        if self.start_view == "library" {
            self.start_view = "playlists".into();
        } else if !START_VIEWS.contains(&self.start_view.as_str()) {
            self.start_view = "home".into();
        }
    }

    pub fn load() -> Result<Self> {
        #[cfg(test)]
        {
            Ok(Self::default())
        }
        #[cfg(not(test))]
        {
            let path = config_path()?;
            let mut config = match fs::read_to_string(&path) {
                Ok(text) => toml::from_str(&text)
                    .with_context(|| format!("Cannot parse {}", path.display()))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let config = Self::default();
                    config.save()?;
                    config
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("Cannot read {}", path.display()));
                }
            };
            config.normalize();
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        #[cfg(test)]
        {
            Ok(())
        }
        #[cfg(not(test))]
        {
            let path = config_path()?;
            let parent = path.parent().context("Configuration path has no parent")?;
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
            let body = format!(
                "# Dymus user configuration.\n# Available themes: tokyo-night, catppuccin-mocha, gruvbox-dark, nord.\n# Start view: home, explore, playlists, albums, artists, podcasts, search, or queue.\n# Colors are optional six-digit hex overrides (examples below use Tokyo Night).\n# Uncomment and edit any value:\n# [colors]\n# text = \"#c0caf5\"\n# secondary = \"#a9b1d6\"\n# muted = \"#737aa2\"\n# accent = \"#7aa2f7\"\n# good = \"#9ece6a\"\n# warning = \"#e0af68\"\n# error = \"#f7768e\"\n\n{}",
                toml::to_string_pretty(self)?
            );
            fs::write(&path, body).with_context(|| format!("Cannot write {}", path.display()))
        }
    }
}

impl Palette {
    fn rgb(
        text: &str,
        secondary: &str,
        muted: &str,
        accent: &str,
        good: &str,
        warning: &str,
        error: &str,
    ) -> Self {
        Self {
            text: parse_color(Some(text)).unwrap(),
            secondary: parse_color(Some(secondary)).unwrap(),
            muted: parse_color(Some(muted)).unwrap(),
            accent: parse_color(Some(accent)).unwrap(),
            good: parse_color(Some(good)).unwrap(),
            warning: parse_color(Some(warning)).unwrap(),
            error: parse_color(Some(error)).unwrap(),
        }
    }
}

fn parse_color(value: Option<&str>) -> Option<Color> {
    let value = value?.trim().strip_prefix('#').unwrap_or(value?.trim());
    if value.len() != 6 {
        return None;
    }
    let rgb = u32::from_str_radix(value, 16).ok()?;
    Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

pub fn config_path() -> Result<PathBuf> {
    let base = if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg)
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .context("Cannot locate your configuration directory")?
            .join(".config")
    };
    ensure!(
        !base.as_os_str().is_empty(),
        "Configuration directory is empty"
    );
    Ok(base.join("dymus").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_partial_toml_overrides_are_valid() {
        let config: Config = toml::from_str(
            "theme = 'nord'\n[colors]\naccent = '#ff00aa'\n[keybindings]\nquit = 'x'\n",
        )
        .unwrap();
        let palette = config.palette();
        assert_eq!(palette.accent, Color::Rgb(255, 0, 170));
        assert_eq!(config.keybindings.quit, "x");
        assert_eq!(config.keybindings.next_track, "n");
    }

    #[test]
    fn generated_defaults_round_trip_with_builtin_theme_and_keys() {
        let encoded = toml::to_string_pretty(&Config::default()).unwrap();
        let decoded: Config = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.theme, "tokyo-night");
        assert_eq!(decoded.start_view, "home");
        assert_eq!(decoded.keybindings.settings, "s");
        assert!(decoded.colors.accent.is_none());
    }
}
