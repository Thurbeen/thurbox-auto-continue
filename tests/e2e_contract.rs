//! The CLI contract an interface builds on (docs/CLI-CONTRACT.md): per-session
//! settings with precedence and validation, and the `status --json` shape.

mod support;

use serde_json::Value;
use support::Sandbox;

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn status(sb: &Sandbox, id: &str) -> Value {
    let out = sb.tac(&["status", id, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["schema"], 2);
    v["sessions"][0].clone()
}

/// A session's own message is what gets typed, not the global one.
#[test]
fn a_session_message_override_is_what_gets_typed() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on"), ("message", "go on")]);
    let id = sb.session("worker", "claude");
    let out = sb.tac(&["config", "set", "message", "keep going please", "--session", &id, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["scope"], "session");
    assert_eq!(v["value"], "keep going please");

    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    sb.hit_limit(&id);
    sb.tick_until_fired(&id);
    assert_eq!(sb.received(&id), ["hello", "keep going please"]);

    let s = status(&sb, &id);
    assert_eq!(s["settings"]["message"]["value"], "keep going please");
    assert_eq!(s["settings"]["message"]["source"], "session");
    assert_eq!(s["last_outcome"]["state"], "sent");
    assert!(s["episode"]["sent_at_ms"].is_i64());
    assert!(s["episode"]["next_send_at_ms"].is_null());
}

/// A session's own delay decides when the next send is scheduled.
#[test]
fn a_session_delay_override_sets_the_next_send() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    assert!(sb.tac(&["config", "set", "delay_secs", "3600", "--session", &id]).status.success());
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    let ep = sb.hit_limit(&id);
    let reset_ms = ep["resets_at"].as_i64().unwrap() * 1000;
    assert_eq!(ep["fire_at_ms"].as_i64().unwrap(), reset_ms + 3_600_000);

    let s = status(&sb, &id);
    assert_eq!(s["settings"]["delay_secs"], serde_json::json!({ "value": 3600, "source": "session" }));
    assert_eq!(s["episode"]["state"], "armed");
    assert_eq!(s["episode"]["resets_at_ms"], reset_ms);
    assert_eq!(s["episode"]["next_send_at_ms"], reset_ms + 3_600_000);
    assert!(s["last_outcome"].is_null());
}

/// Every source shows up where it applies, and status carries no transcript.
#[test]
fn status_reports_sources_and_carries_no_transcript() {
    let sb = Sandbox::new();
    sb.config(&[("delay_secs", "60")]);
    let id = sb.session("worker", "claude");
    assert!(sb.tac(&["enable", &id]).status.success());
    sb.script(&id, &["limit:five_hour:1:armed", "ok"]);
    sb.prompt(&id, "a private prompt about the codebase");
    sb.wait("the episode", std::time::Duration::from_secs(30), || sb.state(&id).is_some());

    let out = sb.tac(&["status", "--json"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v = json(&out);
    assert_eq!(v["schema"], 2);
    assert_eq!(v["global"]["enabled"], serde_json::json!({ "value": false, "source": "default" }));
    assert_eq!(v["global"]["delay_secs"], serde_json::json!({ "value": 60, "source": "global" }));
    assert_eq!(v["global"]["message"]["source"], "default");
    let s = v["sessions"].as_array().unwrap().iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(s["enabled"], true);
    assert_eq!(s["settings"]["enabled"], serde_json::json!({ "value": true, "source": "session" }));
    assert_eq!(s["settings"]["delay_secs"]["source"], "global");
    assert_eq!(s["overrides"], serde_json::json!({ "enabled": "on", "message": null, "delay_secs": null }));
    assert_eq!(s["warnings"], serde_json::json!([]));

    assert!(!text.contains("a private prompt"), "status leaks the prompt");
    assert!(!text.contains(".jsonl"), "status leaks the transcript path");
    assert!(!text.contains("You've hit"), "status leaks screen or transcript text");
}

/// Setters refuse what the engine could not safely type or schedule, and
/// `unset` hands a setting back to the global value.
#[test]
fn setters_validate_and_unset_restores_the_global_value() {
    let sb = Sandbox::new();
    let id = sb.session("worker", "claude");
    let shell = sb.session("shell", "shell");
    let refused = |args: &[&str]| {
        let mut a = args.to_vec();
        a.push("--json");
        let out = sb.tac(&a);
        assert_eq!(out.status.code(), Some(1), "{args:?} was accepted");
        let v = json(&out);
        assert_eq!(v["ok"], false, "{args:?}");
        assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{args:?}");
    };
    refused(&["config", "set", "message", "", "--session", &id]);
    refused(&["config", "set", "message", "two\nlines", "--session", &id]);
    refused(&["config", "set", "message", "/clear", "--session", &id]);
    refused(&["config", "set", "message", "!rm -rf .", "--session", &id]);
    refused(&["config", "set", "message", &"x".repeat(501), "--session", &id]);
    refused(&["config", "set", "delay_secs", "soon", "--session", &id]);
    refused(&["config", "set", "delay_secs", "86401", "--session", &id]);
    refused(&["config", "set", "windows", "seven_day", "--session", &id]);
    refused(&["config", "set", "enabled", "on", "--session", &shell]);
    refused(&["config", "set", "enabled", "on", "--session", "no-such-session"]);
    refused(&["config", "set", "message", "/clear"]);
    refused(&["config", "unset", "message"]);
    assert!(status(&sb, &id)["overrides"]["message"].is_null(), "a refused set stored nothing");

    assert!(sb.tac(&["config", "set", "message", "carry on", "--session", &id]).status.success());
    assert_eq!(status(&sb, &id)["settings"]["message"]["source"], "session");
    let out = sb.tac(&["config", "unset", "message", "--session", &id, "--json"]);
    assert!(out.status.success());
    assert_eq!(json(&out)["value"], Value::Null);
    let s = status(&sb, &id);
    assert_eq!(s["settings"]["message"], serde_json::json!({ "value": "continue", "source": "default" }));

    // A hand-written bad override is ignored, and said so.
    sb.cli(&["session", "meta", "set", &id, "auto-continue.delay_secs", "--", "forever"]);
    let s = status(&sb, &id);
    assert_eq!(s["settings"]["delay_secs"]["source"], "global");
    assert_eq!(s["warnings"].as_array().unwrap().len(), 1, "{s}");

    // `config show --session` is the same resolution, on its own.
    let out = sb.tac(&["config", "show", "--session", &id, "--json"]);
    assert!(out.status.success());
    assert_eq!(json(&out)["message"], serde_json::json!({ "value": "continue", "source": "default" }));
}

/// A message changed after the limit was recorded is the one typed.
#[test]
fn a_message_changed_while_armed_is_the_one_sent() {
    let sb = Sandbox::new();
    sb.config(&[("enabled", "on")]);
    let id = sb.session("worker", "claude");
    sb.script(&id, &["limit:five_hour:1:cancelled", "ok"]);
    sb.hit_limit(&id);
    assert!(sb.tac(&["config", "set", "message", "resume the task", "--session", &id]).status.success());
    sb.tick_until_fired(&id);
    assert_eq!(sb.received(&id), ["hello", "resume the task"]);
}
