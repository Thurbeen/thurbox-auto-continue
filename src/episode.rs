//! One limit episode, stored as JSON in the session-meta key
//! `auto-continue.episode`, where the plugin, `status` and every later run can
//! read it and where it survives a Thurbox restart.
//!
//! ```text
//! armed ──fire──▶ claimed ──▶ sent | unconfirmed | skipped:<why>
//!   │                │
//!   │                └─ found claimed by a later run (a crash) ─▶ abandoned
//!   └─ gates fail before the claim ─▶ skipped:<why>
//! re-rejected after a send ─▶ armed again, until max_attempts ─▶ gave-up
//! ```

use serde::{Deserialize, Serialize};

pub const EPISODE_KEY: &str = "auto-continue.episode";

/// The prefix of every automation this crate schedules, one per session.
pub const AUTOMATION_PREFIX: &str = "auto-continue/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Armed,
    Claimed,
    Sent,
    Unconfirmed,
    Skipped,
    GaveUp,
    Abandoned,
}

impl State {
    pub fn is_final(self) -> bool {
        !matches!(self, State::Armed | State::Claimed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Episode {
    /// The rejected transcript row's uuid.
    pub id: String,
    pub window: String,
    /// Window reset, epoch seconds.
    pub resets_at: i64,
    /// The rejection's own timestamp, epoch ms.
    pub rejected_at_ms: i64,
    /// When the send is due, epoch ms.
    pub fire_at_ms: i64,
    pub transcript: String,
    pub state: State,
    /// Why a `skipped` or `gave-up` happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 1 for a first limit; +1 for each re-rejection after one of our sends.
    pub attempt: u32,
    /// The session's `hook_state_at` once this episode was recorded. Anything
    /// newer means the session moved on without us.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_state_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<i64>,
    /// When we pressed Enter, epoch ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

impl Episode {
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("episode serializes")
    }

    /// `skipped:<why>` / `gave-up:<why>` as one word for logs and status.
    pub fn label(&self) -> String {
        let state =
            serde_json::to_value(self.state).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        match &self.reason {
            Some(why) => format!("{state}:{why}"),
            None => state,
        }
    }
}

/// How long after one of our sends a new rejection still counts as the same
/// run — our message was rejected again rather than a fresh limit later on.
pub const RETRY_WINDOW_MS: i64 = 30 * 60 * 1000;

/// The attempt number a new rejection gets, given the previous episode.
pub fn next_attempt(previous: Option<&Episode>, rejected_at_ms: i64) -> u32 {
    // Any episode that pressed Enter counts, whatever it managed to record
    // afterwards: a run cut off after Enter still sent.
    match previous {
        Some(Episode { sent_at_ms: Some(sent), attempt, .. })
            if rejected_at_ms >= *sent && rejected_at_ms - sent <= RETRY_WINDOW_MS =>
        {
            attempt + 1
        }
        _ => 1,
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(state: State, attempt: u32, updated: i64) -> Episode {
        Episode {
            id: "u".into(),
            window: "five_hour".into(),
            resets_at: 0,
            rejected_at_ms: 0,
            fire_at_ms: 0,
            transcript: String::new(),
            state,
            reason: None,
            attempt,
            hook_state_at: None,
            automation_id: None,
            sent_at_ms: Some(updated),
            updated_at_ms: updated,
        }
    }

    #[test]
    fn a_rejection_soon_after_our_send_is_the_next_attempt() {
        assert_eq!(next_attempt(None, 10), 1);
        assert_eq!(next_attempt(Some(&ep(State::Sent, 1, 1_000)), 2_000), 2);
        assert_eq!(next_attempt(Some(&ep(State::Unconfirmed, 2, 1_000)), 2_000), 3);
        assert_eq!(next_attempt(Some(&ep(State::Sent, 1, 1_000)), 1_000 + RETRY_WINDOW_MS + 1), 1);
        assert_eq!(next_attempt(Some(&ep(State::Skipped, 1, 1_000)), 2_000), 2);
        let mut unsent = ep(State::Skipped, 1, 1_000);
        unsent.sent_at_ms = None;
        assert_eq!(next_attempt(Some(&unsent), 2_000), 1);
    }

    /// Enter was pressed and the run died before recording it: the send still
    /// counts, or a message rejected every window would never give up.
    #[test]
    fn a_send_the_run_did_not_live_to_record_still_counts() {
        assert_eq!(next_attempt(Some(&ep(State::Abandoned, 2, 1_000)), 2_000), 3);
        assert_eq!(next_attempt(Some(&ep(State::Claimed, 1, 1_000)), 2_000), 2);
    }

    #[test]
    fn round_trips_and_labels() {
        let mut e = ep(State::Skipped, 1, 5);
        e.reason = Some("other-text".into());
        assert_eq!(Episode::parse(&e.to_json()), Some(e.clone()));
        assert_eq!(e.label(), "skipped:other-text");
        assert!(State::GaveUp.is_final() && !State::Claimed.is_final());
    }
}
