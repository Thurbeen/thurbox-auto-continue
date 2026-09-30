//! The paths that differ per platform, end to end. On Linux and macOS they
//! run against tmux and `sh -c`; on native Windows against psmux, Thurbox's
//! PowerShell keeper, `cmd /C` for every Exec automation, and Git Bash for
//! Claude's hooks. The rest of the suite runs on every platform too; these are
//! the parts a port could get wrong without any of the others noticing.

mod support;

use std::time::Duration;

use serde_json::Value;
use support::Sandbox;
use thurbox_auto_continue::platform;
use thurbox_auto_continue::screen::{Screen, classify};

/// Thurbox's own keeper — a `while` loop in its heartbeat window, PowerShell
/// under psmux — fires the one-shot with no tick of ours, through the
/// platform shell.
#[test]
fn the_keeper_fires_the_send_with_no_tick_of_ours() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    sb.hit_limit(&id);
    // The keeper ticks every 60 s; the send is due about 5 s from now.
    sb.wait("the keeper to fire the send", Duration::from_secs(150), || {
        !matches!(sb.state(&id).as_deref(), Some("armed") | Some("claimed"))
    });
    assert_eq!(sb.state(&id).as_deref(), Some("sent"), "{:?}", sb.episode(&id));
    assert_eq!(sb.received(&id), ["hello", "continue"]);
}

/// A user profile can hold a space (`C:\Users\First Last`). The one-shot's
/// command names our binary and the extension home by absolute path, and the
/// platform shell has to read both back whole.
#[test]
fn a_home_with_a_space_still_gets_its_send() {
    let sb = Sandbox::with_prefix("tac with space-");
    sb.install();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    sb.hit_limit(&id);
    match sb.our_automations().first() {
        Some(a) => {
            let command = a["action"]["command"].as_str().unwrap_or_default().to_string();
            if cfg!(unix) {
                assert!(command.contains(" with space-"), "{command}");
            }
            sb.tick_until_fired(&id);
        }
        // Windows, where `cmd /C` cannot be handed the path and the volume
        // keeps no short name for it: the episode stays armed for the sweep,
        // which sends it once overdue.
        None => {
            if cfg!(unix) {
                panic!("POSIX always schedules");
            }
            let log = std::fs::read_to_string(sb.ext_home.join("auto-continue.log")).unwrap_or_default();
            assert!(log.contains("left to the sweep"), "{log}");
            let mut ep = sb.episode(&id).unwrap();
            ep["fire_at_ms"] = serde_json::json!(0);
            sb.cli(&["session", "meta", "set", &id, "auto-continue.episode", "--", &ep.to_string()]);
            assert!(sb.tac(&["sweep"]).status.success());
        }
    }
    assert_eq!(sb.state(&id).as_deref(), Some("sent"), "{:?}", sb.episode(&id));
    assert_eq!(sb.received(&id), ["hello", "continue"]);
}

/// The sweep the manifest declares runs clean under the platform shell.
#[test]
fn the_manifest_sweep_runs_under_the_platform_shell() {
    let sb = Sandbox::new();
    let sweep = sb
        .cli(&["automation", "list"])
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == "auto-continue-sweep")
        .cloned()
        .expect("the sweep is declared");
    let sweep_id = sweep["id"].as_i64().unwrap().to_string();
    sb.cli(&["automation", "run", &sweep_id]);
    sb.wait("the sweep to run", Duration::from_secs(30), || {
        sb.tick();
        !runs(&sb, &sweep_id).is_empty()
    });
    let run = runs(&sb, &sweep_id).remove(0);
    assert_eq!(run["status"], "success", "{}: {run}", sweep["action"]["command"]);
}

/// Every Exec automation runs its command as `sh -c <command>`, or on Windows
/// `cmd /C <command>`. The one-shot names our binary and the extension home by
/// path, spelled by `platform::shell_arg`. A path with a space in it — a
/// Windows profile like `C:\Users\First Last` — either comes through whole or
/// is refused up front (the sweep then sends it), never mangled into a
/// command that fails.
#[test]
fn an_exec_command_spelled_for_the_platform_shell_runs() {
    let sb = Sandbox::new();
    let spaced = sb.root.join("a dir with spaces");
    std::fs::create_dir_all(&spaced).unwrap();
    let spaced_bin = spaced.join(support::exe("thurbox-auto-continue"));
    support::link(std::path::Path::new(support::BIN), &spaced_bin);
    let bin = std::path::Path::new(support::BIN);
    let cases = [
        ("plain", bin, sb.ext_home.as_path()),
        ("spaced binary", spaced_bin.as_path(), sb.ext_home.as_path()),
        ("spaced home", bin, spaced.as_path()),
        ("both spaced", spaced_bin.as_path(), spaced.as_path()),
    ];
    let mut outcomes = Vec::new();
    for (what, exe, home) in cases {
        let arg = |p: &std::path::Path| platform::shell_arg(&p.display().to_string());
        let (Some(exe), Some(home)) = (arg(exe), arg(home)) else {
            if cfg!(unix) {
                panic!("{what}: POSIX quotes any path");
            }
            outcomes.push(format!("{what}: refused"));
            continue;
        };
        let command = format!("{exe} config show --home {home}");
        let v = sb.cli(&["automation", "create", "--name", what, "--trigger", "cron:0 0 1 1 *", "--command", &command]);
        let id = v["id"].as_i64().unwrap().to_string();
        sb.cli(&["automation", "run", &id]);
        sb.wait("the automation to run", Duration::from_secs(30), || {
            sb.tick();
            !runs(&sb, &id).is_empty()
        });
        let run = runs(&sb, &id).remove(0);
        outcomes.push(format!("{what}: {} {} <- {command}", run["status"], run["detail"]));
    }
    eprintln!("{outcomes:#?}");
    let failed: Vec<_> = outcomes.iter().filter(|o| !o.contains("\"success\"") && !o.ends_with("refused")).collect();
    assert!(failed.is_empty(), "{outcomes:#?}");
    assert!(outcomes[0].contains("\"success\""), "a plain path always runs: {outcomes:#?}");
}

fn runs(sb: &Sandbox, id: &str) -> Vec<Value> {
    sb.cli(&["automation", "runs", id]).as_array().cloned().unwrap_or_default()
}

/// The keys `fire` presses reach the agent as the bytes Claude reads — Esc,
/// Enter and Ctrl-U — and text typed with `--no-enter` lands in the composer
/// unsubmitted.
#[test]
fn the_keys_fire_presses_reach_the_agent() {
    let sb = Sandbox::new();
    let id = sb.session("worker", "claude");
    sb.cli(&["session", "send", "--no-enter", &id, "--", "half a thought"]);
    // Read the way `fire` reads it: the last composer on the screen.
    let (sb, id) = (&sb, id.as_str());
    let shows = |want: Screen| move || classify(&sb.screen(id), "continue") == want;
    sb.wait("the typed text to show", Duration::from_secs(10), shows(Screen::OtherText));
    assert!(sb.received(id).is_empty(), "typed, not submitted");
    sb.cli(&["session", "key", id, "ctrl-u"]);
    sb.wait("ctrl-u to clear the composer", Duration::from_secs(10), shows(Screen::EmptyPrompt));
    sb.cli(&["session", "key", id, "escape"]);
    sb.wait("escape to arrive", Duration::from_secs(10), || sb.keys(id).contains(&"escape".to_string()));
    sb.cli(&["session", "send", "--no-enter", id, "--", "continue"]);
    sb.wait("our text to show", Duration::from_secs(10), shows(Screen::OurText));
    sb.cli(&["session", "key", id, "enter"]);
    sb.wait("enter to submit", Duration::from_secs(10), || sb.received(id) == ["continue"]);
    let keys = sb.keys(id);
    for k in ["ctrl-u", "escape", "enter"] {
        assert!(keys.contains(&k.to_string()), "{k} in {keys:?}");
    }
}

/// Claude's hook runs the merged command from the user's own settings file —
/// `%USERPROFILE%\.claude\settings.json` on Windows — under its hook shell,
/// finds our binary on PATH and reads the payload on stdin: the episode is
/// armed from the hook alone, and the hook prints nothing.
#[test]
fn the_merged_hook_arms_from_its_stdin() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:3600:cancelled", "ok"]);
    let ep = sb.hit_limit(&id);
    let settings = sb.home.join(".claude").join("settings.json");
    let line = sb
        .ctl_file(&id, "hooks.log")
        .lines()
        .find(|l| l.starts_with("StopFailure") && l.contains("thurbox-auto-continue record"))
        .map(String::from)
        .expect("our hook ran");
    assert!(line.contains(&format!("source={}", settings.display())), "{line}");
    assert!(line.contains("exit=0") && line.contains("stdout_bytes=0"), "{line}");
    assert_eq!(ep["window"], "five_hour");
    // The transcript path came from the payload, as Claude spells it.
    assert!(std::path::Path::new(ep["transcript"].as_str().unwrap()).is_file(), "{ep}");
}
