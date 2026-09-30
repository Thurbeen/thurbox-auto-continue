//! The local, headless path end to end: a real `thurbox-cli` and tmux server in
//! a sandbox, the TUI never started, a fake Claude that runs hooks the way
//! Claude does, and Thurbox's own `automation tick` firing the send.

mod support;

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use support::Sandbox;

fn stopfailure_hooks(settings: &Value) -> Vec<String> {
    settings["hooks"]["StopFailure"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(String::from))
        .collect()
}

/// Install merges our hook beside the user's own, leaves Thurbox's `--settings`
/// hooks alone, and uninstall takes out ours and nothing else.
#[test]
fn install_merges_the_hook_and_uninstall_removes_only_ours() {
    let sb = Sandbox::bare();
    let before_thurbox_hooks = std::fs::read_to_string(sb.root.join("cfg/hooks/claude.json")).unwrap();
    sb.install();

    let merged = sb.settings();
    assert_eq!(merged["theme"], "dark");
    assert_eq!(merged["hooks"]["Stop"][0]["hooks"][0]["command"], "echo user-stop-hook");
    let ours: Vec<String> =
        stopfailure_hooks(&merged).into_iter().filter(|c| c.contains("thurbox-auto-continue record")).collect();
    assert_eq!(ours.len(), 1, "exactly one hook of ours: {merged}");
    assert!(!ours[0].contains("thurbox-cli session signal"), "uninstall would prune it by that text");
    assert!(stopfailure_hooks(&merged).contains(&"echo user-stopfailure-hook".to_string()));
    let group =
        merged["hooks"]["StopFailure"].as_array().unwrap().iter().find(|g| g["matcher"] == "rate_limit").unwrap();
    assert_eq!(group["hooks"].as_array().unwrap().len(), 1);

    // Thurbox's own hooks still reach Claude through their `--settings` file.
    let args = sb.cli(&["agent", "launch-args", "claude"]);
    assert!(args["args"].as_array().unwrap().iter().any(|a| a == "--settings"), "{args}");
    assert_eq!(std::fs::read_to_string(sb.root.join("cfg/hooks/claude.json")).unwrap(), before_thurbox_hooks);

    // Installing again does not add a second copy.
    sb.install();
    let again: Vec<String> =
        stopfailure_hooks(&sb.settings()).into_iter().filter(|c| c.contains("thurbox-auto-continue")).collect();
    assert_eq!(again.len(), 1);

    let out = sb.tac(&["forget", "--all"]);
    assert!(out.status.success());
    sb.cli(&["extension", "uninstall", "auto-continue", "--purge"]);
    let after = sb.settings();
    assert!(stopfailure_hooks(&after).iter().all(|c| !c.contains("thurbox-auto-continue")), "{after}");
    assert_eq!(after["theme"], "dark");
    assert_eq!(after["hooks"]["Stop"][0]["hooks"][0]["command"], "echo user-stop-hook");
    assert!(stopfailure_hooks(&after).contains(&"echo user-stopfailure-hook".to_string()));
    assert_eq!(std::fs::read_to_string(sb.root.join("cfg/hooks/claude.json")).unwrap(), before_thurbox_hooks);
}

/// A1 + A19: a quota rejection, TUI never started, gets exactly one message
/// once the window has reset — fired by Thurbox's own `automation tick`.
#[test]
fn a_quota_limit_gets_exactly_one_continue_after_the_reset() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);

    let ep = sb.hit_limit(&id);
    assert_eq!(ep["window"], "five_hour");
    assert_eq!(ep["attempt"], 1);

    // Both hook sources ran: Thurbox's from `--settings`, ours from settings.json.
    let hooks = sb.ctl_file(&id, "hooks.log");
    // Either separator: Thurbox hands Claude a forward-slash path on Windows.
    let from_thurbox =
        |l: &str| l.starts_with("UserPromptSubmit") && l.replace('\\', "/").contains("cfg/hooks/claude.json");
    assert!(hooks.lines().any(from_thurbox), "{hooks}");
    let ours = hooks
        .lines()
        .find(|l| l.starts_with("StopFailure") && l.contains("thurbox-auto-continue record"))
        .expect(&hooks);
    assert!(ours.contains("exit=0") && ours.contains("stdout_bytes=0"), "{ours}");

    // One pending send, and the session no longer reads as a stale `working`.
    let pending = sb.our_automations();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0]["name"], format!("auto-continue/{id}"));
    assert_eq!(sb.session_row(&id)["hook_state"], "idle");

    sb.tick_until_fired(&id);
    assert_eq!(sb.state(&id).as_deref(), Some("sent"), "{:?}", sb.episode(&id));
    assert_eq!(sb.received(&id), ["hello", "continue"]);

    // Nothing sends it twice: more ticks, a sweep, and a direct fire.
    sb.tick();
    assert!(sb.tac(&["sweep"]).status.success());
    assert!(sb.tac(&["fire", &id]).status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sb.received(&id), ["hello", "continue"]);
    assert!(sb.our_automations().is_empty(), "the one-shot is cleaned up");
}

/// A2: a transient 429 schedules nothing and signals nothing.
#[test]
fn a_transient_429_schedules_nothing() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["transient"]);
    sb.prompt(&id, "hello");
    sb.wait("the StopFailure hook to run", Duration::from_secs(15), || {
        sb.ctl_file(&id, "hooks.log").contains("StopFailure")
    });
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(sb.episode(&id), None);
    assert!(sb.our_automations().is_empty());
    assert_ne!(sb.session_row(&id)["hook_state"], "idle");
}

/// A3: before the reset nothing is sent, however often it is asked.
#[test]
fn nothing_is_sent_before_the_reset() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:3600:cancelled", "ok"]);
    let ep = sb.hit_limit(&id);
    assert!(ep["fire_at_ms"].as_i64().unwrap() > ep["resets_at"].as_i64().unwrap() * 1000 - 1);
    assert!(sb.tac(&["fire", &id]).status.success());
    sb.tick();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sb.received(&id), ["hello"]);
    assert_eq!(sb.state(&id).as_deref(), Some("armed"));
}

/// A11 + "off means no send": the global default is off, a session toggle
/// turns one session on, and turning it off before the fire sends nothing.
#[test]
fn off_by_default_on_per_session_and_off_again_means_no_send() {
    let sb = Sandbox::new();
    let off = sb.session("off", "claude");
    let on = sb.session("on", "claude");
    assert!(sb.tac(&["enable", &on]).status.success());
    for id in [&off, &on] {
        sb.script(id, &["limit:five_hour:1:cancelled", "ok"]);
        sb.prompt(id, "hello");
    }
    sb.wait("the enabled session to arm", Duration::from_secs(20), || sb.state(&on).as_deref() == Some("armed"));
    assert_eq!(sb.episode(&off), None, "a session that is off gets no episode");
    assert_ne!(sb.session_row(&off)["hook_state"], "idle", "and no signal");

    assert!(sb.tac(&["disable", &on]).status.success());
    sb.tick_until_fired(&on);
    assert_eq!(sb.state(&on).as_deref(), Some("skipped"));
    assert_eq!(sb.received(&on), ["hello"]);

    // And the status a plugin reads says so.
    let out = sb.tac(&["status", "--json"]);
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["schema"], 2);
    let row = status["sessions"].as_array().unwrap().iter().find(|s| s["id"] == on.as_str()).unwrap().clone();
    assert_eq!(row["overrides"]["enabled"], "off");
    assert_eq!(row["enabled"], false);
    assert_eq!(row["last_outcome"]["label"], "skipped:disabled");
}

/// A12: a non-Claude session is never acted on, and `enable` refuses it.
#[test]
fn a_non_claude_session_is_refused_and_ignored() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let shell = sb.session("shell", "shell");
    let out = sb.tac(&["enable", &shell]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only acts on Claude"));

    // Even a hook fired with that session's identity does nothing.
    let payload =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stopfailure-hook-input.json"))
            .unwrap();
    let mut child = sb
        .command(support::BIN)
        .arg("record")
        .env("THURBOX_SESSION", &shell)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success() && out.stdout.is_empty());
    assert_eq!(sb.episode(&shell), None);
}

/// A13: the weekly window is not acted on unless asked for.
#[test]
fn a_seven_day_window_is_skipped_by_default() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:seven_day:1:cancelled", "ok"]);
    sb.prompt(&id, "hello");
    sb.wait("the StopFailure hook to run", Duration::from_secs(15), || {
        sb.ctl_file(&id, "hooks.log").contains("StopFailure")
    });
    std::thread::sleep(Duration::from_secs(3));
    assert!(sb.our_automations().is_empty());
    assert!(sb.state(&id).is_none_or(|s| s == "skipped"));
    assert_eq!(sb.received(&id), ["hello"]);
}

/// `record` outside a Thurbox session, or with the binary missing from PATH,
/// is silent and never fails Claude's hook.
#[test]
fn the_hook_is_silent_outside_thurbox_and_without_the_binary() {
    let sb = Sandbox::new();
    let payload =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stopfailure-hook-input.json"))
            .unwrap();
    let hook = support::stopfailure_command(&sb);
    let without_bin = std::env::join_paths(std::env::split_paths(&sb.path()).filter(|d| *d != sb.bin)).unwrap();
    for path in [sb.path(), without_bin] {
        let mut child = sb
            .command(support::hook_shell())
            .arg("-c")
            .arg(&hook)
            .env("PATH", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "PATH={path:?}");
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "PATH={path:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(sb.our_automations().is_empty());
}

/// The kill switch: deactivating the extension stops a send already scheduled.
#[test]
fn deactivating_the_extension_stops_a_pending_send() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    sb.hit_limit(&id);
    sb.cli(&["extension", "deactivate", "auto-continue"]);
    sb.tick_until_fired(&id);
    assert_eq!(sb.episode(&id).unwrap()["reason"], "extension-inactive");
    assert_eq!(sb.received(&id), ["hello"]);
}

/// The sweep picks up an episode whose hook never ran.
#[test]
fn the_sweep_recovers_an_episode_the_hook_missed() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled:nohook", "ok"]);
    sb.prompt(&id, "hello");
    sb.wait("the rejection to be written", Duration::from_secs(15), || sb.received(&id).len() == 1);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(sb.episode(&id), None);
    assert!(sb.tac(&["sweep"]).status.success());
    assert_eq!(sb.state(&id).as_deref(), Some("armed"));
    sb.tick_until_fired(&id);
    assert_eq!(sb.received(&id), ["hello", "continue"], "{:?}", sb.episode(&id));
}

/// `install.sh` leaves the extension active with the hook merged and the
/// binary on PATH, and `--uninstall` takes all of it away again.
#[test]
fn the_installer_round_trips() {
    let sb = Sandbox::bare();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh");
    let out = sb.command(support::hook_shell()).args([script, "--binary", support::BIN]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(sb.active("auto-continue"));
    assert!(sb.home.join(".local/bin").join(support::exe("thurbox-auto-continue")).exists());
    assert!(sb.ext_home.join("bin").join(support::exe("thurbox-auto-continue")).is_file());
    assert!(sb.ext_home.join("config.toml").is_file());
    assert!(stopfailure_hooks(&sb.settings()).iter().any(|c| c.contains("thurbox-auto-continue record")));
    let sweep = sb.cli(&["automation", "list"]).as_array().unwrap().iter().any(|a| a["name"] == "auto-continue-sweep");
    assert!(sweep, "the fallback sweep is scheduled");

    let out = sb.command(support::hook_shell()).args([script, "--uninstall"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!sb.cli(&["extension", "list"]).as_array().unwrap().iter().any(|e| e["name"] == "auto-continue"));
    assert!(!sb.home.join(".local/bin").join(support::exe("thurbox-auto-continue")).exists());
    assert!(stopfailure_hooks(&sb.settings()).iter().all(|c| !c.contains("thurbox-auto-continue")));
    assert_eq!(sb.settings()["hooks"]["Stop"][0]["hooks"][0]["command"], "echo user-stop-hook");

    // A second uninstall, with nothing left to remove, still succeeds.
    let out = sb.command(support::hook_shell()).args([script, "--uninstall"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
