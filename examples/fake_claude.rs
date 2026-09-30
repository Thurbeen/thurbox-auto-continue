//! A scripted stand-in for Claude Code, for the end-to-end tests. Not shipped.
//!
//! It does the parts of Claude the extension depends on, in Claude 2.1.285's
//! shapes (see `tests/fixtures/`):
//!
//! - draws the composer between two rules, and the limit footer, menu and
//!   cancel notice the real one draws;
//! - writes transcript rows to `<config>/projects/<cwd>/<session-id>.jsonl`,
//!   the rejection followed by bookkeeping rows and the prompt row written after
//!   the rejection;
//! - runs hooks the way Claude does: from the user's `settings.json` **and**
//!   every `--settings` file, `StopFailure` matched on the error type, each
//!   under `sh -c` — on Windows under Git Bash, the shell Claude Code runs its
//!   hooks with there (`CLAUDE_CODE_GIT_BASH_PATH`, else the one beside `git`).
//!
//! It is driven by files under `$FAKE_CLAUDE_CTL/<$THURBOX_SESSION>/`:
//!
//! - `script`: one reply per prompt, in order (then `ok`):
//!   `ok`, `transient`, or `limit:<window>:<reset-in-secs>:<after>[:nohook]`
//!   where `<after>` is `cancelled`, `armed` or `menu`.
//! - `native_resume_secs`: when armed, resume by itself after that long.
//!
//! and it reports into the same directory: `received.log` (one line per
//! submitted prompt), `keys.log` (special keys), `hooks.log` (each hook run).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum View {
    Normal,
    Armed,
    Cancelled,
    Menu,
}

struct Fake {
    ctl: PathBuf,
    transcript: PathBuf,
    session_id: String,
    cwd: String,
    settings: Vec<PathBuf>,
    history: Vec<String>,
    composer: String,
    view: View,
    replies_used: usize,
    parent: Option<String>,
    resume_at: Option<SystemTime>,
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

fn iso(ms: u128) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}

fn uuid() -> String {
    let n = now_ms() ^ (u128::from(std::process::id()) << 64);
    let mut x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835);
    x ^= u128::from(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) << 17;
    let h = format!("{x:032x}");
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn append(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn munge(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn clock(ms: u128) -> String {
    let secs = (ms / 1000) as u64 % 86_400;
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

impl Fake {
    fn row(&mut self, mut v: Value, at: u128) {
        let id = uuid();
        v["uuid"] = json!(id);
        v["timestamp"] = json!(iso(at));
        v["sessionId"] = json!(self.session_id);
        v["cwd"] = json!(self.cwd);
        v["parentUuid"] = json!(self.parent);
        v["version"] = json!("2.1.285");
        self.parent = Some(id);
        append(&self.transcript, &v.to_string());
    }

    fn bookkeeping(&self, v: Value) {
        append(&self.transcript, &v.to_string());
    }

    /// Run every hook for `event` from every settings source, like Claude.
    fn hooks(&self, event: &str, target: &str, mut payload: Value) {
        payload["hook_event_name"] = json!(event);
        payload["session_id"] = json!(self.session_id);
        payload["transcript_path"] = json!(self.transcript);
        payload["cwd"] = json!(self.cwd);
        let config = claude_config();
        let mut sources = vec![config.join("settings.json")];
        sources.extend(self.settings.iter().cloned());
        for src in sources {
            let Ok(text) = std::fs::read_to_string(&src) else { continue };
            let Ok(doc) = serde_json::from_str::<Value>(&text) else { continue };
            for group in doc["hooks"][event].as_array().into_iter().flatten() {
                let matcher = group["matcher"].as_str().unwrap_or("");
                if !(matcher.is_empty() || matcher == "*" || matcher.split('|').any(|m| m == target)) {
                    continue;
                }
                for hook in group["hooks"].as_array().into_iter().flatten() {
                    let Some(cmd) = hook["command"].as_str() else { continue };
                    let timeout = hook["timeout"].as_u64().unwrap_or(60);
                    let (code, out_len) = run_hook(cmd, &payload.to_string(), timeout);
                    append(
                        &self.ctl.join("hooks.log"),
                        &format!("{event} source={} exit={code} stdout_bytes={out_len} cmd={cmd}", src.display()),
                    );
                }
            }
        }
    }

    fn next_reply(&mut self) -> String {
        let script = std::fs::read_to_string(self.ctl.join("script")).unwrap_or_default();
        let reply =
            script.lines().filter(|l| !l.trim().is_empty()).nth(self.replies_used).unwrap_or("ok").trim().to_string();
        self.replies_used += 1;
        reply
    }

    fn submit(&mut self, text: String) {
        append(&self.ctl.join("received.log"), &text);
        self.history.push(format!("❯ {text}"));
        let asked = now_ms();
        self.hooks("UserPromptSubmit", "", json!({ "prompt": text }));
        let reply = self.next_reply();
        let parts: Vec<&str> = reply.split(':').collect();
        match parts[0] {
            "limit" => {
                let window = parts.get(1).copied().unwrap_or("five_hour");
                let reset_in: u128 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(3600);
                let after = parts.get(3).copied().unwrap_or("cancelled");
                let hook = parts.get(4).copied() != Some("nohook");
                let at = now_ms();
                let resets = at / 1000 + reset_in;
                let line = format!("You've hit your session limit · resets {} (Etc/UTC)", clock(resets * 1000));
                self.row(
                    json!({
                        "type": "assistant", "isApiErrorMessage": true, "error": "rate_limit",
                        "apiErrorStatus": 429,
                        "quotaLimits": { "status": "rejected", "resetsAt": resets as u64,
                            "unifiedRateLimitFallbackAvailable": false, "rateLimitType": window,
                            "isUsingOverage": false },
                        "message": { "role": "assistant", "content": [{ "type": "text", "text": line }] }
                    }),
                    at,
                );
                self.row(json!({ "type": "system", "subtype": "turn_duration", "durationMs": 90 }), at + 2);
                // Claude writes the prompt row after the rejection, with the
                // earlier timestamp, then its bookkeeping.
                self.row(json!({ "type": "user", "message": { "role": "user", "content": text } }), asked);
                self.bookkeeping(json!({ "type": "last-prompt", "lastPrompt": text, "sessionId": self.session_id }));
                self.bookkeeping(json!({ "type": "cost-state", "sessionId": self.session_id }));
                self.history.push(format!("  ⎿  {line}"));
                self.view = match after {
                    "armed" => View::Armed,
                    "menu" => View::Menu,
                    _ => View::Cancelled,
                };
                if self.view == View::Armed {
                    let secs = std::fs::read_to_string(self.ctl.join("native_resume_secs"))
                        .ok()
                        .and_then(|s| s.trim().parse().ok());
                    self.resume_at = secs.map(|s: u64| SystemTime::now() + Duration::from_secs(s));
                }
                if hook {
                    self.hooks(
                        "StopFailure",
                        "rate_limit",
                        json!({ "error": "rate_limit", "last_assistant_message": line }),
                    );
                }
            }
            "transient" => {
                let line = "API Error: Server is temporarily limiting requests (not your usage limit) · rate limited";
                self.row(
                    json!({ "type": "assistant", "isApiErrorMessage": true, "error": "rate_limit",
                        "apiErrorIsTransient": true, "apiErrorStatus": 429,
                        "message": { "role": "assistant", "content": [{ "type": "text", "text": line }] } }),
                    now_ms(),
                );
                self.row(json!({ "type": "user", "message": { "role": "user", "content": text } }), asked);
                self.history.push(format!("  ⎿  {line}"));
                self.view = View::Normal;
                self.hooks(
                    "StopFailure",
                    "rate_limit",
                    json!({ "error": "rate_limit", "last_assistant_message": line }),
                );
            }
            _ => {
                self.row(json!({ "type": "user", "message": { "role": "user", "content": text } }), asked);
                self.row(
                    json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "ok" }] } }),
                    now_ms(),
                );
                self.history.push("● ok".into());
                self.view = View::Normal;
                self.hooks("Stop", "", json!({}));
            }
        }
    }

    fn native_resume(&mut self) {
        self.resume_at = None;
        let text = "Your claude.ai usage limit has reset. Continue the task you were working on when the limit was reached; do not repeat work.";
        self.bookkeeping(json!({ "type": "system", "subtype": "informational", "content": "Usage limit reset · continuing automatically" }));
        self.row(json!({ "type": "user", "isMeta": true, "message": { "role": "user", "content": text } }), now_ms());
        self.row(
            json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "ok" }] } }),
            now_ms(),
        );
        append(&self.ctl.join("keys.log"), "native-resume");
        self.history.push("● ok".into());
        self.view = View::Normal;
    }

    fn draw(&self) {
        let rule = "─".repeat(78);
        let mut out = String::from("\x1b[H\x1b[J");
        out.push_str(" ▐▛███▛█   Claude Code v2.1.285 (fake)\r\n\r\n");
        let start = self.history.len().saturating_sub(8);
        for line in &self.history[start..] {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        if self.view == View::Menu {
            out.push_str(&"▔".repeat(78));
            out.push_str("\r\n   What do you want to do?\r\n");
            out.push_str("   ❯ 1. Stop and wait for limit to reset\r\n");
            out.push_str("     2. Wait here, then continue automatically\r\n");
            out.push_str("     3. Upgrade your plan\r\n");
            out.push_str("   Enter to confirm · Esc to cancel\r\n");
        } else {
            if self.view == View::Cancelled {
                out.push_str("● Automatic continue cancelled · /rate-limit-options to re-arm\r\n");
            }
            out.push_str(&format!("{rule}\r\n❯ {}\r\n{rule}\r\n", self.composer));
            if self.view == View::Armed {
                out.push_str("  ⚠ Usage limit reached · limit resets soon\r\n");
                out.push_str("    Continuing automatically at 12:54pm · esc to cancel\r\n");
            }
            out.push_str("  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents");
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn key(&mut self, name: &str) {
        append(&self.ctl.join("keys.log"), name);
    }
}

/// The shell a hook command runs under.
fn hook_shell() -> PathBuf {
    if !cfg!(windows) {
        return PathBuf::from("sh");
    }
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join("git.exe"))
        .find(|p| p.is_file())
        .and_then(|git| Some(git.parent()?.parent()?.join("bin").join("bash.exe")))
        .unwrap_or_else(|| PathBuf::from("bash.exe"))
}

fn run_hook(cmd: &str, stdin: &str, timeout_secs: u64) -> (i32, usize) {
    let Ok(mut child) = Command::new(hook_shell())
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return (-1, 0);
    };
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(stdin.as_bytes());
    }
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf.len()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return (status.code().unwrap_or(-1), reader.join().unwrap_or(0));
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            return (-9, 0);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn claude_config() -> PathBuf {
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os(home).unwrap()).join(".claude"))
}

/// Raw keys, no echo: the composer is drawn by us, as Claude draws its own.
#[cfg(unix)]
fn raw_mode() {
    let _ = Command::new("stty").args(["-icanon", "-echo", "min", "1"]).stdin(Stdio::inherit()).status();
}

/// The same on a Windows console (a psmux pane is a ConPTY): no line editing,
/// no echo, Ctrl-keys as bytes, and keys arriving as VT sequences — what
/// Claude Code's own input layer asks for.
#[cfg(windows)]
fn raw_mode() {
    type Handle = *mut std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    }
    const STD_INPUT: u32 = -10i32 as u32;
    const STD_OUTPUT: u32 = -11i32 as u32;
    const PROCESSED_INPUT: u32 = 0x1;
    const LINE_INPUT: u32 = 0x2;
    const ECHO_INPUT: u32 = 0x4;
    const VT_INPUT: u32 = 0x200;
    const VT_PROCESSING: u32 = 0x4;
    // SAFETY: plain Win32 calls on this process's own standard handles.
    unsafe {
        let input = GetStdHandle(STD_INPUT);
        let mut mode = 0;
        if GetConsoleMode(input, &mut mode) != 0 {
            SetConsoleMode(input, (mode & !(PROCESSED_INPUT | LINE_INPUT | ECHO_INPUT)) | VT_INPUT);
        }
        let output = GetStdHandle(STD_OUTPUT);
        let mut mode = 0;
        if GetConsoleMode(output, &mut mode) != 0 {
            SetConsoleMode(output, mode | VT_PROCESSING);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut settings = Vec::new();
    let mut session_id = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--settings" => settings.extend(args.get(i + 1).map(PathBuf::from)),
            "--session-id" | "--resume" => session_id = args.get(i + 1).cloned(),
            _ => {}
        }
        i += 1;
    }
    let session_id = session_id.unwrap_or_else(uuid);
    let cwd = std::env::current_dir().unwrap().display().to_string();
    let thurbox_session = std::env::var("THURBOX_SESSION").unwrap_or_else(|_| "none".into());
    let ctl = PathBuf::from(std::env::var_os("FAKE_CLAUDE_CTL").expect("FAKE_CLAUDE_CTL")).join(thurbox_session);
    std::fs::create_dir_all(&ctl).unwrap();
    let transcript = claude_config().join("projects").join(munge(&cwd)).join(format!("{session_id}.jsonl"));

    raw_mode();

    let mut fake = Fake {
        ctl,
        transcript,
        session_id,
        cwd,
        settings,
        history: Vec::new(),
        composer: String::new(),
        view: View::Normal,
        replies_used: 0,
        parent: None,
        resume_at: None,
    };
    fake.hooks("SessionStart", "startup", json!({ "source": "startup" }));
    // Claude asks for bracketed paste; tmux then brackets what is pasted.
    print!("\x1b[?2004h");
    fake.draw();

    let (tx, rx) = mpsc::channel::<u8>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut b = [0u8; 1];
        while stdin.read(&mut b).map(|n| n == 1).unwrap_or(false) {
            if tx.send(b[0]).is_err() {
                break;
            }
        }
    });
    let mut pending: Vec<u8> = Vec::new();
    loop {
        if fake.resume_at.is_some_and(|t| SystemTime::now() >= t) && fake.view == View::Armed {
            fake.native_resume();
            fake.draw();
        }
        let byte = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => b,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        match byte {
            0x1b => match rx.recv_timeout(Duration::from_millis(30)) {
                // A CSI sequence: read up to its final byte.
                Ok(b'[') => {
                    let mut seq = Vec::new();
                    while let Ok(b) = rx.recv_timeout(Duration::from_millis(50)) {
                        seq.push(b);
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                    if seq == b"200~" {
                        // Bracketed paste, as `thurbox-cli session send` types.
                        let mut text = Vec::new();
                        while let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
                            text.push(b);
                            if text.ends_with(b"\x1b[201~") {
                                text.truncate(text.len() - 6);
                                break;
                            }
                        }
                        if fake.view != View::Menu {
                            fake.composer.push_str(&String::from_utf8_lossy(&text));
                            if fake.view == View::Cancelled {
                                fake.view = View::Normal;
                            }
                        }
                        fake.key("paste");
                    } else {
                        fake.key("sequence");
                    }
                }
                Ok(_) => fake.key("sequence"),
                Err(_) => {
                    fake.key("escape");
                    if matches!(fake.view, View::Menu | View::Armed) {
                        fake.view = View::Cancelled;
                        fake.resume_at = None;
                    }
                }
            },
            b'\r' | b'\n' => {
                fake.key("enter");
                if fake.view == View::Menu {
                    fake.view = View::Cancelled;
                } else if !fake.composer.is_empty() {
                    let text = std::mem::take(&mut fake.composer);
                    fake.submit(text);
                }
            }
            0x15 => {
                fake.key("ctrl-u");
                fake.composer.clear();
            }
            0x7f | 0x08 => {
                fake.composer.pop();
            }
            b if b >= 0x20 => {
                if fake.view == View::Menu {
                    continue;
                }
                pending.push(b);
                if let Ok(s) = std::str::from_utf8(&pending) {
                    fake.composer.push_str(s);
                    pending.clear();
                }
                if fake.view == View::Cancelled {
                    fake.view = View::Normal;
                }
            }
            _ => {}
        }
        fake.draw();
    }
}
