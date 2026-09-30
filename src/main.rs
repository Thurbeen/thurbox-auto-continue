use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use thurbox_auto_continue::config::{self, Config, SESSION_SETTINGS};
use thurbox_auto_continue::episode::{AUTOMATION_PREFIX, EPISODE_KEY, Episode, State};
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(home) = cli.home.clone().or_else(platform::default_home) else {
        eprintln!("error: no home directory; pass --home or set {}", platform::HOME_ENV);
        return ExitCode::from(2);
    };
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
        Cmd::Status { session, json } => status(&home, session.as_deref(), json),
        Cmd::Enable { session } => set(&home, "enabled", Some("on"), Some(&session), false),
        Cmd::Disable { session } => set(&home, "enabled", Some("off"), Some(&session), false),
        Cmd::Clear { session } => set(&home, "enabled", None, Some(&session), false),
        Cmd::Config { action: ConfigCmd::Show { session, json } } => show(&home, session.as_deref(), json),
        Cmd::Config { action: ConfigCmd::Set { key, value, session, json } } => {
            return report(set(&home, &key, Some(&value), session.as_deref(), json), json);
        }
        Cmd::Config { action: ConfigCmd::Unset { key, session, json } } => {
            let r = match session.as_deref() {
                Some(s) => set(&home, &key, None, Some(s), json),
                None => Err(format!("`unset` needs --session: a global `{key}` is changed with `config set`")),
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

/// Exit code and output for a setter: with `--json`, one object on stdout
/// whether it worked or not.
fn report(result: Result<(), String>, json: bool) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json {
                println!("{}", json!({ "ok": false, "error": e }));
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::from(1)
        }
    }
}

/// Set (`value` Some) or clear (`None`) one setting, globally or for a session.
fn set(
    home: &std::path::Path,
    key: &str,
    value: Option<&str>,
    session: Option<&str>,
    json: bool,
) -> Result<(), String> {
    let Some(reference) = session else {
        let value = value.ok_or("a global setting is changed with `config set`")?;
        let mut cfg = Config::load(home)?;
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
        return Err(format!("`{key}` is global only; per session: enabled, message, delay_secs"));
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
    if key == "enabled" && stored.as_deref() == Some("on") {
        if !s.is_claude() {
            return Err(format!("`{}` runs `{}`; auto-continue only acts on Claude sessions", s.name, s.agent));
        }
        if !s.is_local() {
            return Err(format!(
                "`{}` lives on `{}`; install the extension on that host and set it there",
                s.name, s.backend_type
            ));
        }
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

fn show(home: &std::path::Path, session: Option<&str>, json: bool) -> Result<(), String> {
    let cfg = Config::load(home)?;
    let effective = match session {
        Some(r) => {
            let tb = Thurbox::default();
            let s = find(&tb, r)?;
            cfg.for_session(&tb.meta_list(&s.id)?)
        }
        None => cfg.global(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&effective).unwrap_or_default());
    } else {
        let src = |s: config::Source| {
            serde_json::to_value(s).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
        };
        println!("enabled    = {:<8} ({})", effective.enabled.value, src(effective.enabled.source));
        println!("message    = {:?} ({})", effective.message.value, src(effective.message.source));
        println!("delay_secs = {:<8} ({})", effective.delay_secs.value, src(effective.delay_secs.source));
        for w in &effective.warnings {
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

/// The shape `status --json` prints (docs/CLI-CONTRACT.md). `schema` is
/// bumped on any breaking change, because the interface plugin reads it.
fn status(home: &std::path::Path, only: Option<&str>, as_json: bool) -> Result<(), String> {
    let cfg = Config::load(home)?;
    let tb = Thurbox::default();
    let mut sessions = tb.sessions()?;
    if let Some(r) = only {
        let s = find(&tb, r)?;
        sessions.retain(|x| x.id == s.id);
    }
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let meta = tb.meta_list(&s.id).unwrap_or_default();
            let settings = cfg.for_session(&meta);
            let overrides: serde_json::Map<String, Value> = SESSION_SETTINGS
                .iter()
                .map(|(k, mk)| (k.to_string(), meta.get(*mk).cloned().unwrap_or(Value::Null)))
                .collect();
            let episode = meta.get(EPISODE_KEY).and_then(Value::as_str).and_then(Episode::parse);
            let why = ineligible(s);
            json!({
                "id": s.id,
                "name": s.name,
                "agent": s.agent,
                "eligible": why.is_none(),
                "ineligible_reason": why,
                "enabled": why.is_none() && settings.enabled.value,
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
            })
        })
        .collect();
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
        "sessions": rows,
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(());
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
        println!("{:<24} {:<18} episode: {ep}", r["name"].as_str().unwrap_or(""), state);
        for w in r["warnings"].as_array().into_iter().flatten() {
            println!("  warning: {}", w.as_str().unwrap_or(""));
        }
    }
    Ok(())
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
