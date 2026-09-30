//! Every platform-specific decision lives here, so a port has one file to read.
//!
//! Linux and macOS (POSIX) are the verified targets. The Windows branches are
//! written but UNVERIFIED: nothing here has run on native Windows, and the
//! README says so.

use std::path::PathBuf;

/// Env var that overrides where the extension keeps `config.toml`, its log and
/// its lock files. The hook command cannot carry a `--home` flag (Thurbox does
/// not substitute `{home}` inside a config merge), so this and the default
/// below are how `record` finds its home.
pub const HOME_ENV: &str = "THURBOX_AUTO_CONTINUE_HOME";

/// Env var naming the `thurbox-cli` to call. Defaults to the one on `PATH`.
pub const THURBOX_CLI_ENV: &str = "THURBOX_AUTO_CONTINUE_THURBOX_CLI";

/// The user's home directory, as Thurbox itself resolves `~`.
pub fn user_home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

/// The default extension home — the `home` in `extension.toml`, expanded the
/// way Thurbox expands a leading `~`.
pub fn default_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    user_home().map(|h| h.join(".config").join("thurbox").join("auto-continue"))
}

/// Where the per-session locks live. Not under the extension home: a run
/// started with another `--home` must still contend for the same lock.
pub fn lock_dir() -> Option<PathBuf> {
    user_home().map(|h| h.join(".config").join("thurbox").join("auto-continue").join("locks"))
}

/// Claude's own config directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
/// Only the sweep needs it; the hook is handed the transcript path directly.
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    user_home().map(|h| h.join(".claude"))
}

/// The `thurbox-cli` program to run.
pub fn thurbox_cli() -> PathBuf {
    std::env::var_os(THURBOX_CLI_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX)))
}

/// Quote one argument for the shell an Exec automation runs under: `sh -c` on
/// POSIX, `cmd /C` on Windows (unverified).
pub fn shell_quote(arg: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", arg.replace('"', "\"\""))
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Whether a Thurbox backend type is this machine's own multiplexer. A session
/// on an SSH or WSL host is acted on by the extension installed on that host.
pub fn is_local_backend(backend_type: &str) -> bool {
    backend_type.starts_with("local")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn posix_quote_survives_single_quotes() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
    }

    #[test]
    fn only_local_backends_are_ours() {
        assert!(is_local_backend("local-tmux"));
        assert!(!is_local_backend("ssh:build-box"));
        assert!(!is_local_backend("wsl:Ubuntu"));
    }
}
