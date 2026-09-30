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

/// The `thurbox-cli` program to run: the one on PATH, else the one in
/// `~/.local/bin`, else the one Thurbox links under its data directory. A run
/// delegated to a shared host over ssh gets a non-interactive PATH, which
/// rarely holds either, and Thurbox provisions a host's CLI there, never on
/// PATH.
pub fn thurbox_cli() -> PathBuf {
    if let Some(cli) = std::env::var_os(THURBOX_CLI_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(cli);
    }
    let name = format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|d| d.join(&name).is_file()) {
        return PathBuf::from(name);
    }
    [user_home().map(|h| h.join(".local").join("bin")), thurbox_data_dir().map(|d| d.join("bin"))]
        .into_iter()
        .flatten()
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Thurbox's data directory, resolved the way Thurbox resolves it.
fn thurbox_data_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("THURBOX_DATA_DIR") {
        return Some(dir);
    }
    let base = var("XDG_DATA_HOME").or_else(|| {
        if cfg!(windows) { var("LOCALAPPDATA") } else { user_home().map(|h| h.join(".local").join("share")) }
    });
    base.map(|b| b.join("thurbox"))
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
        // Thurbox 2.39.7 spells the local route with its multiplexer.
        assert!(is_local_backend("local:tmux"));
        assert!(is_local_backend("local:psmux"));
        assert!(!is_local_backend("ssh:build-box"));
        assert!(!is_local_backend("wsl:Ubuntu"));
    }
}
