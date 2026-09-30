//! `<home>/config.toml`, and how it combines with a session's own settings.
//!
//! Three settings resolve per session, first match wins: the session's own
//! override (session meta), the global `config.toml`, the built-in default.
//! docs/CLI-CONTRACT.md is the contract this implements.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The session-meta key that turns auto-continue on or off for one session.
pub const ENABLED_KEY: &str = "auto-continue.enabled";
/// A session's own message.
pub const MESSAGE_KEY: &str = "auto-continue.message";
/// A session's own delay after the reset, in seconds.
pub const DELAY_KEY: &str = "auto-continue.delay_secs";

/// The settings a session may override, and the meta key each lives under.
pub const SESSION_SETTINGS: [(&str, &str); 3] =
    [("enabled", ENABLED_KEY), ("message", MESSAGE_KEY), ("delay_secs", DELAY_KEY)];

pub const MAX_MESSAGE_CHARS: usize = 500;
pub const MAX_DELAY_SECS: u64 = 86_400;

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
    /// The keys `config.toml` itself sets; the rest are defaults.
    #[serde(skip)]
    pub present: BTreeSet<String>,
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
            present: BTreeSet::new(),
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

const HEADER: &str = "# thurbox-auto-continue settings; a key left out takes its default.\n\
# Change them with `thurbox-auto-continue config set <key> <value> [--session <ref>]`.\n";

impl Config {
    /// Read the config. A missing file is the defaults; a broken one is an
    /// error, so a typo never silently turns the feature on or off.
    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut cfg: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                let table: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                cfg.present = table.keys().cloned().collect();
                // The same rules as `config set`: a hand edit cannot put a
                // slash command in the composer or a send days away.
                validate_message(&cfg.message).map_err(|e| format!("{}: {e}", path.display()))?;
                validate_delay(&cfg.delay_secs.to_string()).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
        // Only what was set, so every other key keeps reading as the default.
        let mut table = toml::Table::try_from(self).map_err(|e| e.to_string())?;
        table.retain(|k, _| self.present.contains(k));
        let body = toml::to_string(&table).map_err(|e| e.to_string())?;
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
            "enabled" => self.enabled = validate_switch(value)?,
            "delay_secs" => self.delay_secs = validate_delay(value)?,
            "message" => self.message = validate_message(value)?,
            "windows" => {
                self.windows = value.split(',').map(str::trim).filter(|w| !w.is_empty()).map(String::from).collect()
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
        self.present.insert(key.to_string());
        Ok(())
    }

    /// The global value of each per-session setting, and where it came from.
    pub fn global(&self) -> Effective {
        let source = |key: &str| if self.present.contains(key) { Source::Global } else { Source::Default };
        Effective {
            enabled: Resolved { value: self.enabled, source: source("enabled") },
            message: Resolved { value: self.message.clone(), source: source("message") },
            delay_secs: Resolved { value: self.delay_secs, source: source("delay_secs") },
            warnings: Vec::new(),
        }
    }

    /// One session's settings: its own overrides (`meta`, as `session meta
    /// list` prints it) over the global ones. An override that does not
    /// validate is ignored and reported in `warnings`.
    pub fn for_session(&self, meta: &Map<String, Value>) -> Effective {
        let mut e = self.global();
        let get = |k: &str| meta.get(k).and_then(Value::as_str);
        let mut warn = |key: &str, err: String| e.warnings.push(format!("session override `{key}` ignored: {err}"));
        if let Some(v) = get(ENABLED_KEY) {
            match validate_switch(v) {
                Ok(value) => e.enabled = Resolved { value, source: Source::Session },
                Err(err) => warn("enabled", err),
            }
        }
        if let Some(v) = get(MESSAGE_KEY) {
            match validate_message(v) {
                Ok(value) => e.message = Resolved { value, source: Source::Session },
                Err(err) => warn("message", err),
            }
        }
        if let Some(v) = get(DELAY_KEY) {
            match validate_delay(v) {
                Ok(value) => e.delay_secs = Resolved { value, source: Source::Session },
                Err(err) => warn("delay_secs", err),
            }
        }
        e
    }
}

/// Where a setting's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Session,
    Global,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

/// The settings that apply to one session (or globally).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Effective {
    pub enabled: Resolved<bool>,
    pub message: Resolved<String>,
    pub delay_secs: Resolved<u64>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

pub fn validate_switch(value: &str) -> Result<bool, String> {
    parse_switch(value).ok_or_else(|| format!("`enabled` takes on or off, not `{value}`"))
}

/// Whole seconds, at most a day.
pub fn validate_delay(value: &str) -> Result<u64, String> {
    match value.trim().parse::<u64>() {
        Ok(n) if n <= MAX_DELAY_SECS => Ok(n),
        _ => Err(format!("`delay_secs` takes whole seconds from 0 to {MAX_DELAY_SECS}, not `{value}`")),
    }
}

/// What may be typed into a Claude session: one line of plain text that is
/// not a slash command or a shell escape.
pub fn validate_message(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("`message` must not be empty".into());
    }
    if value.chars().any(char::is_control) {
        return Err("`message` must be one line with no control characters".into());
    }
    if value.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("`message` must be at most {MAX_MESSAGE_CHARS} characters"));
    }
    if value.trim_start().starts_with(['/', '!']) {
        return Err("`message` must not start with / or !: it would run a Claude command or a shell".into());
    }
    Ok(value.to_string())
}

/// `on`/`off` and their usual spellings.
pub fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Some(true),
        "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
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
        let on = |global: bool, toggle: Option<&str>| {
            let cfg = Config { enabled: global, ..Config::default() };
            cfg.for_session(&toggle.map(|t| meta(&[(ENABLED_KEY, t)])).unwrap_or_default()).enabled.value
        };
        assert!(on(false, Some("on")));
        assert!(!on(true, Some("off")));
        assert!(on(true, Some("on")));
        assert!(!on(false, Some("off")));
        assert!(on(true, None));
        assert!(!on(false, None));
        assert!(!on(false, Some("garbage")));
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
        assert_eq!(back.present, BTreeSet::from(["enabled".to_string(), "windows".to_string()]));
        assert_eq!(back.global().message.source, Source::Default);
        assert!(back.enabled);
        assert_eq!(back.windows, ["five_hour", "seven_day"]);
        assert!(c.set("nope", "1").is_err());
        assert!(c.set("on_menu", "sideways").is_err());
    }

    fn meta(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect()
    }

    #[test]
    fn a_session_override_beats_global_which_beats_the_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "delay_secs = 60\n").unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        let g = cfg.global();
        assert_eq!(g.delay_secs, Resolved { value: 60, source: Source::Global });
        assert_eq!(g.message, Resolved { value: "continue".into(), source: Source::Default });

        let e = cfg.for_session(&meta(&[(MESSAGE_KEY, "keep going"), (ENABLED_KEY, "on")]));
        assert_eq!(e.message, Resolved { value: "keep going".into(), source: Source::Session });
        assert_eq!(e.enabled, Resolved { value: true, source: Source::Session });
        assert_eq!(e.delay_secs.source, Source::Global);
        assert!(e.warnings.is_empty());
    }

    /// A hand-edited config is held to the same rules as `config set`; an
    /// error means the config is unreadable, and nothing is sent.
    #[test]
    fn a_hand_edited_unsafe_message_or_delay_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["message = \"/clear\"\n", "message = \"\"\n", "delay_secs = 999999\n"] {
            std::fs::write(dir.path().join("config.toml"), bad).unwrap();
            assert!(Config::load(dir.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_bad_override_falls_back_and_warns() {
        let e = Config::default().for_session(&meta(&[(DELAY_KEY, "forever"), (MESSAGE_KEY, "/clear")]));
        assert_eq!(e.delay_secs, Resolved { value: 300, source: Source::Default });
        assert_eq!(e.message.value, "continue");
        assert_eq!(e.warnings.len(), 2);
    }

    #[test]
    fn messages_that_could_not_be_typed_safely_are_refused() {
        for bad in ["", "   ", "two\nlines", "tab\there", "/clear", "  /compact", "!ls"] {
            assert!(validate_message(bad).is_err(), "{bad:?}");
        }
        assert!(validate_message(&"x".repeat(MAX_MESSAGE_CHARS)).is_ok());
        assert!(validate_message(&"x".repeat(MAX_MESSAGE_CHARS + 1)).is_err());
        assert!(validate_message("keep going — résumé the task").is_ok());
        assert_eq!(validate_delay("0"), Ok(0));
        assert_eq!(validate_delay("86400"), Ok(86_400));
        assert!(validate_delay("86401").is_err() && validate_delay("-1").is_err() && validate_delay("1.5").is_err());
    }

    #[test]
    fn shipped_seed_parses_to_the_defaults() {
        let seed = include_str!("../extension/config.toml");
        assert_eq!(toml::from_str::<Config>(seed).unwrap(), Config::default());
    }
}
