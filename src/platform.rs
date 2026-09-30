//! Every platform-specific decision lives here, so a port has one file to read.
//!
//! Linux and macOS run the end-to-end suite against tmux; native Windows runs
//! it against psmux, Thurbox's PowerShell keeper and `cmd /C` (CI's
//! `test-windows` job). What Windows does differently is written down here.

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

/// One argument of an Exec automation's command, spelled for the shell
/// Thurbox runs it under: `sh -c` on POSIX, `cmd /C` on Windows. `None` when
/// that shell cannot be handed it.
///
/// On Windows no quoting survives: Thurbox passes the command to `cmd /C` as
/// one argument, the Rust runtime escapes every `"` in it as `\"`, and `cmd`
/// does not read that escape (measured: `"C:\x.exe" --version` fails as
/// `'\"C:\x.exe\"' is not recognized`). So an argument goes in bare, or as its
/// 8.3 short path when that has nothing `cmd` would split on, or not at all.
pub fn shell_arg(arg: &str) -> Option<String> {
    if cfg!(windows) {
        if cmd_safe(arg) {
            return Some(arg.to_string());
        }
        short_path(arg).filter(|s| cmd_safe(s))
    } else {
        Some(format!("'{}'", arg.replace('\'', "'\\''")))
    }
}

/// Whether `cmd` reads `arg` back as the one argument it is.
fn cmd_safe(arg: &str) -> bool {
    !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || "\"&|<>^%!(),;=".contains(c))
}

/// The 8.3 short form of an existing path, when the volume keeps them.
#[cfg(windows)]
fn short_path(path: &str) -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
    }
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `buf` is as long as we say it is.
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

#[cfg(not(windows))]
fn short_path(_: &str) -> Option<String> {
    None
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
        assert_eq!(shell_arg("/a b/it's").as_deref(), Some(r"'/a b/it'\''s'"));
    }

    #[cfg(windows)]
    #[test]
    fn cmd_gets_bare_arguments_or_none() {
        assert_eq!(shell_arg(r"C:\Users\me\x.exe").as_deref(), Some(r"C:\Users\me\x.exe"));
        assert_eq!(shell_arg("0d9c-4e2f").as_deref(), Some("0d9c-4e2f"));
        // Not a path, so there is no short form to fall back on.
        assert_eq!(shell_arg("a b&c"), None);
    }

    #[test]
    fn cmd_splits_on_whitespace_and_its_operators() {
        assert!(cmd_safe(r"C:\Users\me\.config/thurbox"));
        for bad in ["", "a b", "a\"b", "a&b", "a|b", "a^b", "%PATH%", "a(b)", "a,b", "a;b", "a=b"] {
            assert!(!cmd_safe(bad), "{bad}");
        }
    }

    #[test]
    fn only_local_backends_are_ours() {
        assert!(is_local_backend("local-tmux"));
        assert!(!is_local_backend("ssh:build-box"));
        assert!(!is_local_backend("wsl:Ubuntu"));
    }
}
