use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use thurbox_auto_continue::config::{self, Config, GLOBAL_KEYS, SESSION_SETTINGS};
use thurbox_auto_continue::episode::{AUTOMATION_PREFIX, EPISODE_KEY, Episode, State};
use thurbox_auto_continue::remote::{self, Unsupported};
use thurbox_auto_continue::thurbox::{Session, Thurbox};
use thurbox_auto_continue::{engine, platform};

/// Continue a Claude session in Thurbox once its usage limit resets — headless,
/// deterministic, no model involved.
#[derive(Parser)]
#[command(
    version,
    after_help = "Examples:\n  \
        thurbox-auto-continue status                  every session, its toggle and its last episode\n  \
        thurbox-auto-continue enable <session>        turn it on for one Claude session\n  \
        thurbox-auto-continue config set enabled on   turn it on for every Claude session\n  \
        thurbox-auto-continue config set message 'keep going' --session <session>\n  \
        thurbox-auto-continue status --json           the stable shape a plugin reads (docs/CLI-CONTRACT.md)\n\n\
        Kill switch: `thurbox-cli extension deactivate auto-continue` stops every pending send."
)]
struct Cli {
    /// The extension home (config.toml, log, locks). Default: $THURBOX_AUTO_CONTINUE_HOME,
    /// else ~/.config/thurbox/auto-continue.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Set by another machine asking this one about a session that lives here
    /// (see src/remote.rs): answer for this machine only, never ask further.
    #[arg(long, global = true, hide = true)]
    delegated: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Claude's StopFailure hook: read the hook JSON on stdin, prove a quota
    /// episode and schedule its send. Prints nothing, always exits 0.
    Record,
    /// Send the message for a session's due episode, if every gate passes.
    Fire { session: String },
    /// The slow fallback: pick up episodes the hook missed and overdue sends.
    Sweep,
    /// Show the global switch, each session's toggle and its last episode.
    Status {
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Turn auto-continue on for one Claude session (beats the global default).
    Enable { session: String },
    /// Turn auto-continue off for one session (beats the global default).
    Disable { session: String },
    /// Forget a session's own toggle, so the global default applies again.
    Clear { session: String },
    /// Read or change config.toml.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Remove every trace from Thurbox: toggles, episodes and pending sends.
    Forget {
        #[arg(long, required = true)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective settings, globally or for one session.
    Show {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Set one key. Globally: enabled, delay_secs, message, windows, max_attempts,
    /// on_menu, confirm_secs. With --session: enabled, message, delay_secs.
    Set {
        key: String,
        value: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Remove a session's own value, so the global one applies again.
    Unset {
        key: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// Why a setter did not run: the command was wrong (exit 2), or it was
/// refused (exit 1).
enum Fail {
    Usage(String),
    Refused(String),
}

impl From<String> for Fail {
    fn from(e: String) -> Self {
        Fail::Refused(e)
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if e.use_stderr() && std::env::args().any(|a| a == "--json") => {
            // A plugin reads stdout: it gets one object even when the command
            // line itself was wrong.
            let first = e.to_string().lines().next().unwrap_or_default().trim_start_matches("error: ").to_string();
            println!("{}", json!({ "ok": false, "error": first }));
            return ExitCode::from(2);
        }
        Err(e) => e.exit(),
    };
    let Some(home) = cli.home.clone().or_else(platform::default_home) else {
        eprintln!("error: no home directory; pass --home or set {}", platform::HOME_ENV);
        return ExitCode::from(2);
    };
    let d = cli.delegated;
    let result = match cli.command {
        Cmd::Record => {
            // Bounded: Claude closes the pipe when it has written the payload.
            let mut input = String::new();
            let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
            engine::record(&home, &input);
            return ExitCode::SUCCESS;
        }
        Cmd::Fire { session } => {
            engine::fire(&home, &session);
            return ExitCode::SUCCESS;
        }
        Cmd::Sweep => {
            engine::sweep(&home);
            return ExitCode::SUCCESS;
        }
        Cmd::Status { session, json } => status(&home, session.as_deref(), json, d),
        Cmd::Enable { session } => return report(set(&home, "enabled", Some("on"), Some(&session), false, d), false),
        Cmd::Disable { session } => return report(set(&home, "enabled", Some("off"), Some(&session), false, d), false),
        Cmd::Clear { session } => return report(set(&home, "enabled", None, Some(&session), false, d), false),
        Cmd::Config { action: ConfigCmd::Show { session, json } } => show(&home, session.as_deref(), json, d),
        Cmd::Config { action: ConfigCmd::Set { key, value, session, json } } => {
            return report(set(&home, &key, Some(&value), session.as_deref(), json, d), json);
        }
        Cmd::Config { action: ConfigCmd::Unset { key, session, json } } => {
            let r = match session.as_deref() {
                Some(s) => set(&home, &key, None, Some(s), json, d),
                None => {
                    Err(Fail::Usage(format!("`unset` needs --session: a global `{key}` is changed with `config set`")))
                }
            };
            return report(r, json);
        }
        Cmd::Forget { .. } => forget_all(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn find(tb: &Thurbox, reference: &str) -> Result<Session, String> {
    tb.session(reference)?.ok_or_else(|| format!("no session `{reference}`; `thurbox-cli session list` shows the ids"))
}

/// Ask the host a remote session lives on, or say why it cannot be asked. A
/// delegated run never asks further: its remote rows are another host's.
fn on_host(
    tb: &Thurbox,
    s: &Session,
    delegated: bool,
    args: &[&str],
    positional: &[&str],
) -> Result<remote::Answer, Unsupported> {
    if delegated {
        return Err(Unsupported::Transitive);
    }
    remote::Hosts::read(tb).map_err(|_| Unsupported::Unreachable)?.check(s)?;
    remote::ask(tb, s, args, positional)
}

/// The error a host's answer carries, as this machine's own refusal.
fn host_refusal(s: &Session, a: &remote::Answer) -> Fail {
    let e = format!("{}: {}", s.backend_type, a.body["error"].as_str().unwrap_or("the host refused"));
    if a.code == 2 { Fail::Usage(e) } else { Fail::Refused(e) }
}

/// Exit code and output for a setter: with `--json`, one object on stdout
/// whether it worked or not.
fn report(result: Result<(), Fail>, json: bool) -> ExitCode {
    let (code, e) = match result {
        Ok(()) => return ExitCode::SUCCESS,
        Err(Fail::Usage(e)) => (2, e),
        Err(Fail::Refused(e)) => (1, e),
    };
    if json {
        println!("{}", json!({ "ok": false, "error": e }));
    } else {
        eprintln!("error: {e}");
    }
    ExitCode::from(code)
}

/// Set (`value` Some) or clear (`None`) one setting, globally or for a session.
fn set(
    home: &std::path::Path,
    key: &str,
    value: Option<&str>,
    session: Option<&str>,
    json: bool,
    delegated: bool,
) -> Result<(), Fail> {
    if !GLOBAL_KEYS.contains(&key) {
        return Err(Fail::Usage(format!("unknown key `{key}`; one of: {}", GLOBAL_KEYS.join(", "))));
    }
    let Some(reference) = session else {
        let value = value.ok_or_else(|| Fail::Usage("a global setting is changed with `config set`".into()))?;
        // Lenient: the value being set may be what fixes a broken file.
        let (mut cfg, _) = Config::load_lenient(home)?;
        cfg.set(key, value)?;
        cfg.save(home)?;
        let stored = toml::Table::try_from(&cfg).ok().and_then(|t| t.get(key).cloned());
        if json {
            println!("{}", json!({ "ok": true, "scope": "global", "key": key, "value": stored }));
        } else {
            println!("{key} set globally in {}", home.join("config.toml").display());
        }
        return Ok(());
    };
    let Some(&(_, meta_key)) = SESSION_SETTINGS.iter().find(|(k, _)| *k == key) else {
        return Err(Fail::Usage(format!("`{key}` is global only; per session: enabled, message, delay_secs")));
    };
    let stored = match value {
        Some(v) => Some(match key {
            "enabled" => if config::validate_switch(v)? { "on" } else { "off" }.to_string(),
            "message" => config::validate_message(v)?,
            _ => config::validate_delay(v)?.to_string(),
        }),
        None => None,
    };
    let tb = Thurbox::default();
    let s = find(&tb, reference)?;
    if key == "enabled" && stored.as_deref() == Some("on") && !s.is_claude() {
        return Err(format!("`{}` runs `{}`; auto-continue only acts on Claude sessions", s.name, s.agent).into());
    }
    // A shared host's own install decides, in its own database.
    if !s.is_local() {
        return set_on_host(&tb, &s, key, stored.as_deref(), json, delegated);
    }
    match &stored {
        Some(v) => tb.meta_set(&s.id, meta_key, v)?,
        None => tb.meta_unset(&s.id, meta_key)?,
    }
    if json {
        println!("{}", json!({ "ok": true, "scope": "session", "session": s.id, "key": key, "value": stored }));
    } else {
        match &stored {
            Some(v) => println!("{key} = {v} for {} ({})", s.name, s.id),
            None => println!("{} follows the global {key} again", s.name),
        }
    }
    Ok(())
}

/// A setter for a session on a shared host: the host validates and stores it,
/// in its own database, where its own install reads it.
fn set_on_host(
    tb: &Thurbox,
    s: &Session,
    key: &str,
    stored: Option<&str>,
    json: bool,
    delegated: bool,
) -> Result<(), Fail> {
    let (verb, positional) = match stored {
        Some(v) => ("set", vec![key, v]),
        None => ("unset", vec![key]),
    };
    let a = on_host(tb, s, delegated, &["config", verb, "--session", &s.id], &positional)
        .map_err(|u| Fail::Refused(u.message(s)))?;
    if a.code != 0 || a.body["ok"] != true {
        return Err(host_refusal(s, &a));
    }
    let value = a.body["value"].as_str();
    if json {
        println!(
            "{}",
            json!({ "ok": true, "scope": "session", "session": s.id, "key": key, "value": value, "host": s.backend_type })
        );
    } else {
        match value {
            Some(v) => println!("{key} = {v} for {} ({}) on {}", s.name, s.id, s.backend_type),
            None => println!("{} follows {}'s global {key} again", s.name, s.backend_type),
        }
    }
    Ok(())
}

fn show(home: &std::path::Path, session: Option<&str>, json: bool, delegated: bool) -> Result<(), String> {
    let (cfg, mut warnings) = Config::load_lenient(home)?;
    let effective = match session {
        Some(r) => {
            let tb = Thurbox::default();
            let s = find(&tb, r)?;
            if s.is_local() {
                let e = cfg.for_session(&tb.meta_list(&s.id)?);
                warnings.extend(e.warnings.iter().cloned());
                serde_json::to_value(&e).unwrap_or_default()
            } else {
                // The host's global config and the host's overrides apply.
                warnings.clear();
                let a = on_host(&tb, &s, delegated, &["config", "show", "--session", &s.id], &[])
                    .map_err(|u| u.message(&s))?;
                if a.code != 0 {
                    return Err(format!(
                        "{}: {}",
                        s.backend_type,
                        a.body["error"].as_str().unwrap_or("the host refused")
                    ));
                }
                remote::sanitize_row(&json!({ "settings": a.body }))["settings"].clone()
            }
        }
        None => serde_json::to_value(cfg.global()).unwrap_or_default(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&effective).unwrap_or_default());
    } else {
        let line = |k: &str| {
            let r = &effective[k];
            println!("{k:<10} = {:<8} ({})", r["value"], r["source"].as_str().unwrap_or_default());
        };
        ["enabled", "message", "delay_secs"].into_iter().for_each(line);
        for w in &warnings {
            println!("warning: {w}");
        }
    }
    Ok(())
}

fn ineligible(s: &Session) -> Option<&'static str> {
    if !s.is_claude() {
        Some("not-claude")
    } else if !s.is_local() {
        Some("remote")
    } else if s.stopped {
        Some("stopped")
    } else {
        None
    }
}

/// One session's row of `status --json`, from this machine's own records.
fn local_row(cfg: &Config, tb: &Thurbox, s: &Session) -> Value {
    // Unreadable meta is shown as off, as the engine treats it.
    let (meta, meta_error) = match tb.meta_list(&s.id) {
        Ok(m) => (m, None),
        Err(e) => (Default::default(), Some(format!("session settings unreadable: {e}"))),
    };
    let mut settings = cfg.for_session(&meta);
    settings.warnings.extend(meta_error.clone());
    let overrides: serde_json::Map<String, Value> =
        SESSION_SETTINGS.iter().map(|(k, mk)| (k.to_string(), meta.get(*mk).cloned().unwrap_or(Value::Null))).collect();
    let episode = meta.get(EPISODE_KEY).and_then(Value::as_str).and_then(Episode::parse);
    let why = ineligible(s);
    json!({
        "id": s.id,
        "name": s.name,
        "agent": s.agent,
        "eligible": why.is_none(),
        "ineligible_reason": why,
        "enabled": why.is_none() && meta_error.is_none() && settings.enabled.value,
        "settings": settings,
        "overrides": overrides,
        "episode": episode.as_ref().map(|e| json!({
            "state": e.state,
            "label": e.label(),
            "reason": e.reason,
            "window": e.window,
            "resets_at_ms": e.resets_at * 1000,
            "next_send_at_ms": (e.state == State::Armed).then_some(e.fire_at_ms),
            "attempt": e.attempt,
            "max_attempts": cfg.max_attempts,
            "sent_at_ms": e.sent_at_ms,
            "updated_at_ms": e.updated_at_ms,
        })),
        "last_outcome": episode.as_ref().filter(|e| e.state.is_final()).map(|e| json!({
            "state": e.state,
            "reason": e.reason,
            "label": e.label(),
            "at_ms": e.updated_at_ms,
        })),
        "warnings": settings.warnings,
        "host": null,
    })
}

/// The shape `status --json` prints (docs/CLI-CONTRACT.md). `schema` is
/// bumped on any breaking change, because the interface plugin reads it.
///
/// A session on a shared host is the host's answer: one `status` run there
/// per host, through the first of its sessions listed here.
fn status(home: &std::path::Path, only: Option<&str>, as_json: bool, delegated: bool) -> Result<(), String> {
    let (cfg, global_warnings) = Config::load_lenient(home)?;
    let tb = Thurbox::default();
    let mut sessions = tb.sessions()?;
    if let Some(r) = only {
        let s = find(&tb, r)?;
        sessions.retain(|x| x.id == s.id);
    }
    let mut hosts: std::collections::HashMap<String, Result<Value, Unsupported>> = Default::default();
    let mut rows = Vec::new();
    for s in &sessions {
        if s.is_local() {
            rows.push(local_row(&cfg, &tb, s));
            continue;
        }
        let answer = hosts.entry(s.backend_type.clone()).or_insert_with(|| {
            let positional: &[&str] = if only.is_some() { &[&s.id] } else { &[] };
            let a = on_host(&tb, s, delegated, &["status"], positional)?;
            match (a.code, a.body["schema"].as_i64()) {
                (0, Some(2)) => Ok(a.body),
                (0, _) => Err(Unsupported::Outdated),
                _ => Err(Unsupported::Unreachable),
            }
        });
        let theirs = answer
            .as_ref()
            .ok()
            .and_then(|b| b["sessions"].as_array()?.iter().find(|r| r["id"] == s.id.as_str()).cloned());
        let row = match (&*answer, theirs) {
            (Ok(body), Some(r)) => {
                let mut row = remote::sanitize_row(&r);
                let reason = (r["ineligible_reason"] == "remote").then(|| Unsupported::Transitive.reason());
                row["host"] = json!({
                    "backend": s.backend_type,
                    "reason": reason,
                    "extension_active": body["extension_active"],
                });
                row
            }
            (Ok(_), None) => with_host(local_row(&cfg, &tb, s), Unsupported::UnknownToHost.host_json(s)),
            (Err(u), _) => with_host(local_row(&cfg, &tb, s), u.host_json(s)),
        };
        rows.push(row);
    }
    let global = cfg.global();
    let out = json!({
        "schema": 2,
        "extension_active": tb.extension_active().ok(),
        "global": {
            "enabled": global.enabled,
            "message": global.message,
            "delay_secs": global.delay_secs,
            "windows": cfg.windows,
            "max_attempts": cfg.max_attempts,
        },
        "warnings": global_warnings,
        "sessions": rows,
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(());
    }
    for w in &global_warnings {
        println!("warning: {w}");
    }
    println!(
        "global: {}  (delay {}s, message {:?}, windows {})",
        if cfg.enabled { "on" } else { "off" },
        cfg.delay_secs,
        cfg.message,
        cfg.windows.join(",")
    );
    match out["extension_active"].as_bool() {
        Some(false) => {
            println!("extension: INACTIVE — nothing is sent (`thurbox-cli extension activate auto-continue`)")
        }
        None => println!("extension: unknown (thurbox-cli did not answer)"),
        Some(true) => {}
    }
    for r in &rows {
        let state = if !r["eligible"].as_bool().unwrap_or(false) {
            format!("n/a ({})", r["ineligible_reason"].as_str().unwrap_or("?"))
        } else if r["enabled"].as_bool().unwrap_or(false) {
            "on".into()
        } else {
            "off".into()
        };
        let ep = r["episode"]["label"].as_str().unwrap_or("-");
        let host = match (r["host"]["backend"].as_str(), r["host"]["reason"].as_str()) {
            (Some(b), Some(why)) => format!("  on {b}: unsupported ({why})"),
            (Some(b), None) => format!("  on {b}"),
            _ => String::new(),
        };
        println!("{:<24} {:<18} episode: {ep}{host}", r["name"].as_str().unwrap_or(""), state);
        for w in r["warnings"].as_array().into_iter().flatten() {
            println!("  warning: {}", w.as_str().unwrap_or(""));
        }
    }
    Ok(())
}

fn with_host(mut row: Value, host: Value) -> Value {
    row["host"] = host;
    row
}

fn forget_all() -> Result<(), String> {
    let tb = Thurbox::default();
    for a in tb.automations()?.into_iter().filter(|a| a.name.starts_with(AUTOMATION_PREFIX)) {
        tb.remove_automation(a.id)?;
    }
    for s in tb.sessions()? {
        for (_, key) in SESSION_SETTINGS {
            tb.meta_unset(&s.id, key)?;
        }
        tb.meta_unset(&s.id, EPISODE_KEY)?;
    }
    println!("forgot every auto-continue setting, episode and pending send");
    Ok(())
}
