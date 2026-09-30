use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use thurbox_auto_continue::config::{self, Config, ENABLED_KEY};
use thurbox_auto_continue::episode::{AUTOMATION_PREFIX, EPISODE_KEY, Episode};
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
        thurbox-auto-continue status --json           the stable shape a plugin reads\n\n\
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
    /// Print the effective config.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Set one key: enabled, delay_secs, message, windows, max_attempts, on_menu, confirm_secs.
    Set { key: String, value: String },
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
        Cmd::Enable { session } => toggle(&session, Some(true)),
        Cmd::Disable { session } => toggle(&session, Some(false)),
        Cmd::Clear { session } => toggle(&session, None),
        Cmd::Config { action: ConfigCmd::Show { json } } => Config::load(&home).map(|c| {
            if json {
                println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            } else {
                print!("{}", toml::to_string(&c).unwrap_or_default());
            }
        }),
        Cmd::Config { action: ConfigCmd::Set { key, value } } => Config::load(&home).and_then(|mut c| {
            c.set(&key, &value)?;
            c.save(&home)?;
            println!("{key} set; config at {}", home.join("config.toml").display());
            Ok(())
        }),
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

fn toggle(reference: &str, on: Option<bool>) -> Result<(), String> {
    let tb = Thurbox::default();
    let s = find(&tb, reference)?;
    match on {
        Some(true) => {
            if !s.is_claude() {
                return Err(format!("`{}` runs `{}`; auto-continue only acts on Claude sessions", s.name, s.agent));
            }
            if !s.is_local() {
                return Err(format!(
                    "`{}` lives on `{}`; install the extension on that host and enable it there",
                    s.name, s.backend_type
                ));
            }
            tb.meta_set(&s.id, ENABLED_KEY, "on")?;
            println!("auto-continue on for {} ({})", s.name, s.id);
        }
        Some(false) => {
            tb.meta_set(&s.id, ENABLED_KEY, "off")?;
            println!("auto-continue off for {} ({})", s.name, s.id);
        }
        None => {
            tb.meta_unset(&s.id, ENABLED_KEY)?;
            println!("{} follows the global default again", s.name);
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

/// The shape `status --json` prints. `schema` is bumped on any breaking change,
/// because the interface plugin reads it.
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
            let toggle = meta.get(ENABLED_KEY).and_then(Value::as_str);
            let episode = meta.get(EPISODE_KEY).and_then(Value::as_str).and_then(Episode::parse);
            let why = ineligible(s);
            json!({
                "id": s.id,
                "name": s.name,
                "agent": s.agent,
                "eligible": why.is_none(),
                "ineligible_reason": why,
                "toggle": toggle,
                "enabled": why.is_none() && config::effective(cfg.enabled, toggle),
                "episode": episode.as_ref().map(|e| json!({
                    "state": e.state,
                    "label": e.label(),
                    "window": e.window,
                    "resets_at": e.resets_at,
                    "fire_at_ms": e.fire_at_ms,
                    "attempt": e.attempt,
                    "updated_at_ms": e.updated_at_ms,
                })),
            })
        })
        .collect();
    let out = json!({
        "schema": 1,
        "home": home,
        "extension_active": tb.extension_active().ok(),
        "config": cfg,
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
    }
    Ok(())
}

fn forget_all() -> Result<(), String> {
    let tb = Thurbox::default();
    for a in tb.automations()?.into_iter().filter(|a| a.name.starts_with(AUTOMATION_PREFIX)) {
        tb.remove_automation(a.id)?;
    }
    for s in tb.sessions()? {
        tb.meta_unset(&s.id, ENABLED_KEY)?;
        tb.meta_unset(&s.id, EPISODE_KEY)?;
    }
    println!("forgot every auto-continue toggle, episode and pending send");
    Ok(())
}
