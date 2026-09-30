//! Reading a Claude Code transcript (`<config>/projects/<dir>/<id>.jsonl`).
//!
//! Claude records a usage-limit rejection as an assistant row with
//! `isApiErrorMessage`, `error: "rate_limit"` and a `quotaLimits` object whose
//! `status` is `"rejected"` and whose `resetsAt` is the window's reset in epoch
//! seconds. A transient 429 has the same `error` but no `quotaLimits`, and is
//! never an episode. Both shapes are recorded from Claude Code 2.1.285 in
//! `tests/fixtures/transcripts/`.
//!
//! Rows are not written in time order — the prompt that hit the limit lands
//! *after* the rejection in the file — so every "what came later" question is
//! asked of the rows' own timestamps, never of their position.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How much of the file's tail is read. A limit episode is the latest turn, so
/// it sits in the last few rows; the cap keeps a huge transcript cheap.
pub const TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// One proven quota rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejected row's `uuid`, which names the episode.
    pub uuid: String,
    /// The row's own timestamp, epoch milliseconds.
    pub at_ms: i64,
    /// When the window resets, epoch seconds (Claude's unit).
    pub resets_at: i64,
    /// `rateLimitType`: `five_hour`, `seven_day`, ... or `unknown`.
    pub window: String,
}

/// The parsed rows of a transcript's tail. Lines that do not parse are dropped.
pub struct Transcript {
    rows: Vec<Value>,
}

impl Transcript {
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let start = len.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut buf)?;
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if start > 0 {
            // The first line was cut; drop it rather than misread it.
            text = text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
        }
        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        let rows = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Self { rows }
    }

    /// The newest proven quota rejection, by timestamp.
    pub fn latest_rejection(&self) -> Option<Rejection> {
        self.rows
            .iter()
            .filter_map(rejection)
            .max_by_key(|r| r.at_ms)
    }

    /// Whether anything conversational happened after `at_ms`: a user row (the
    /// user typing, a restart prompt, or Claude's own auto-resume, which writes
    /// an `isMeta` user row) or an assistant row, other than the rejection
    /// itself. Bookkeeping rows (`system`, `last-prompt`, ...) do not count.
    pub fn activity_after(&self, at_ms: i64, except_uuid: &str) -> bool {
        self.rows.iter().any(|row| {
            matches!(row["type"].as_str(), Some("user" | "assistant"))
                && row["uuid"].as_str() != Some(except_uuid)
                && row_time(row).is_some_and(|t| t > at_ms)
        })
    }
}

fn rejection(row: &Value) -> Option<Rejection> {
    if row["type"] != "assistant" || row["isApiErrorMessage"] != true || row["error"] != "rate_limit" {
        return None;
    }
    let q = row.get("quotaLimits")?;
    if q["status"] != "rejected" || q["isUsingOverage"] == true || q["overageInUse"] == true {
        return None;
    }
    let resets_at = q["resetsAt"].as_f64().filter(|v| v.is_finite() && *v > 0.0)? as i64;
    Some(Rejection {
        uuid: row["uuid"].as_str()?.to_string(),
        at_ms: row_time(row)?,
        resets_at,
        window: q["rateLimitType"].as_str().unwrap_or("unknown").to_string(),
    })
}

fn row_time(row: &Value) -> Option<i64> {
    parse_rfc3339_ms(row["timestamp"].as_str()?)
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` to epoch milliseconds. Claude writes UTC with a
/// `Z`; anything else is not trusted and reads as no time at all.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let digits: String = frac.chars().take(3).collect();
    let ms = format!("{digits:0<3}").parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

/// Where Claude keeps a conversation's transcript: `<config>/projects/<dir>/<id>.jsonl`
/// for whichever project directory holds it.
pub fn locate(claude_config: &Path, agent_session_id: &str) -> Option<PathBuf> {
    let name = format!("{agent_session_id}.jsonl");
    std::fs::read_dir(claude_config.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Transcript {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts").join(name);
        Transcript::read(&path).unwrap()
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-09-30T09:53:30.896Z"), Some(1_790_762_010_896));
        assert_eq!(parse_rfc3339_ms("2026-09-30T09:53:30Z"), Some(1_790_762_010_000));
        assert_eq!(parse_rfc3339_ms("2026-09-30T09:53:30+02:00"), None);
    }

    /// The rejection is followed by bookkeeping rows, so it is not the last row.
    #[test]
    fn a_quota_rejection_is_found_behind_bookkeeping_rows() {
        let t = fixture("quota-rejected-then-bookkeeping.jsonl");
        let r = t.latest_rejection().expect("rejection");
        assert_eq!(r.window, "five_hour");
        assert_eq!(r.resets_at, 1_790_765_610);
        assert_eq!(r.uuid, "a1abdfe2-b980-41ad-a4f7-62943faf62e6");
        assert!(!t.activity_after(r.at_ms, &r.uuid));
    }

    /// A2 at the parser: a transient 429 carries no `quotaLimits`.
    #[test]
    fn a_transient_429_is_not_an_episode() {
        assert_eq!(fixture("transient-429.jsonl").latest_rejection(), None);
    }

    /// The prompt row is written after the rejection but is older than it.
    #[test]
    fn the_prompt_written_after_the_rejection_is_not_activity() {
        let t = fixture("quota-interactive-cancelled.jsonl");
        let r = t.latest_rejection().expect("rejection");
        assert!(!t.activity_after(r.at_ms, &r.uuid));
    }

    /// Claude's own auto-resume writes an `isMeta` user row after the reset.
    #[test]
    fn native_resume_is_activity_after_the_first_rejection() {
        let t = fixture("native-resume.jsonl");
        let latest = t.latest_rejection().expect("rejection");
        assert_eq!(latest.resets_at, 1_790_762_427);
        let first_at = parse_rfc3339_ms("2026-09-30T09:55:12.309Z").unwrap();
        assert!(t.activity_after(first_at, "637c4532-f6d0-45dc-b84b-0f031539495e"));
        assert!(!t.activity_after(latest.at_ms, &latest.uuid));
    }

    #[test]
    fn an_overage_or_unbounded_rejection_is_not_an_episode() {
        let row = |q: &str| {
            format!(
                r#"{{"type":"assistant","uuid":"u","timestamp":"2026-09-30T09:00:00Z","isApiErrorMessage":true,"error":"rate_limit","quotaLimits":{q}}}"#
            )
        };
        let none = |q: &str| Transcript::parse(&row(q)).latest_rejection().is_none();
        assert!(none(r#"{"status":"rejected","resetsAt":1790765610,"isUsingOverage":true}"#));
        assert!(none(r#"{"status":"rejected"}"#));
        assert!(none(r#"{"status":"allowed_warning","resetsAt":1790765610}"#));
        assert!(!none(r#"{"status":"rejected","resetsAt":1790765610}"#));
    }
}
