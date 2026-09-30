//! Shared SSH and WSL hosts, host-local: the extension is installed on the
//! host, and the host's own Thurbox database and heartbeat own its sessions.
//! The laptop is a second, real sandbox that reaches the host through a
//! stand-in `ssh` or `wsl.exe` (see `Sandbox::add_host`); only the network is
//! faked. The laptop's TUI is never started.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use support::{Link, Sandbox};

const HOST: &str = "devbox";

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    })
}

/// A laptop that reaches `host` as `devbox`, both with the extension installed.
fn pair(host: Sandbox, link: Link, shared: bool) -> (Sandbox, Sandbox) {
    let laptop = Sandbox::new();
    laptop.add_host(HOST, &host, link, shared);
    (laptop, host)
}

fn backend(link: Link) -> String {
    format!("{}:{HOST}", if link == Link::Ssh { "ssh" } else { "wsl" })
}

fn meta(sb: &Sandbox, id: &str, key: &str) -> Option<String> {
    let v = sb.cli(&["session", "meta", "get", id, key]);
    v["value"].as_str().map(String::from)
}

fn status(sb: &Sandbox, id: &str) -> (Value, String) {
    let out = sb.tac(&["status", id, "--json"]);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["schema"], 2);
    (v["sessions"][0].clone(), String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Everything the laptop's own install would ever run for a session.
fn laptop_does_everything(laptop: &Sandbox, id: &str) {
    laptop.tick();
    assert!(laptop.tac(&["sweep"]).status.success());
    // As if a one-shot of its own were due: it must still not act.
    assert!(laptop.tac(&["fire", id]).status.success());
}

/// A15 / A15w: a limit on a shared host gets exactly one message, typed by
/// the host's own install, while the laptop — installed too, on globally, its
/// TUI never started — runs its heartbeat, its sweep and a stray `fire`.
fn one_send_from_the_host(link: Link) {
    let (laptop, host) = pair(Sandbox::new(), link, true);
    host.config(&[("enabled", "on")]);
    laptop.config(&[("enabled", "on")]);
    let id = laptop.remote_session("worker", HOST, &host);
    assert_eq!(laptop.session_row(&id)["backend_type"], backend(link));
    assert_eq!(host.session_row(&id)["backend_type"], "local-tmux");

    // Set from the laptop, stored on the host: what the host types.
    let out = laptop.tac(&["config", "set", "message", "resume on the host", "--session", &id, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(meta(&host, &id, "auto-continue.message").as_deref(), Some("resume on the host"));

    host.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    laptop.prompt(&id, "hello");
    host.wait("the host's hook to arm the episode", Duration::from_secs(30), || {
        host.state(&id).as_deref() == Some("armed")
    });
    // The host's hook reported the session idle, which is what keeps fleet's
    // refuel (it restarts a stale `working`) away; the laptop's mirror agrees.
    laptop.cli(&["session", "sync"]);
    assert_eq!(laptop.session_row(&id)["hook_state"], "idle");

    for _ in 0..3 {
        laptop_does_everything(&laptop, &id);
    }
    host.tick_until_fired(&id);
    assert_eq!(host.received(&id), ["hello", "resume on the host"]);

    // Both sides keep running; nothing more arrives.
    for _ in 0..3 {
        laptop_does_everything(&laptop, &id);
        host.tick();
        assert!(host.tac(&["sweep"]).status.success());
    }
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(host.received(&id), ["hello", "resume on the host"]);
    assert_eq!(host.state(&id).as_deref(), Some("sent"));
    assert_eq!(meta(&laptop, &id, "auto-continue.episode"), None, "the laptop never claims an episode");
    assert!(laptop.our_automations().is_empty(), "the laptop never schedules a send");

    // The laptop's status is the host's own answer.
    let (s, _) = status(&laptop, &id);
    assert_eq!(s["host"]["backend"], backend(link));
    assert_eq!(s["host"]["reason"], Value::Null, "{s}");
    assert_eq!(s["eligible"], true, "{s}");
    assert_eq!(s["enabled"], true, "{s}");
    assert_eq!(s["settings"]["message"], json!({ "value": "resume on the host", "source": "session" }));
    assert_eq!(s["settings"]["enabled"], json!({ "value": true, "source": "global" }));
    assert_eq!(s["last_outcome"]["state"], "sent");

    // All of it went over the link to the host's own install.
    let (over, log) = (if link == Link::Ssh { "ssh " } else { "wsl " }, laptop.link_log());
    assert!(log.lines().any(|l| l.starts_with(over)) && log.contains("--delegated"), "{log}");
}

#[test]
fn a_shared_ssh_host_sends_once_while_the_laptop_watches() {
    one_send_from_the_host(Link::Ssh);
}

#[test]
fn a_shared_wsl_distro_sends_once_while_the_laptop_watches() {
    one_send_from_the_host(Link::Wsl);
}

/// A remote session's toggle and its prompt and delay overrides, set from the
/// laptop, land in the host's database, and `status` reads them back from
/// there with the episode the host armed — and no transcript.
#[test]
fn a_remote_toggle_and_overrides_land_in_the_hosts_database() {
    let (laptop, host) = pair(Sandbox::new(), Link::Ssh, true);
    let id = laptop.remote_session("worker", HOST, &host);

    let out = laptop.tac(&["enable", &id]);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for (key, value) in [("message", "keep going there"), ("delay_secs", "3600")] {
        let out = laptop.tac(&["config", "set", key, value, "--session", &id, "--json"]);
        assert!(out.status.success(), "{key}: {}", String::from_utf8_lossy(&out.stdout));
        let v = json(&out);
        assert_eq!(v["ok"], true);
        assert_eq!(v["scope"], "session");
        assert_eq!(v["session"], id.as_str());
        assert_eq!(v["key"], key);
        assert_eq!(v["value"], value);
        assert_eq!(v["host"], "ssh:devbox");
    }
    assert_eq!(meta(&host, &id, "auto-continue.enabled").as_deref(), Some("on"));
    assert_eq!(meta(&host, &id, "auto-continue.message").as_deref(), Some("keep going there"));
    assert_eq!(meta(&host, &id, "auto-continue.delay_secs").as_deref(), Some("3600"));
    for key in ["auto-continue.enabled", "auto-continue.message", "auto-continue.delay_secs"] {
        assert_eq!(meta(&laptop, &id, key), None, "{key} was written on the laptop, where nothing reads it");
    }

    let (s, _) = status(&laptop, &id);
    assert_eq!(s["id"], id.as_str());
    assert_eq!(s["eligible"], true, "{s}");
    assert_eq!(s["enabled"], true);
    assert_eq!(s["settings"]["enabled"], json!({ "value": true, "source": "session" }));
    assert_eq!(s["settings"]["message"], json!({ "value": "keep going there", "source": "session" }));
    assert_eq!(s["settings"]["delay_secs"], json!({ "value": 3600, "source": "session" }));
    assert_eq!(s["overrides"], json!({ "enabled": "on", "message": "keep going there", "delay_secs": "3600" }));
    assert_eq!(s["host"]["backend"], "ssh:devbox");
    assert_eq!(s["host"]["extension_active"], true);

    host.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    laptop.prompt(&id, "a private prompt about the codebase");
    host.wait("the host's hook to arm the episode", Duration::from_secs(30), || {
        host.state(&id).as_deref() == Some("armed")
    });
    let (s, text) = status(&laptop, &id);
    assert_eq!(s["episode"]["state"], "armed", "{s}");
    let reset_ms = s["episode"]["resets_at_ms"].as_i64().unwrap();
    assert_eq!(s["episode"]["next_send_at_ms"], reset_ms + 3_600_000);
    assert!(s["last_outcome"].is_null());
    assert!(!text.contains("a private prompt"), "status leaks the prompt");
    assert!(!text.contains(".jsonl"), "status leaks the transcript path");
    assert!(!text.contains("You've hit"), "status leaks screen or transcript text");
    assert!(!text.contains(host.home.to_str().unwrap()), "status leaks the host's paths");

    // The whole list reaches the host too, one call for the host.
    let out = laptop.tac(&["status", "--json"]);
    let all = json(&out);
    let row = all["sessions"].as_array().unwrap().iter().find(|r| r["id"] == id.as_str()).cloned().unwrap();
    assert_eq!(row["episode"]["state"], "armed", "{row}");

    // `config show` and `unset` go there too; `disable` beats the global value.
    let out = laptop.tac(&["config", "show", "--session", &id, "--json"]);
    assert!(out.status.success());
    assert_eq!(json(&out)["message"], json!({ "value": "keep going there", "source": "session" }));
    assert!(laptop.tac(&["config", "unset", "message", "--session", &id]).status.success());
    assert_eq!(meta(&host, &id, "auto-continue.message"), None);
    assert!(laptop.tac(&["disable", &id]).status.success());
    assert_eq!(meta(&host, &id, "auto-continue.enabled").as_deref(), Some("off"));
    let (s, _) = status(&laptop, &id);
    assert_eq!(s["enabled"], false);
    assert_eq!(s["settings"]["enabled"], json!({ "value": false, "source": "session" }));
}

/// A16: a host with `share_sessions = false` is driven from the laptop, so no
/// host database owns the session. It is unsupported, says so, and nothing is
/// ever typed into it — though both machines are installed and on.
#[test]
fn a_host_that_does_not_share_its_sessions_is_never_sent() {
    let (laptop, host) = pair(Sandbox::new(), Link::Ssh, false);
    host.config(&[("enabled", "on")]);
    laptop.config(&[("enabled", "on")]);
    let id = laptop.remote_session("worker", HOST, &host);
    assert!(
        host.cli(&["session", "list"]).as_array().unwrap().iter().all(|s| s["id"] != id.as_str()),
        "the host's database does not know a session it does not share"
    );

    let out = laptop.tac(&["config", "set", "enabled", "on", "--session", &id, "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stdout));
    let v = json(&out);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("share_sessions"), "{v}");

    let (s, _) = status(&laptop, &id);
    assert_eq!(s["eligible"], false);
    assert_eq!(s["ineligible_reason"], "remote");
    assert_eq!(s["enabled"], false);
    assert_eq!(s["host"], json!({ "backend": "ssh:devbox", "reason": "not-shared", "extension_active": null }));

    for ctl in [&host, &laptop] {
        ctl.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    }
    // Typed into the pane itself: the laptop's Thurbox has no CLI there to
    // delegate a `send` to.
    let pane = laptop.session_row(&id)["backend_id"].as_str().unwrap().to_string();
    laptop.legacy_pane(&pane, &["-l", "hello"]);
    laptop.legacy_pane(&pane, &["Enter"]);
    let received = || [host.received(&id), laptop.received(&id)].concat();
    laptop.wait("the prompt to arrive", Duration::from_secs(30), || received() == ["hello"]);
    std::thread::sleep(Duration::from_secs(2));
    for _ in 0..3 {
        laptop_does_everything(&laptop, &id);
        host.tick();
        assert!(host.tac(&["sweep"]).status.success());
    }
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(received(), ["hello"]);
    assert_eq!(laptop.episode(&id), None);
    assert!(laptop.our_automations().is_empty() && host.our_automations().is_empty());
}

/// A shared host that never got the extension is reported, not guessed at:
/// the laptop cannot turn it on there, and says why.
#[test]
fn a_shared_host_without_the_extension_is_reported_not_installed() {
    let (laptop, host) = pair(Sandbox::bare(), Link::Wsl, true);
    let id = laptop.remote_session("worker", HOST, &host);

    let out = laptop.tac(&["config", "set", "enabled", "on", "--session", &id, "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stdout));
    let v = json(&out);
    assert!(v["error"].as_str().unwrap().contains("install"), "{v}");
    assert_eq!(meta(&host, &id, "auto-continue.enabled"), None);

    let (s, _) = status(&laptop, &id);
    assert_eq!(s["enabled"], false);
    assert_eq!(s["host"]["reason"], "not-installed", "{s}");
}
