//! The only module that runs `thurbox-cli`. Every call asks for `--json`, has
//! a hard timeout, and kills the child when it runs over.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Default per-call timeout. Overridden by `THURBOX_AUTO_CONTINUE_CLI_TIMEOUT_MS`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const TIMEOUT_ENV: &str = "THURBOX_AUTO_CONTINUE_CLI_TIMEOUT_MS";

/// The manifest name `extension list` reports us under.
pub const EXTENSION_NAME: &str = "auto-continue";

pub struct Thurbox {
    bin: PathBuf,
    timeout: Duration,
}

/// The fields of a session row this crate reads.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub reports_as: Option<String>,
    pub backend_type: String,
    pub stopped: bool,
    pub agent_session_id: Option<String>,
    pub hook_state: Option<String>,
    pub hook_state_at: Option<i64>,
}

impl Session {
    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(String::from);
        Some(Self {
            id: s("id")?,
            name: s("name").unwrap_or_default(),
            agent: s("agent").unwrap_or_default(),
            reports_as: s("reports_as"),
            backend_type: s("backend_type").unwrap_or_default(),
            stopped: v["stopped"].as_bool().unwrap_or(false),
            agent_session_id: s("agent_session_id"),
            hook_state: s("hook_state"),
            hook_state_at: v["hook_state_at"].as_i64(),
        })
    }

    /// Only Claude sessions are ever acted on.
    pub fn is_claude(&self) -> bool {
        self.agent == "claude" || self.reports_as.as_deref() == Some("claude")
    }

    pub fn is_local(&self) -> bool {
        crate::platform::is_local_backend(&self.backend_type)
    }
}

#[derive(Debug, Clone)]
pub struct Automation {
    pub id: i64,
    pub name: String,
}

impl Default for Thurbox {
    fn default() -> Self {
        let timeout = std::env::var(TIMEOUT_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        Self { bin: crate::platform::thurbox_cli(), timeout }
    }
}

impl Thurbox {
    /// Run `thurbox-cli --json <args>` and parse what it prints. A non-zero
    /// exit, a timeout or unparseable output is an error.
    pub fn run(&self, args: &[&str]) -> Result<Value, String> {
        let verb = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let mut child = Command::new(&self.bin)
            .arg("--json")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("thurbox-cli {verb}: {e}"))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The reader thread is left behind: a grandchild may still
                    // hold the pipe open, and waiting on it would be unbounded.
                    return Err(format!("thurbox-cli {verb}: timed out after {:?}", self.timeout));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(format!("thurbox-cli {verb}: {e}")),
            }
        };
        let out = reader.join().unwrap_or_default();
        if !status.success() {
            return Err(format!("thurbox-cli {verb}: exited {status}"));
        }
        let text = String::from_utf8_lossy(&out);
        serde_json::from_str(text.trim()).map_err(|e| format!("thurbox-cli {verb}: unreadable output: {e}"))
    }

    pub fn sessions(&self) -> Result<Vec<Session>, String> {
        let v = self.run(&["session", "list"])?;
        Ok(v.as_array().map(|a| a.iter().filter_map(Session::from_json).collect()).unwrap_or_default())
    }

    /// One session, or `None` when it does not exist.
    pub fn session(&self, id: &str) -> Result<Option<Session>, String> {
        let all = self.sessions()?;
        Ok(all.into_iter().find(|s| s.id == id || s.name == id))
    }

    pub fn meta_get(&self, id: &str, key: &str) -> Result<Option<String>, String> {
        let v = self.run(&["session", "meta", "get", id, key])?;
        Ok(v["value"].as_str().map(String::from))
    }

    /// Every meta key set on a session.
    pub fn meta_list(&self, id: &str) -> Result<serde_json::Map<String, Value>, String> {
        let v = self.run(&["session", "meta", "list", id])?;
        Ok(v.as_object().cloned().unwrap_or_default())
    }

    pub fn meta_set(&self, id: &str, key: &str, value: &str) -> Result<(), String> {
        self.run(&["session", "meta", "set", id, key, "--", value]).map(drop)
    }

    pub fn meta_unset(&self, id: &str, key: &str) -> Result<(), String> {
        self.run(&["session", "meta", "unset", id, key]).map(drop)
    }

    pub fn signal_idle(&self, id: &str) -> Result<(), String> {
        self.run(&["session", "signal", "--session", id, "--state", "idle"]).map(drop)
    }

    /// The pane's rendered text.
    pub fn capture(&self, id: &str) -> Result<String, String> {
        let v = self.run(&["session", "capture", id, "--lines", "60"])?;
        v["output"].as_str().map(String::from).ok_or_else(|| "capture: no output field".into())
    }

    pub fn type_text(&self, id: &str, text: &str) -> Result<(), String> {
        self.run(&["session", "send", "--no-enter", id, "--", text]).map(drop)
    }

    pub fn key(&self, id: &str, key: &str) -> Result<(), String> {
        self.run(&["session", "key", id, key]).map(drop)
    }

    /// Whether the extension is installed and active on this machine.
    pub fn extension_active(&self) -> Result<bool, String> {
        let v = self.run(&["extension", "list"])?;
        Ok(v.as_array().into_iter().flatten().any(|e| e["name"] == EXTENSION_NAME && e["active"] == true))
    }

    pub fn automations(&self) -> Result<Vec<Automation>, String> {
        let v = self.run(&["automation", "list"])?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| Some(Automation { id: a["id"].as_i64()?, name: a["name"].as_str()?.to_string() }))
            .collect())
    }

    /// A one-shot Exec automation at `at_ms`. Returns its id.
    pub fn schedule_exec(&self, name: &str, at_ms: i64, command: &str) -> Result<i64, String> {
        let trigger = format!("at:{at_ms}");
        let v = self.run(&["automation", "create", "--name", name, "--trigger", &trigger, "--command", command])?;
        v["id"].as_i64().ok_or_else(|| "automation create: no id".into())
    }

    pub fn remove_automation(&self, id: i64) -> Result<(), String> {
        self.run(&["automation", "remove", &id.to_string()]).map(drop)
    }
}
