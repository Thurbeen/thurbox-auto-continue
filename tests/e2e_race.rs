//! The safety gates and the races: what must NOT be sent, and what must never
//! be sent twice.

mod support;

use std::time::{Duration, Instant};

use support::Sandbox;

fn armed(sb: &Sandbox, replies: &[&str]) -> String {
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, replies);
    sb.hit_limit(&id);
    // The send is due: nothing below waits on the clock.
    let mut ep = sb.episode(&id).unwrap();
    ep["fire_at_ms"] = serde_json::json!(0);
    sb.cli(&["session", "meta", "set", &id, "auto-continue.episode", "--", &ep.to_string()]);
    id
}

/// A5: eight concurrent fires give exactly one send.
#[test]
fn eight_concurrent_fires_send_once() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    let children: Vec<_> = (0..8)
        .map(|_| sb.command(support::BIN).args(["fire", &id, "--home"]).arg(&sb.ext_home).spawn().unwrap())
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sb.received(&id), ["hello", "continue"]);
    assert_eq!(sb.state(&id).as_deref(), Some("sent"));
}

/// A5: a crash after the claim loses the send rather than doubling it.
#[test]
fn a_claim_left_by_a_crash_is_abandoned_not_resent() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    let mut ep = sb.episode(&id).unwrap();
    ep["state"] = serde_json::json!("claimed");
    sb.cli(&["session", "meta", "set", &id, "auto-continue.episode", "--", &ep.to_string()]);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert!(sb.tac(&["sweep"]).status.success());
    assert_eq!(sb.state(&id).as_deref(), Some("abandoned"));
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A6: a newer user row in the transcript means the session moved on.
#[test]
fn a_newer_transcript_row_skips() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    let ep = sb.episode(&id).unwrap();
    let path = ep["transcript"].as_str().unwrap().to_string();
    let row = serde_json::json!({
        "type": "user", "uuid": "11111111-2222-3333-4444-555555555555",
        "timestamp": "2099-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": "I am back" }
    });
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!("{row}\n"));
    std::fs::write(&path, text).unwrap();
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.episode(&id).unwrap()["reason"], "transcript-moved");
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A6: a newer `hook_state_at` means something reported since the limit.
#[test]
fn a_newer_hook_state_skips() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    std::thread::sleep(Duration::from_millis(20));
    sb.cli(&["session", "signal", "--session", &id, "--state", "working"]);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.episode(&id).unwrap()["reason"], "hook-state-moved");
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A7: Claude's own resume wins; we send nothing after it.
#[test]
fn claude_resuming_by_itself_gets_nothing_from_us() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on"), ("delay_secs", "4")]);
    let id = sb.session("worker", "claude");
    std::fs::create_dir_all(sb.ctl.join(&id)).unwrap();
    std::fs::write(sb.ctl.join(&id).join("native_resume_secs"), "2").unwrap();
    sb.script(&id, &["limit:five_hour:1:armed", "ok"]);
    sb.hit_limit(&id);
    sb.wait("the native resume", Duration::from_secs(15), || sb.keys(&id).contains(&"native-resume".to_string()));
    sb.tick_until_fired(&id);
    assert_eq!(sb.state(&id).as_deref(), Some("skipped"));
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A8: the limit menu is closed with one Esc, then the message goes in.
#[test]
fn the_limit_menu_gets_one_escape_then_the_message() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:menu", "ok"]);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.received(&id), ["hello", "continue"], "{:?} {:?}", sb.episode(&id), sb.keys(&id));
    assert_eq!(sb.keys(&id).iter().filter(|k| *k == "escape").count(), 1);
    assert_eq!(sb.state(&id).as_deref(), Some("sent"));
}

/// A8: `on_menu = "skip"` leaves the menu alone.
#[test]
fn the_limit_menu_is_left_alone_when_asked() {
    let sb = Sandbox::new();
    sb.config(&[("on_menu", "skip")]);
    let id = armed(&sb, &["limit:five_hour:1:menu", "ok"]);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.episode(&id).unwrap()["reason"], "limit-menu");
    assert!(!sb.keys(&id).contains(&"escape".to_string()));
}

/// A8: Claude's own countdown is never cancelled and never typed over.
#[test]
fn an_armed_native_wait_is_skipped_without_escape() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:armed", "ok"]);
    let keys_before = sb.keys(&id);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.episode(&id).unwrap()["reason"], "armed-wait");
    assert_eq!(sb.keys(&id), keys_before, "not one key pressed");
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A9: text the user left in the composer is never added to or submitted.
#[test]
fn other_text_in_the_composer_gets_nothing() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    sb.cli(&["session", "send", "--no-enter", &id, "--", "half a thought"]);
    sb.wait("the typed text to show", Duration::from_secs(5), || sb.screen(&id).contains("half a thought"));
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.episode(&id).unwrap()["reason"], "other-text");
    assert!(sb.screen(&id).contains("❯ half a thought\n"));
    assert_eq!(sb.received(&id), ["hello"]);
}

/// A9: our own text already in the composer gets Enter only, not a second copy.
#[test]
fn our_own_text_in_the_composer_gets_enter_only() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    sb.cli(&["session", "send", "--no-enter", &id, "--", "continue"]);
    sb.wait("the typed text to show", Duration::from_secs(5), || sb.screen(&id).contains("❯ continue"));
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.received(&id), ["hello", "continue"]);
}

/// A10: a re-rejection re-arms; past `max_attempts` it gives up.
#[test]
fn re_rejections_re_arm_until_the_cap() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on"), ("max_attempts", "2")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "limit:five_hour:1:cancelled", "limit:five_hour:1:cancelled"]);
    sb.hit_limit(&id);
    sb.tick_until_fired(&id);
    sb.wait("the re-rejection to re-arm", Duration::from_secs(20), || {
        sb.episode(&id).is_some_and(|e| e["attempt"] == 2 && e["state"] == "armed")
    });
    sb.tick_until_fired(&id);
    sb.wait("the cap", Duration::from_secs(20), || sb.state(&id).as_deref() == Some("gave-up"));
    assert_eq!(sb.received(&id), ["hello", "continue", "continue"]);
    assert!(sb.our_automations().is_empty());
}

/// A20: a `thurbox-cli` that hangs cannot hang a run.
#[test]
fn a_hung_thurbox_cli_cannot_hang_a_run() {
    let sb = Sandbox::new();
    let hung = sb.root.join("hung-thurbox-cli");
    std::fs::write(&hung, "#!/bin/sh\nexec sleep 600\n").unwrap();
    std::fs::set_permissions(&hung, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for args in [&["fire", "some-session"][..], &["sweep"][..]] {
        let start = Instant::now();
        let out = sb
            .command(support::BIN)
            .args(args)
            .arg("--home")
            .arg(&sb.ext_home)
            .env("THURBOX_AUTO_CONTINUE_THURBOX_CLI", &hung)
            .env("THURBOX_AUTO_CONTINUE_CLI_TIMEOUT_MS", "500")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        assert!(start.elapsed() < Duration::from_secs(10), "{args:?} took {:?}", start.elapsed());
    }
}

/// Fleet's `refuel` restarts a session whose `working` has gone stale for 30
/// minutes. On the hook path the episode signals `idle`, so that condition
/// cannot hold (asserted in the A1 test). On the sweep path no signal is
/// possible from outside the pane, the session still reads `working`, and a
/// refuel restart can happen: its prompt lands in the transcript and our send
/// is skipped, so the session gets one prompt, not two.
#[test]
fn a_refuel_restart_over_a_sweep_armed_episode_is_not_doubled() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled:nohook", "ok"]);
    sb.prompt(&id, "hello");
    sb.wait("the rejection", Duration::from_secs(15), || sb.received(&id).len() == 1);
    std::thread::sleep(Duration::from_millis(500));
    assert!(sb.tac(&["sweep"]).status.success());
    assert_eq!(sb.state(&id).as_deref(), Some("armed"));
    assert_eq!(sb.session_row(&id)["hook_state"], "working", "refuel's stale-working condition can match here");

    // What refuel does: restart with --resume, then prompt.
    sb.cli(&["session", "restart", &id]);
    sb.wait("the restarted fake", Duration::from_secs(15), || sb.screen(&id).contains("(fake)"));
    std::thread::sleep(Duration::from_millis(500));
    sb.prompt(&id, "keep going");
    sb.wait("refuel's prompt", Duration::from_secs(15), || sb.received(&id).contains(&"keep going".to_string()));

    let mut ep = sb.episode(&id).unwrap();
    ep["fire_at_ms"] = serde_json::json!(0);
    sb.cli(&["session", "meta", "set", &id, "auto-continue.episode", "--", &ep.to_string()]);
    assert!(sb.tac(&["fire", &id]).status.success());
    assert_eq!(sb.state(&id).as_deref(), Some("skipped"));
    assert!(!sb.received(&id).contains(&"continue".to_string()), "{:?}", sb.received(&id));
}

/// A14: a session stuck in `working` with no limit in its transcript is none
/// of our business: no episode, no signal.
#[test]
fn a_stale_working_session_without_an_episode_is_untouched() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.cli(&["session", "signal", "--session", &id, "--state", "working"]);
    let before = sb.session_row(&id)["hook_state_at"].clone();
    assert!(sb.tac(&["sweep"]).status.success());
    assert_eq!(sb.episode(&id), None);
    assert_eq!(sb.session_row(&id)["hook_state_at"], before);
    assert!(sb.our_automations().is_empty());
}

/// Runs started with different `--home` directories still share one lock, so
/// a late one-shot and an overdue sweep cannot both act on a session.
#[test]
fn fires_under_two_homes_still_send_once() {
    let sb = Sandbox::new();
    let id = armed(&sb, &["limit:five_hour:1:cancelled", "ok"]);
    let other = sb.root.join("other-home");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(sb.ext_home.join("config.toml"), other.join("config.toml")).unwrap();
    let children: Vec<_> = (0..8)
        .map(|i| {
            let home = if i % 2 == 0 { &sb.ext_home } else { &other };
            sb.command(support::BIN).args(["fire", &id, "--home"]).arg(home).spawn().unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sb.received(&id), ["hello", "continue"]);
}
