//! The Thurbox plugin, end to end: installed with `thurbox-cli plugin install`
//! into a disposable interface directory (`THURBOX_UI_DIR`), loaded by
//! `plugin check`, and then driven by `tests/ui/live.lua` — the installed pane
//! and badge, their `run` executed for real against the sandbox's Thurbox and
//! this crate's binary. The TUI is never started: the send itself is the
//! headless one, fired by Thurbox's heartbeat.
//!
//! Needs Lua 5.4 (`TAC_LUA`, else `lua5.4` or a `lua` that is 5.4) besides what
//! the rest of the suite needs.

mod support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::Value;
use support::{Link, Sandbox};

const PANE: &str = "ui/plugins/85_auto_continue.lua";
const BADGE: &str = "ui/plugins/86_auto_continue_badge.lua";

fn lua() -> PathBuf {
    if let Some(p) = std::env::var_os("TAC_LUA") {
        return PathBuf::from(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["lua5.4", "lua54", "lua"] {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                let out = std::process::Command::new(&candidate).arg("-v").output();
                if out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Lua 5.4")) {
                    return candidate;
                }
            }
        }
    }
    panic!("these tests need Lua 5.4: put lua5.4 on PATH or set TAC_LUA");
}

/// A sandbox whose interface directory is its own, empty until the plugin is
/// installed into it.
fn with_ui(sb: Sandbox) -> (Sandbox, PathBuf) {
    let ui = sb.root.join("ui");
    std::fs::create_dir_all(&ui).unwrap();
    let ui_s = ui.display().to_string();
    (sb.with_env("THURBOX_UI_DIR", &ui_s), ui)
}

/// This checkout's `ui/`, committed into a repository named as the published
/// one: `plugin install` names the working copy after the source, and the pane
/// requires its modules by that name. The working tree, not HEAD, so the test
/// runs the code being changed.
fn plugin_source(sb: &Sandbox) -> PathBuf {
    let src = sb.root.join("src/thurbox-auto-continue");
    std::fs::create_dir_all(&src).unwrap();
    let from = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ok = std::process::Command::new("cp").arg("-R").arg(from.join("ui")).arg(&src).status().unwrap().success();
    assert!(ok, "copy ui/");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec!["-c", "commit.gpgsign=false", "commit", "-qm", "plugin"],
    ] {
        let ok = sb.command("git").args(&args).current_dir(&src).status().unwrap().success();
        assert!(ok, "git {args:?}");
    }
    src
}

fn install_plugin(sb: &Sandbox) {
    let src = plugin_source(sb);
    let url = format!("git+file://{}", src.display());
    for file in [PANE, BADGE] {
        sb.cli(&["plugin", "install", &url, "--as", file]);
    }
}

fn plugin_check(sb: &Sandbox) -> (bool, String) {
    let out = sb.command(sb.bin.join("thurbox-cli")).args(["plugin", "check", "--text"]).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Run `steps` through the installed pane (see tests/ui/live.lua).
fn live(sb: &Sandbox, ui: &Path, steps: &str) -> Output {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui");
    let scratch = sb.root.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let mut child = sb
        .command(lua())
        .arg(here.join("live.lua"))
        .env("HERE", &here)
        .env("UI", ui)
        .env("PLUGIN_ROOT", ui.join("thurbox-auto-continue"))
        .env("SCRATCH", &scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(steps.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn live_ok(sb: &Sandbox, ui: &Path, steps: &str) -> String {
    let out = live(sb, ui, steps);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the pane did not do what the steps expect:\n{text}");
    text
}

fn show(sb: &Sandbox, session: Option<&str>) -> Value {
    let mut args = vec!["config", "show", "--json"];
    if let Some(s) = session {
        args.extend(["--session", s]);
    }
    let out = sb.tac(&args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn meta(sb: &Sandbox, id: &str, key: &str) -> Option<String> {
    sb.cli(&["session", "meta", "get", id, key])["value"].as_str().map(String::from)
}

/// Install, load, configure from the pane, refuse bad input, stay away from a
/// session that is not Claude's, and uninstall without a trace.
#[test]
fn plugin_installs_configures_and_uninstalls() {
    let (sb, ui) = with_ui(Sandbox::new());
    let worker = sb.session("worker", "claude");
    let shell = sb.session("shell", "shell");

    install_plugin(&sb);
    let (ok, text) = plugin_check(&sb);
    assert!(ok, "plugin check failed:\n{text}");
    assert!(text.contains("auto-continue") && text.contains("auto-continue-badge"), "{text}");
    assert!(!text.contains("claimed by both"), "a chord conflict:\n{text}");

    // Before the user grants `run`, the pane says how, and changes nothing.
    live_ok(&sb, &ui, "untrust\nsettle\nexpect run capability\nexpect 85_auto_continue.lua\n");

    // Off by default, and said so.
    live_ok(&sb, &ui, "settle\nselect Every Claude session\nsettle\nexpect off (default)\nexpect extension active\n");

    // The global message and delay, then a session's own, through the editor.
    live_ok(
        &sb,
        &ui,
        "settle\n\
         select Every Claude session\nsettle\n\
         key m\ntype carry on\nkey enter\nsettle\nexpect ✓ saved\n\
         key d\ntype 0\nkey enter\nsettle\nexpect ✓ saved\n\
         select worker\nsettle\n\
         key e\nsettle\nexpect ✓ saved worker on\n\
         key m\ntype resume the task\nkey enter\nsettle\nexpect ✓ saved worker message\n\
         expect resume the task\n",
    );
    let global = show(&sb, None);
    assert_eq!(global["message"]["value"], "carry on");
    assert_eq!(global["delay_secs"]["value"], 0);
    assert_eq!(global["enabled"]["value"], false, "the global switch was not touched");
    let own = show(&sb, Some(&worker));
    assert_eq!(own["enabled"]["value"], true);
    assert_eq!(own["enabled"]["source"], "session");
    assert_eq!(own["message"]["value"], "resume the task");
    assert_eq!(own["message"]["source"], "session");

    // Bad input never reaches the config; a refusal from the CLI is shown.
    live_ok(
        &sb,
        &ui,
        "settle\nselect worker\nsettle\n\
         key d\ntype -3\nkey enter\nexpect 0 to 86400\nkey esc\n\
         key m\nkey enter\nexpect must not be blank\n\
         key esc\nkey m\ntype !rm -rf\nkey enter\nexpect must not start with\nkey esc\n",
    );
    assert_eq!(show(&sb, Some(&worker))["message"]["value"], "resume the task");
    assert_eq!(show(&sb, Some(&worker))["delay_secs"]["source"], "global");

    // A session that is not Claude's: no controls, nothing written.
    live_ok(
        &sb,
        &ui,
        "settle\nselect shell\nsettle\nexpect not a Claude session\nreject e on/off\nkey e\nkey m\nsettle\n",
    );
    assert_eq!(meta(&sb, &shell, "auto-continue.enabled"), None);
    assert_eq!(meta(&sb, &shell, "auto-continue.message"), None);

    // Back to the global value from the pane.
    live_ok(&sb, &ui, "settle\nselect worker\nsettle\nkey M\nsettle\nexpect back to the global value\n");
    assert_eq!(show(&sb, Some(&worker))["message"]["value"], "carry on");

    // Uninstall: the panes, then the extension and everything it stored.
    for file in [PANE, BADGE] {
        sb.cli(&["plugin", "remove", &format!("thurbox-auto-continue/{file}")]);
    }
    let (ok, text) = plugin_check(&sb);
    assert!(ok, "{text}");
    let loads = text.lines().find(|l| l.contains("loads")).unwrap_or_default();
    assert!(!loads.contains("auto-continue"), "still loaded:\n{text}");
    assert!(!ui.join("thurbox-auto-continue").exists(), "the working copy is left behind");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let out = sb.command("sh").arg(&script).arg("--uninstall").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(meta(&sb, &worker, "auto-continue.enabled"), None);
    assert_eq!(meta(&sb, &worker, "auto-continue.message"), None);
    assert!(!sb.active("auto-continue"));
}

/// A limit episode as the pane and the badge see it: armed with its window,
/// reset and countdown, then sent by the heartbeat with no TUI running — the
/// message the pane set is the one typed.
#[test]
fn plugin_monitors_a_limit_episode() {
    let (sb, ui) = with_ui(Sandbox::new());
    let worker = sb.session("worker", "claude");
    let idle = sb.session("idle", "claude");
    install_plugin(&sb);
    // Nothing is armed and nothing is on: no badge anywhere.
    live_ok(
        &sb,
        &ui,
        "settle\nnobadge worker ↻\nnobadge idle ↻\nselect worker\nsettle\nexpect no limit recorded yet\n",
    );

    live_ok(
        &sb,
        &ui,
        "settle\nselect worker\nsettle\n\
         key e\nsettle\n\
         key m\ntype keep at it\nkey enter\nsettle\nexpect ✓ saved\n\
         expire\nbadge worker ↻\nnobadge idle ↻\n",
    );

    // A usage limit whose window resets in a minute, the menu cancelled, as
    // the fake Claude draws it; the sandbox's delay is 0.
    sb.script(&worker, &["limit:five_hour:60:cancelled", "ok"]);
    sb.hit_limit(&worker);
    live_ok(
        &sb,
        &ui,
        "settle\nselect worker\nsettle\n\
         expect armed · five_hour resets in\nexpect next send\nexpect attempt 1 of 2\n\
         badge worker ⏸\nnobadge idle ⏸\n",
    );

    // The heartbeat sends it; the TUI is not running.
    sb.wait("the window to reset", Duration::from_secs(90), || {
        sb.tick();
        sb.state(&worker).as_deref() == Some("sent")
    });
    assert_eq!(sb.received(&worker).last().map(String::as_str), Some("keep at it"));
    live_ok(&sb, &ui, "settle\nselect worker\nsettle\nexpect sent\nbadge worker ✓\n");
    assert_eq!(sb.state(&idle), None);
}

/// A session on a shared host, from the laptop: its switch lands in the
/// host's database through this machine's CLI, a host without the extension
/// is reported rather than guessed at, and the Settings switch mirrors the
/// laptop's own config.toml from an event.
#[test]
fn plugin_reaches_shared_hosts_and_mirrors_the_settings_switch() {
    let (laptop, ui) = with_ui(Sandbox::new());
    let host = Sandbox::new();
    let bare = Sandbox::bare();
    laptop.add_host("devbox", &host, Link::Ssh, true);
    // Over WSL: the stand-in `ssh` is one per sandbox, and it reaches devbox.
    laptop.add_host("oldbox", &bare, Link::Wsl, true);
    laptop.session("here", "claude");
    let there = laptop.remote_session("there", "devbox", &host);
    laptop.remote_session("elsewhere", "oldbox", &bare);
    install_plugin(&laptop);

    live_ok(
        &laptop,
        &ui,
        "settle\n\
         select elsewhere\nsettle\nexpect not installed on wsl:oldbox\nreject e on/off\n\
         select there\nsettle\nexpect ssh:devbox\nkey e\nsettle\nexpect ✓ saved there on\n",
    );
    assert_eq!(meta(&host, &there, "auto-continue.enabled").as_deref(), Some("on"));
    assert_eq!(meta(&laptop, &there, "auto-continue.enabled"), None, "nothing stored on the laptop");

    // The Settings switch: first sight agrees (both off), then the user turns
    // it on in the Settings panel, and the next event writes config.toml.
    live_ok(
        &laptop,
        &ui,
        "settle\nevent focus.session\nsettle\n\
         setting auto-continue.enabled true\n\
         expect Settings switch: on · config.toml: off\n\
         event focus.pane\nsettle\nexpect ✓ saved the global switch (from Settings)\n",
    );
    assert_eq!(show(&laptop, None)["enabled"]["value"], true);

    // Changed outside the TUI: config.toml wins, and the switch follows.
    laptop.config(&[("enabled", "off")]);
    live_ok(
        &laptop,
        &ui,
        "setting auto-continue.enabled true\nsettle\nevent focus.session\nsettle\n\
         command set auto-continue.enabled\ncommand set flag=false\n",
    );
    assert_eq!(show(&laptop, None)["enabled"]["value"], false, "the pane did not write it back");
}
