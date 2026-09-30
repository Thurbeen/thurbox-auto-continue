//! A per-session OS file lock, so two runs on one machine never act on one
//! session at once. The kernel drops it when the holder dies, so a crash never
//! leaves it held.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct SessionLock {
    _file: File,
}

impl SessionLock {
    /// Take the lock for `session` in `dir`, waiting up to `wait`. `None` means another
    /// run holds it; the caller leaves the session to that run.
    pub fn acquire(dir: &Path, session: &str, wait: Duration) -> Result<Option<Self>, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let safe: String =
            session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let path = dir.join(format!("{safe}.lock"));
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_refused_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let held = SessionLock::acquire(dir.path(), "s1", Duration::ZERO).unwrap();
        assert!(held.is_some());
        assert!(SessionLock::acquire(dir.path(), "s1", Duration::from_millis(50)).unwrap().is_none());
        assert!(SessionLock::acquire(dir.path(), "s2", Duration::ZERO).unwrap().is_some());
        drop(held);
        assert!(SessionLock::acquire(dir.path(), "s1", Duration::ZERO).unwrap().is_some());
    }
}
