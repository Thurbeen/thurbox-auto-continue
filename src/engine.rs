//! The three headless entry points: `record` (the hook), `fire` (the scheduled
//! send) and `sweep` (the fallback). Each takes the session's lock before it
//! reads or writes an episode, and `fire` writes `claimed` before it touches
//! the pane — so a crash can lose a send but never double one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::{Config, Effective, OnMenu};
use crate::episode::{AUTOMATION_PREFIX, EPISODE_KEY, Episode, State, next_attempt, now_ms};
use crate::lock::SessionLock;
use crate::screen::{Screen, classify};
use crate::thurbox::{Session, Thurbox};
use crate::transcript::{Rejection, Transcript};
use crate::{log, platform};

/// No run lives longer than this, whatever hangs.
pub const DEADLINE: Duration = Duration::from_secs(45);

/// The confirmation wait is capped so a run always ends inside [`DEADLINE`].
const MAX_CONFIRM_SECS: u64 = 20;
/// How old a rejection the hook may still pick up: the hook fires as the turn
/// fails, so anything older is some earlier episode.
const HOOK_FRESH_MS: i64 = 10 * 60 * 1000;
/// How old a rejection the sweep may still pick up.
const SWEEP_FRESH_MS: i64 = 24 * 60 * 60 * 1000;
/// How long past its time a send has to be before the sweep fires it itself.
const OVERDUE_MS: i64 = 90 * 1000;
/// `at:` triggers in the past never fire, so a send is never scheduled sooner.
const MIN_LEAD_MS: i64 = 5000;

struct Ctx {
    home: PathBuf,
    locks: PathBuf,
    cfg: Config,
    tb: Thurbox,
}

impl Ctx {
    fn load(home: &Path) -> Option<Self> {
        match Config::load(home) {
            Ok(cfg) => Some(Self {
                home: home.to_path_buf(),
                locks: platform::lock_dir().unwrap_or_else(|| home.join("locks")),
                cfg,
                tb: Thurbox::default(),
            }),
            Err(e) => {
                log::event(home, "config", "-", &format!("unreadable: {e}"));
                None
            }
        }
    }

    fn log(&self, verb: &str, session: &str, what: &str) {
        log::event(&self.home, verb, session, what);
    }

    /// The session's own settings over the global ones. Unreadable meta is an
    /// error, never an `on` and never a lasting `off`.
    fn settings(&self, session: &str) -> Result<Effective, String> {
        Ok(self.cfg.for_session(&self.tb.meta_list(session)?))
    }

    fn episode(&self, session: &str) -> Result<Option<Episode>, String> {
        Ok(self.tb.meta_get(session, EPISODE_KEY)?.as_deref().and_then(Episode::parse))
    }

    fn save(&self, session: &str, ep: &mut Episode) -> Result<(), String> {
        ep.updated_at_ms = now_ms();
        self.tb.meta_set(session, EPISODE_KEY, &ep.to_json())
    }

    /// Close an episode: record the outcome and drop its pending send.
    fn finish(&self, session: &str, ep: &mut Episode, state: State, reason: Option<&str>) {
        ep.state = state;
        ep.reason = reason.map(String::from);
        if let Err(e) = self.save(session, ep) {
            self.log("finish", session, &format!("could not record {}: {e}", ep.label()));
        }
        self.drop_automations(session);
        self.log("finish", session, &ep.label());
    }

    fn drop_automations(&self, session: &str) {
        let name = format!("{AUTOMATION_PREFIX}{session}");
        if let Ok(all) = self.tb.automations() {
            for a in all.into_iter().filter(|a| a.name == name) {
                let _ = self.tb.remove_automation(a.id);
            }
        }
    }
}

/// End the process if a run outlives [`DEADLINE`]. A run cut off after its
/// claim leaves `claimed` behind, which the next run records as `abandoned`.
fn watchdog(home: &Path, verb: &'static str) {
    let home = home.to_path_buf();
    std::thread::spawn(move || {
        std::thread::sleep(DEADLINE);
        log::event(&home, verb, "-", "deadline reached; exiting");
        std::process::exit(if verb == "record" { 0 } else { 3 });
    });
}

fn eligible(s: &Session) -> bool {
    s.is_claude() && s.is_local() && !s.stopped
}

// --- record -----------------------------------------------------------------

/// Claude's `StopFailure` hook. Everything that is not a proven quota episode
/// for an enabled local Claude session is a silent no-op.
pub fn record(home: &Path, stdin: &str) {
    watchdog(home, "record");
    let Ok(hook) = serde_json::from_str::<serde_json::Value>(stdin) else { return };
    if hook["error"] != "rate_limit" || hook.get("hook_event_name").is_some_and(|e| e != "StopFailure") {
        return;
    }
    let Some(session) = std::env::var("THURBOX_SESSION").ok().filter(|s| !s.is_empty()) else { return };
    let Some(path) = hook["transcript_path"].as_str().filter(|p| p.ends_with(".jsonl")).map(PathBuf::from) else {
        return;
    };
    let Some(ctx) = Ctx::load(home) else { return };
    let Ok(Some(s)) = ctx.tb.session(&session) else { return };
    let Some(settings) = ctx.settings(&s.id).ok().filter(|e| eligible(&s) && e.enabled.value) else { return };
    // Claude batches its transcript writes, so the row can trail the hook.
    let mut proven = None;
    for _ in 0..15 {
        if let Some(r) = fresh_rejection(&path, HOOK_FRESH_MS) {
            proven = Some(r);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let Some(r) = proven else {
        ctx.log("record", &s.id, "no proven quota episode");
        return;
    };
    let Ok(Some(_lock)) = SessionLock::acquire(&ctx.locks, &s.id, Duration::from_secs(25)) else {
        ctx.log("record", &s.id, "lock busy; left to the sweep");
        return;
    };
    arm(&ctx, &s, r, &path, settings.delay_secs.value);
}

/// The newest rejection in the transcript, if it is recent and nothing has
/// happened since.
fn fresh_rejection(path: &Path, fresh_ms: i64) -> Option<Rejection> {
    let t = Transcript::read(path).ok()?;
    let r = t.latest_rejection()?;
    (now_ms() - r.at_ms <= fresh_ms && !t.activity_after(r.at_ms, &r.uuid)).then_some(r)
}

/// Record the episode and schedule its send. Called with the session locked.
fn arm(ctx: &Ctx, s: &Session, r: Rejection, transcript: &Path, delay_secs: u64) {
    let previous = match ctx.episode(&s.id) {
        Ok(p) => p,
        Err(e) => return ctx.log("arm", &s.id, &format!("cannot read the episode: {e}")),
    };
    if previous.as_ref().is_some_and(|p| p.id == r.uuid) {
        return;
    }
    let mut ep = Episode {
        id: r.uuid.clone(),
        window: r.window.clone(),
        resets_at: r.resets_at,
        rejected_at_ms: r.at_ms,
        fire_at_ms: fire_at(&r, delay_secs),
        transcript: transcript.display().to_string(),
        state: State::Armed,
        reason: None,
        attempt: next_attempt(previous.as_ref(), r.at_ms),
        hook_state_at: s.hook_state_at,
        automation_id: None,
        sent_at_ms: None,
        updated_at_ms: 0,
    };
    if !ctx.cfg.windows.contains(&r.window) {
        return ctx.finish(&s.id, &mut ep, State::Skipped, Some("window"));
    }
    if ep.attempt > ctx.cfg.max_attempts {
        return ctx.finish(&s.id, &mut ep, State::GaveUp, Some("max-attempts"));
    }

    // Keep fleet's refuel off a session that is only waiting on its quota: it
    // restarts a `working` that has gone stale. Only when nothing has reported
    // since the rejection, so a newer state is never overwritten — and only
    // from inside the session's own pane (the hook). A signal from anywhere
    // else reaches Thurbox's database but not the pane's own state option, and
    // Thurbox's next headless tick copies the pane's stale `working` back over
    // it. The sweep therefore does not signal; a refuel restart it then fails
    // to prevent writes its prompt to the transcript, and `fire` skips.
    let in_own_pane = std::env::var("THURBOX_SESSION").is_ok_and(|v| v == s.id);
    if in_own_pane && s.hook_state_at.is_none_or(|t| t <= r.at_ms) {
        match ctx.tb.signal_idle(&s.id) {
            Ok(()) => {
                if let Ok(Some(now)) = ctx.tb.session(&s.id) {
                    ep.hook_state_at = now.hook_state_at;
                }
            }
            Err(e) => ctx.log("arm", &s.id, &format!("idle signal failed: {e}")),
        }
    }

    if let Err(e) = ctx.save(&s.id, &mut ep) {
        return ctx.log("arm", &s.id, &format!("cannot record the episode: {e}"));
    }
    ctx.drop_automations(&s.id);
    let exe =
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "thurbox-auto-continue".into());
    let words = [exe, s.id.clone(), ctx.home.display().to_string()].map(|w| platform::shell_arg(&w));
    let [Some(exe), Some(id), Some(home)] = words else {
        // Still armed: the sweep fires it once it is overdue.
        return ctx.log("arm", &s.id, "a path the platform shell cannot be handed; left to the sweep");
    };
    let command = format!("{exe} fire {id} --home {home}");
    // Again now: the calls above take time, and an `at:` in the past never fires.
    ep.fire_at_ms = fire_at(&r, delay_secs);
    match ctx.tb.schedule_exec(&format!("{AUTOMATION_PREFIX}{}", s.id), ep.fire_at_ms, &command) {
        Ok(id) => {
            ep.automation_id = Some(id);
            let _ = ctx.save(&s.id, &mut ep);
        }
        // The episode stays armed; the sweep fires it once it is overdue.
        Err(e) => ctx.log("arm", &s.id, &format!("could not schedule: {e}")),
    }
    ctx.log("arm", &s.id, &format!("armed attempt={} window={} fire_at_ms={}", ep.attempt, ep.window, ep.fire_at_ms));
}

fn fire_at(r: &Rejection, delay_secs: u64) -> i64 {
    (r.resets_at * 1000 + delay_secs as i64 * 1000).max(now_ms() + MIN_LEAD_MS)
}

// --- fire -------------------------------------------------------------------

pub fn fire(home: &Path, session: &str) {
    watchdog(home, "fire");
    let Some(ctx) = Ctx::load(home) else { return };
    let Ok(Some(_lock)) = SessionLock::acquire(&ctx.locks, session, Duration::from_secs(2)) else { return };
    fire_locked(&ctx, session, false);
}

/// Why the session is no longer where the episode left it, if it moved.
fn moved(ctx: &Ctx, session: &str, ep: &Episode) -> Result<Option<&'static str>, String> {
    let Some(s) = ctx.tb.session(session)? else { return Ok(Some("session-gone")) };
    if s.hook_state_at > ep.hook_state_at {
        return Ok(Some("hook-state-moved"));
    }
    let t = Transcript::read(Path::new(&ep.transcript)).map_err(|e| format!("transcript: {e}"))?;
    if t.activity_after(ep.rejected_at_ms, &ep.id) || t.latest_rejection().is_some_and(|r| r.uuid != ep.id) {
        return Ok(Some("transcript-moved"));
    }
    Ok(None)
}

fn fire_locked(ctx: &Ctx, session: &str, overdue_only: bool) {
    let mut ep = match ctx.episode(session) {
        Ok(Some(ep)) => ep,
        _ => return,
    };
    match ep.state {
        // Only a crash leaves a claim behind: the lock is ours now.
        State::Claimed => return ctx.finish(session, &mut ep, State::Abandoned, None),
        State::Armed => {}
        _ => return,
    }
    let now = now_ms();
    if now < ep.fire_at_ms || (overdue_only && now < ep.fire_at_ms + OVERDUE_MS) {
        return;
    }

    // Gates, before anything is claimed. A gate that cannot be read leaves the
    // episode armed for a later run.
    match ctx.tb.extension_active() {
        Ok(true) => {}
        Ok(false) => return ctx.finish(session, &mut ep, State::Skipped, Some("extension-inactive")),
        Err(e) => return ctx.log("fire", session, &format!("gate unreadable: {e}")),
    }
    // Read now, not when the limit was recorded: a switch or a message changed
    // since applies to this send.
    let settings = match ctx.settings(session) {
        Ok(e) if e.enabled.value => e,
        Ok(_) => return ctx.finish(session, &mut ep, State::Skipped, Some("disabled")),
        Err(e) => return ctx.log("fire", session, &format!("gate unreadable: {e}")),
    };
    let s = match ctx.tb.session(session) {
        Ok(Some(s)) => s,
        Ok(None) => return ctx.finish(session, &mut ep, State::Skipped, Some("session-gone")),
        Err(e) => return ctx.log("fire", session, &format!("gate unreadable: {e}")),
    };
    let why = if !s.is_claude() {
        Some("not-claude")
    } else if !s.is_local() {
        Some("remote")
    } else if s.stopped {
        Some("stopped")
    } else {
        None
    };
    if let Some(why) = why {
        return ctx.finish(session, &mut ep, State::Skipped, Some(why));
    }
    match moved(ctx, session, &ep) {
        Ok(None) => {}
        Ok(Some(why)) => return ctx.finish(session, &mut ep, State::Skipped, Some(why)),
        Err(e) => return ctx.log("fire", session, &format!("gate unreadable: {e}")),
    }

    // Claim before acting, on the episode as it is now.
    match ctx.episode(session) {
        Ok(Some(now)) if now.id == ep.id && now.state == State::Armed => {}
        _ => return,
    }
    ep.state = State::Claimed;
    if let Err(e) = ctx.save(session, &mut ep) {
        return ctx.log("fire", session, &format!("could not claim: {e}"));
    }
    let (state, reason) = match act(ctx, session, &mut ep, &settings.message.value) {
        Ok(outcome) => outcome,
        Err(e) => {
            ctx.log("fire", session, &format!("failed after the claim: {e}"));
            (State::Skipped, Some("error"))
        }
    };
    ctx.finish(session, &mut ep, state, reason);
}

/// Look, type, verify, submit, confirm. Returns the episode's outcome.
fn act(ctx: &Ctx, session: &str, ep: &mut Episode, msg: &str) -> Result<(State, Option<&'static str>), String> {
    let mut escaped = false;
    loop {
        match classify(&ctx.tb.capture(session)?, msg) {
            Screen::OurText => break,
            Screen::EmptyPrompt => {
                ctx.tb.type_text(session, msg)?;
                if !wait_for_screen(ctx, session, msg, Screen::OurText, Duration::from_secs(3))? {
                    return Ok((State::Skipped, Some("verify-failed")));
                }
                break;
            }
            Screen::LimitMenu if ctx.cfg.on_menu == OnMenu::Escape && !escaped => {
                ctx.tb.key(session, "escape")?;
                escaped = true;
                std::thread::sleep(Duration::from_millis(300));
            }
            other => return Ok((State::Skipped, Some(other.name()))),
        }
    }

    // The last look before Enter.
    if moved(ctx, session, ep)?.is_some() {
        if classify(&ctx.tb.capture(session)?, msg) == Screen::OurText {
            ctx.tb.key(session, "ctrl-u")?;
        }
        return Ok((State::Skipped, Some("moved-before-enter")));
    }
    let before = ctx.tb.session(session)?.and_then(|s| s.hook_state_at);
    // Recorded before Enter, so a run cut off after it still counts the send.
    ep.sent_at_ms = Some(now_ms());
    ctx.save(session, ep)?;
    if let Err(e) = ctx.tb.key(session, "enter") {
        // Enter may have landed even though the call failed.
        ctx.log("fire", session, &format!("enter: {e}"));
        return Ok((State::Unconfirmed, None));
    }
    ctx.log("fire", session, "submitted");

    let end = Instant::now() + Duration::from_secs(ctx.cfg.confirm_secs.min(MAX_CONFIRM_SECS));
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
        if ctx.tb.session(session).ok().flatten().is_some_and(|s| s.hook_state_at > before) {
            return Ok((State::Sent, None));
        }
    }
    Ok((State::Unconfirmed, None))
}

fn wait_for_screen(ctx: &Ctx, session: &str, msg: &str, want: Screen, limit: Duration) -> Result<bool, String> {
    let end = Instant::now() + limit;
    loop {
        if classify(&ctx.tb.capture(session)?, msg) == want {
            return Ok(true);
        }
        if Instant::now() >= end {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

// --- sweep ------------------------------------------------------------------

/// The fallback pass: an episode whose hook never ran, a send whose one-shot
/// never fired, a claim a crash left behind.
pub fn sweep(home: &Path) {
    watchdog(home, "sweep");
    let Some(ctx) = Ctx::load(home) else { return };
    let Ok(sessions) = ctx.tb.sessions() else { return };
    let claude_config = platform::claude_config_dir();
    for s in sessions.iter().filter(|s| eligible(s)) {
        let Some(settings) = ctx.settings(&s.id).ok().filter(|e| e.enabled.value) else { continue };
        let Ok(Some(_lock)) = SessionLock::acquire(&ctx.locks, &s.id, Duration::ZERO) else { continue };
        let ep = ctx.episode(&s.id).ok().flatten();
        if ep.as_ref().is_some_and(|e| matches!(e.state, State::Armed | State::Claimed)) {
            fire_locked(&ctx, &s.id, true);
            continue;
        }
        let (Some(dir), Some(agent_id)) = (&claude_config, &s.agent_session_id) else { continue };
        let Some(path) = crate::transcript::locate(dir, agent_id) else { continue };
        if let Some(r) = fresh_rejection(&path, SWEEP_FRESH_MS)
            && ep.as_ref().is_none_or(|e| e.id != r.uuid)
        {
            ctx.log("sweep", &s.id, "found an episode the hook missed");
            arm(&ctx, s, r, &path, settings.delay_secs.value);
        }
    }
}
