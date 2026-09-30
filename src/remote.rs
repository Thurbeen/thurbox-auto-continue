//! Sessions on shared SSH and WSL hosts, host-local.
//!
//! A session on a host that shares its sessions (`share_sessions = true`, the
//! Thurbox default) is a row in that host's own database, mirrored here under
//! `ssh:<name>` / `wsl:<name>` with the same id. Its settings and its episode
//! live in the host's session meta, which Thurbox does not mirror, and the
//! host's own install records, schedules and sends. This machine never acts on
//! such a session. It only asks the host, through `thurbox-cli session exec`,
//! which runs one command in the session's context on the machine it lives on:
//! the host's `thurbox-auto-continue`, with `--delegated` so that the host
//! never passes the question on again.
//!
//! Everything else is unsupported and says why: a host with sharing off, a
//! backend `hosts.toml` does not name, a host without the extension, one whose
//! extension predates delegation, one that cannot be reached, a session the
//! host does not know (made before sharing), and a session the host itself
//! reaches through another host.

use serde_json::{Value, json};

use crate::thurbox::{Session, Thurbox};

/// The flag a delegated run carries. Hidden from `--help`.
pub const DELEGATED_FLAG: &str = "--delegated";

/// Finds the host's binary without relying on the PATH a non-interactive
/// login gives: the extension home first, then PATH, then `~/.local/bin`.
/// Exit 127 means none was found.
const LAUNCH: &str = r#"b="$HOME/.config/thurbox/auto-continue/bin/thurbox-auto-continue"
[ -x "$b" ] || b=$(command -v thurbox-auto-continue) || b="$HOME/.local/bin/thurbox-auto-continue"
[ -x "$b" ] || exit 127
exec "$b" "$@""#;

/// Why a remote session cannot be read or changed from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// The backend names a host `hosts.toml` does not describe.
    UnknownHost,
    /// `share_sessions = false`: this machine drives the host, no host
    /// database owns the session, and nothing there can act for it.
    NotShared,
    /// No `thurbox-auto-continue` on the host.
    NotInstalled,
    /// The host's extension predates delegation.
    Outdated,
    /// `session exec` failed, or the host's answer could not be read.
    Unreachable,
    /// The host does not list the session: made before the host shared.
    UnknownToHost,
    /// The host itself reaches the session through another host.
    Transitive,
}

impl Unsupported {
    /// The word `status --json` reports under `host.reason`.
    pub fn reason(&self) -> &'static str {
        match self {
            Unsupported::UnknownHost => "unknown-host",
            Unsupported::NotShared => "not-shared",
            Unsupported::NotInstalled => "not-installed",
            Unsupported::Outdated => "outdated",
            Unsupported::Unreachable => "unreachable",
            Unsupported::UnknownToHost => "unknown-to-host",
            Unsupported::Transitive => "transitive",
        }
    }

    pub fn message(&self, s: &Session) -> String {
        let (name, backend) = (&s.name, &s.backend_type);
        match self {
            Unsupported::UnknownHost => format!("`{name}` runs on `{backend}`, which is not in hosts.toml"),
            Unsupported::NotShared => format!(
                "`{name}` runs on `{backend}`, which does not share its sessions (share_sessions = false); \
                 auto-continue needs the host's own database, so it is unsupported there"
            ),
            Unsupported::NotInstalled => {
                format!("`{name}` runs on `{backend}`, which has no auto-continue: install the extension on that host")
            }
            Unsupported::Outdated => {
                format!("`{name}` runs on `{backend}`, whose auto-continue is too old: update it on that host")
            }
            Unsupported::Unreachable => format!("`{name}` runs on `{backend}`, which did not answer"),
            Unsupported::UnknownToHost => format!(
                "`{name}` runs on `{backend}`, whose database does not know it (made before the host shared its \
                 sessions; `thurbox-cli session sync --host <name> --adopt` registers it there)"
            ),
            Unsupported::Transitive => {
                format!("`{name}` is reached through `{backend}` from another host; set it on the host it runs on")
            }
        }
    }

    /// The `host` object `status --json` carries for a session it could not read.
    pub fn host_json(&self, s: &Session) -> Value {
        json!({ "backend": s.backend_type, "reason": self.reason(), "extension_active": null })
    }
}

/// The host part of a remote backend type: `devbox` for `ssh:devbox`.
pub fn host_name(backend_type: &str) -> Option<&str> {
    backend_type.strip_prefix("ssh:").or_else(|| backend_type.strip_prefix("wsl:"))
}

/// The hosts this machine's Thurbox knows, and which of them share.
#[derive(Debug, Default)]
pub struct Hosts {
    names: Vec<String>,
    shared: Vec<String>,
}

impl Hosts {
    pub fn read(tb: &Thurbox) -> Result<Self, String> {
        let v = tb.run(&["config", "show"])?;
        let list = |k: &str| -> Vec<String> {
            v["hosts"][k].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)).collect()
        };
        Ok(Self { names: list("names"), shared: list("shared") })
    }

    /// Whether this machine may ask the session's host at all.
    pub fn check(&self, s: &Session) -> Result<(), Unsupported> {
        let name = host_name(&s.backend_type).ok_or(Unsupported::UnknownHost)?;
        if !self.names.iter().any(|n| n == name) {
            return Err(Unsupported::UnknownHost);
        }
        if !self.shared.iter().any(|n| n == name) {
            return Err(Unsupported::NotShared);
        }
        Ok(())
    }
}

/// What the host's `thurbox-auto-continue` answered: its exit code and the one
/// JSON object it printed.
#[derive(Debug)]
pub struct Answer {
    pub code: i32,
    pub body: Value,
}

/// Run `thurbox-auto-continue <args> --json --delegated -- <positional>` on
/// the session's host. The `--` keeps a value that starts with `-` a value.
pub fn ask(tb: &Thurbox, s: &Session, args: &[&str], positional: &[&str]) -> Result<Answer, Unsupported> {
    let mut argv = vec!["session", "exec", &s.id, "--", "sh", "-c", LAUNCH, "thurbox-auto-continue"];
    argv.extend_from_slice(args);
    argv.extend(["--json", DELEGATED_FLAG, "--"]);
    argv.extend_from_slice(positional);
    let out = tb.run_remote(&argv).map_err(|_| Unsupported::Unreachable)?;
    read_answer(&out)
}

/// `session exec`'s result, read as the host's answer.
fn read_answer(out: &Value) -> Result<Answer, Unsupported> {
    let code = out["exit_code"].as_i64().ok_or(Unsupported::Unreachable)? as i32;
    if code == 127 {
        return Err(Unsupported::NotInstalled);
    }
    let body: Value = out["stdout"]
        .as_str()
        .and_then(|t| serde_json::from_str(t.trim()).ok())
        .filter(Value::is_object)
        .ok_or(Unsupported::Unreachable)?;
    // An install from before delegation rejects the flag as a usage error.
    if code == 2 && body["error"].as_str().is_some_and(|e| e.contains(DELEGATED_FLAG)) {
        return Err(Unsupported::Outdated);
    }
    Ok(Answer { code, body })
}

/// Keep only the known fields of `v`, recursively by `shape`: a host's answer
/// is relayed as this contract defines it and never carries anything else.
fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().filter_map(|k| v.get(*k).map(|x| (k.to_string(), x.clone()))).collect())
}

/// A session row of the host's `status --json`, reduced to the contract's
/// fields (docs/CLI-CONTRACT.md).
pub fn sanitize_row(row: &Value) -> Value {
    let mut out =
        pick(row, &["id", "name", "agent", "eligible", "ineligible_reason", "enabled", "overrides", "warnings"]);
    let setting = |k: &str| pick(&row["settings"][k], &["value", "source"]);
    out["settings"] = json!({
        "enabled": setting("enabled"),
        "message": setting("message"),
        "delay_secs": setting("delay_secs"),
    });
    out["overrides"] = pick(&row["overrides"], &["enabled", "message", "delay_secs"]);
    out["episode"] = match &row["episode"] {
        Value::Object(_) => pick(
            &row["episode"],
            &[
                "state",
                "label",
                "reason",
                "window",
                "resets_at_ms",
                "next_send_at_ms",
                "attempt",
                "max_attempts",
                "sent_at_ms",
                "updated_at_ms",
            ],
        ),
        _ => Value::Null,
    };
    out["last_outcome"] = match &row["last_outcome"] {
        Value::Object(_) => pick(&row["last_outcome"], &["state", "reason", "label", "at_ms"]),
        _ => Value::Null,
    };
    out["warnings"] =
        Value::Array(row["warnings"].as_array().into_iter().flatten().filter(|w| w.is_string()).cloned().collect());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(backend: &str) -> Session {
        Session { id: "s".into(), name: "w".into(), backend_type: backend.into(), ..Default::default() }
    }

    #[test]
    fn only_a_known_sharing_host_is_asked() {
        let hosts = Hosts { names: vec!["a".into(), "b".into()], shared: vec!["a".into()] };
        assert_eq!(hosts.check(&session("ssh:a")), Ok(()));
        assert_eq!(hosts.check(&session("wsl:a")), Ok(()));
        assert_eq!(hosts.check(&session("ssh:b")), Err(Unsupported::NotShared));
        assert_eq!(hosts.check(&session("ssh:c")), Err(Unsupported::UnknownHost));
        assert_eq!(hosts.check(&session("psmux:x")), Err(Unsupported::UnknownHost));
    }

    #[test]
    fn a_host_answer_is_read_or_named_unsupported() {
        let exec = |code: Value, stdout: &str| json!({ "exit_code": code, "stdout": stdout, "env": {"K": "secret"} });
        assert_eq!(read_answer(&exec(json!(127), "")).unwrap_err(), Unsupported::NotInstalled);
        assert_eq!(read_answer(&exec(Value::Null, "")).unwrap_err(), Unsupported::Unreachable);
        assert_eq!(read_answer(&exec(json!(0), "not json")).unwrap_err(), Unsupported::Unreachable);
        let old = r#"{"ok":false,"error":"unexpected argument '--delegated' found"}"#;
        assert_eq!(read_answer(&exec(json!(2), old)).unwrap_err(), Unsupported::Outdated);
        let a = read_answer(&exec(json!(1), r#"{"ok":false,"error":"no"}"#)).unwrap();
        assert_eq!((a.code, a.body["error"].as_str()), (1, Some("no")));
    }

    #[test]
    fn a_relayed_row_carries_only_the_contract() {
        let row = json!({
            "id": "s", "name": "w", "agent": "claude", "eligible": true, "ineligible_reason": null,
            "enabled": true, "transcript": "/home/user/.claude/x.jsonl",
            "settings": { "message": { "value": "go", "source": "session", "raw": "x" } },
            "overrides": { "message": "go", "other": 1 },
            "episode": { "state": "armed", "transcript": "/secret.jsonl", "attempt": 1 },
            "last_outcome": null,
            "warnings": ["w", { "not": "a string" }],
        });
        let out = sanitize_row(&row);
        let text = out.to_string();
        assert!(!text.contains("jsonl") && !text.contains("raw") && !text.contains("other"), "{text}");
        assert_eq!(out["settings"]["message"], json!({ "value": "go", "source": "session" }));
        assert_eq!(out["episode"], json!({ "state": "armed", "attempt": 1 }));
        assert_eq!(out["warnings"], json!(["w"]));
        assert!(out["last_outcome"].is_null());
    }
}
