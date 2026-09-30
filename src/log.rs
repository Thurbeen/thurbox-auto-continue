//! `<home>/auto-continue.log`: one line per decision. It records session ids,
//! states and reasons only — never the message, the screen or the transcript.

use std::io::Write;
use std::path::Path;

const MAX_BYTES: u64 = 256 * 1024;

pub fn event(home: &Path, verb: &str, session: &str, what: &str) {
    let path = home.join("auto-continue.log");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, home.join("auto-continue.log.1"));
    }
    let _ = std::fs::create_dir_all(home);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {verb} session={session} {what}", crate::episode::now_ms());
    }
}
