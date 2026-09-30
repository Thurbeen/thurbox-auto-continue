//! `<home>/config.toml`, and how it combines with a session's own toggle.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The session-meta key that turns auto-continue on or off for one session.
pub const ENABLED_KEY: &str = "auto-continue.enabled";

/// What to do when Claude's usage-limit menu is open at send time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnMenu {
    /// Press Esc once, then look at the screen again.
    Escape,
    /// Leave it alone and record a skip.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The global default. A session's own toggle wins in both directions.
    pub enabled: bool,
    /// How long after the window resets to send, in seconds.
    pub delay_secs: u64,
    /// The text typed into the session.
    pub message: String,
    /// The quota windows acted on (`five_hour`, `seven_day`, ...).
    pub windows: Vec<String>,
    /// How many sends one run of re-rejections may take before giving up.
    pub max_attempts: u32,
    pub on_menu: OnMenu,
    /// How long to wait for the session to report `working` after Enter.
    pub confirm_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            delay_secs: 300,
            message: "continue".into(),
            windows: vec!["five_hour".into()],
            max_attempts: 2,
            on_menu: OnMenu::Escape,
            confirm_secs: 20,
        }
    }
}

const HEADER: &str = "# thurbox-auto-continue settings. Auto-continue is off unless `enabled` is true\n\
# here or a session is turned on with `thurbox-auto-continue enable <session>`.\n";

impl Config {
    /// Read the config. A missing file is the defaults; a broken one is an
    /// error, so a typo never silently turns the feature on or off.
    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        let path = home.join("config.toml");
        let tmp = home.join("config.toml.tmp");
        std::fs::write(&tmp, format!("{HEADER}{body}"))
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Set one key from its command-line spelling.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let bad = |what: &str| format!("`{key}` takes {what}, not `{value}`");
        match key {
            "enabled" => self.enabled = parse_switch(value).ok_or_else(|| bad("on or off"))?,
            "delay_secs" => self.delay_secs = value.parse().map_err(|_| bad("a number of seconds"))?,
            "message" if !value.trim().is_empty() => self.message = value.to_string(),
            "message" => return Err(bad("some text")),
            "windows" => {
                self.windows = value
                    .split(',')
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(String::from)
                    .collect()
            }
            "max_attempts" => self.max_attempts = value.parse().map_err(|_| bad("a number"))?,
            "on_menu" => {
                self.on_menu = match value {
                    "escape" => OnMenu::Escape,
                    "skip" => OnMenu::Skip,
                    _ => return Err(bad("escape or skip")),
                }
            }
            "confirm_secs" => self.confirm_secs = value.parse().map_err(|_| bad("a number of seconds"))?,
            _ => {
                return Err(format!(
                    "unknown key `{key}`; one of: enabled, delay_secs, message, windows, max_attempts, on_menu, confirm_secs"
                ));
            }
        }
        Ok(())
    }
}

/// `on`/`off` and their usual spellings.
pub fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Some(true),
        "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// A session's own toggle beats the global default in both directions; an
/// unset or unreadable toggle falls back to it.
pub fn effective(global: bool, session_override: Option<&str>) -> bool {
    session_override.and_then(parse_switch).unwrap_or(global)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_five_hour_only() {
        let c = Config::default();
        assert!(!c.enabled);
        assert_eq!(c.windows, ["five_hour"]);
    }

    /// A11: the four global/per-session combinations, plus an unset toggle.
    #[test]
    fn session_toggle_beats_global_both_ways() {
        assert!(effective(false, Some("on")));
        assert!(!effective(true, Some("off")));
        assert!(effective(true, Some("on")));
        assert!(!effective(false, Some("off")));
        assert!(effective(true, None));
        assert!(!effective(false, None));
        assert!(!effective(false, Some("garbage")));
    }

    #[test]
    fn missing_file_is_defaults_and_broken_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), Config::default());
        std::fs::write(dir.path().join("config.toml"), "enabled = maybe\n").unwrap();
        assert!(Config::load(dir.path()).is_err());
    }

    #[test]
    fn set_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.set("enabled", "on").unwrap();
        c.set("windows", "five_hour, seven_day").unwrap();
        c.save(dir.path()).unwrap();
        let back = Config::load(dir.path()).unwrap();
        assert!(back.enabled);
        assert_eq!(back.windows, ["five_hour", "seven_day"]);
        assert!(c.set("nope", "1").is_err());
        assert!(c.set("on_menu", "sideways").is_err());
    }

    #[test]
    fn shipped_seed_parses_to_the_defaults() {
        let seed = include_str!("../extension/config.toml");
        assert_eq!(toml::from_str::<Config>(seed).unwrap(), Config::default());
    }
}
