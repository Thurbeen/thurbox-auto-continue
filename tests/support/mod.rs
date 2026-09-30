//! A sandboxed Thurbox for the end-to-end tests: a real `thurbox-cli` and a
//! real tmux server, every path of theirs under one temporary directory, the
//! TUI never started, and `claude` replaced by `examples/fake_claude.rs`.
//!
//! The `thurbox-cli` comes from `$TAC_THURBOX_CLI`, else from `PATH`. Every
//! command runs with a cleared environment, so nothing reaches the operator's
//! own Thurbox, tmux server or Claude config.
//!
//! On native Windows the multiplexer is psmux (on `PATH`, or named by
//! `$TAC_PSMUX`), Claude's hooks run under Git Bash as Claude runs them there
//! (`$CLAUDE_CODE_GIT_BASH_PATH`, else the one beside `git`), and the sandbox
//! copies binaries where POSIX links them.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

pub const BIN: &str = env!("CARGO_BIN_EXE_thurbox-auto-continue");

/// What the operator's Claude settings held before the extension arrived.
pub const USER_SETTINGS: &str = r#"{
  "theme": "dark",
  "hooks": {
    "Stop": [ { "hooks": [ { "type": "command", "command": "echo user-stop-hook" } ] } ],
    "StopFailure": [ { "matcher": "server_error", "hooks": [ { "type": "command", "command": "echo user-stopfailure-hook" } ] } ]
  }
}
"#;

pub struct Sandbox {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub ext_home: PathBuf,
    pub repo: PathBuf,
    pub bin: PathBuf,
    pub ctl: PathBuf,
    socket: std::cell::RefCell<Option<String>>,
    extra_env: Vec<(String, String)>,
}

/// `name` as an executable file name on this platform.
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `name` on the test runner's own PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

pub fn thurbox_cli() -> PathBuf {
    if let Some(p) = std::env::var_os("TAC_THURBOX_CLI") {
        return PathBuf::from(p);
    }
    on_path(&exe("thurbox-cli")).expect("no thurbox-cli: put one on PATH or set TAC_THURBOX_CLI")
}

/// The multiplexer Thurbox drives on this platform: tmux, or psmux on Windows.
pub fn mux() -> PathBuf {
    find_mux().expect("tests need tmux on PATH (psmux on Windows, or TAC_PSMUX)")
}

/// [`mux`], without panicking: `Drop` calls it while a failed test unwinds,
/// and a second panic there aborts the whole test binary.
fn find_mux() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("TAC_PSMUX").map(PathBuf::from).or_else(|| on_path("psmux.exe"))
    } else {
        on_path("tmux")
    }
}

/// The shell Claude runs a hook command under: `sh` on POSIX; on Windows the
/// Git Bash Claude Code requires, found the way Claude finds it.
pub fn hook_shell() -> PathBuf {
    find_hook_shell().expect("no Git Bash: set CLAUDE_CODE_GIT_BASH_PATH")
}

fn find_hook_shell() -> Option<PathBuf> {
    if !cfg!(windows) {
        return Some(PathBuf::from("sh"));
    }
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH") {
        return Some(PathBuf::from(p));
    }
    git_bash(&on_path("git.exe")?)
}

/// Git for Windows' own bash, for the `git.exe` found on PATH — which may be
/// `<git>\\cmd`, `<git>\\bin` or `<git>\\mingw64\\bin` — as `<git>\\bin\\bash.exe`.
/// Never a bare `bash.exe` from PATH: on Windows that can be WSL's.
pub fn git_bash(git: &Path) -> Option<PathBuf> {
    git.ancestors().skip(1).take(3).map(|d| d.join("bin").join("bash.exe")).find(|b| b.is_file())
}

/// Put `src` at `dst`: a symlink on POSIX, a hard link (or a copy) on Windows,
/// where a symlink needs a privilege a test runner may not have.
pub fn link(src: &Path, dst: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, dst).unwrap();
    #[cfg(windows)]
    if std::fs::hard_link(src, dst).is_err() {
        std::fs::copy(src, dst).unwrap();
    }
}

fn fake_claude() -> PathBuf {
    // target/<profile>/deps/<test> -> target/<profile>/examples/fake_claude
    let exe_path = std::env::current_exe().unwrap();
    let profile = exe_path.parent().unwrap().parent().unwrap();
    let fake = profile.join("examples").join(exe("fake_claude"));
    assert!(fake.is_file(), "{} missing: run through `cargo test`, which builds examples", fake.display());
    fake
}

impl Sandbox {
    /// A sandbox with Thurbox's built-in hooks active and the extension
    /// installed, as `install.sh` would leave a machine.
    pub fn new() -> Self {
        let sb = Self::bare();
        sb.install();
        sb
    }

    /// A sandbox with Thurbox's hooks active but without the extension.
    pub fn bare() -> Self {
        Self::with_prefix("tac-")
    }

    /// [`Sandbox::bare`], its root named from `prefix` — every path in it
    /// (the home, our binary, the extension home) starts with that name.
    pub fn with_prefix(prefix: &str) -> Self {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let tmp = tempfile::Builder::new().prefix(prefix).tempdir_in(&base).unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        let bin = root.join("bin");
        let ctl = root.join("ctl");
        for d in [&home, &repo, &bin, &ctl, &home.join(".claude")] {
            std::fs::create_dir_all(d).unwrap();
        }
        link(&thurbox_cli(), &bin.join(exe("thurbox-cli")));
        link(Path::new(BIN), &bin.join(exe("thurbox-auto-continue")));
        // tmux may live outside the sandbox PATH (Homebrew's /opt/homebrew/bin).
        // psmux stays where it is: it is found through PATH, see `command`.
        if cfg!(unix) {
            link(&mux(), &bin.join("tmux"));
        }
        std::fs::write(home.join(".claude/settings.json"), USER_SETTINGS).unwrap();
        let sb = Self {
            ext_home: home.join(".config/thurbox/auto-continue"),
            _tmp: tmp,
            root,
            home,
            repo,
            bin,
            ctl,
            socket: Default::default(),
            extra_env: Vec::new(),
        };
        sb.git(&["init", "-q", "-b", "main"]);
        std::fs::write(sb.repo.join("README.md"), "probe\n").unwrap();
        sb.git(&["add", "."]);
        sb.git(&["commit", "-qm", "init"]);
        // Thurbox's own status hooks, delivered through `--settings`.
        sb.cli(&["extension", "activate", "hooks"]);
        // Thurbox activates its other built-in on its own, and it writes its
        // active set read-modify-write: an install racing that write can be
        // dropped from the set. Activating it here first leaves nothing to race.
        sb.cli(&["extension", "activate", "ui-skill"]);
        let agents = sb.root.join("cfg/agents.toml");
        let text = std::fs::read_to_string(&agents).unwrap();
        // A TOML literal string: a Windows path is all backslashes.
        let fake = format!("command = '{}'", fake_claude().display());
        std::fs::write(&agents, text.replacen("command = \"claude\"", &fake, 1)).unwrap();
        sb
    }

    /// Install the extension from this checkout, the way `install.sh` does.
    pub fn install(&self) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"));
        self.cli(&["extension", "install", src.to_str().unwrap()]);
        // As install.sh does: Thurbox writes its active set read-modify-write,
        // so a heartbeat tick at the same moment can drop the install from it.
        self.wait("the extension to stay active", Duration::from_secs(20), || {
            self.cli(&["extension", "activate", "auto-continue"]);
            std::thread::sleep(Duration::from_millis(300));
            self.active("auto-continue")
        });
        let bin = self.ext_home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let _ = std::fs::remove_file(bin.join(exe("thurbox-auto-continue")));
        link(Path::new(BIN), &bin.join(exe("thurbox-auto-continue")));
        // Tests that need a clock run with no delay and a short confirmation.
        self.config(&[("delay_secs", "0"), ("confirm_secs", "5")]);
    }

    pub fn active(&self, name: &str) -> bool {
        self.cli(&["extension", "list"])
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| e["name"] == name && e["active"] == true)
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    fn git(&self, args: &[&str]) {
        let ok = self.command("git").args(args).current_dir(&self.repo).status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    /// The PATH every sandboxed command gets: the sandbox's own bin first.
    pub fn path(&self) -> std::ffi::OsString {
        if cfg!(windows) {
            // psmux, git and PowerShell where the machine keeps them.
            let mut dirs = vec![self.bin.clone()];
            dirs.extend(find_mux().and_then(|m| Some(m.parent()?.to_path_buf())));
            dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            std::env::join_paths(dirs).unwrap()
        } else {
            format!("{}:/usr/local/bin:/usr/bin:/bin", self.bin.display()).into()
        }
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear();
        if cfg!(windows) {
            // What Windows itself needs to start a process, and nothing of the
            // runner's own profile: the home, temp and app-data dirs are ours.
            for k in [
                "SystemRoot",
                "SystemDrive",
                "windir",
                "ComSpec",
                "PATHEXT",
                "OS",
                "PROCESSOR_ARCHITECTURE",
                "NUMBER_OF_PROCESSORS",
                "ProgramData",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramW6432",
                "CommonProgramFiles",
                "PSModulePath",
            ] {
                if let Some(v) = std::env::var_os(k) {
                    c.env(k, v);
                }
            }
            let tmp = self.root.join("tmp");
            let _ = std::fs::create_dir_all(&tmp);
            c.env("USERPROFILE", &self.home)
                .env("APPDATA", self.home.join("AppData").join("Roaming"))
                .env("LOCALAPPDATA", self.home.join("AppData").join("Local"))
                .env("TEMP", &tmp)
                .env("TMP", &tmp);
            if let Some(bash) = find_hook_shell() {
                c.env("CLAUDE_CODE_GIT_BASH_PATH", bash);
            }
        }
        c.env("PATH", self.path())
            .env("HOME", &self.home)
            .env("THURBOX_CONFIG_DIR", self.root.join("cfg"))
            .env("THURBOX_DATA_DIR", self.root.join("data"))
            .env("FAKE_CLAUDE_CTL", &self.ctl)
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .stdin(Stdio::null());
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        c
    }

    /// `thurbox-cli --json <args>`, which must succeed.
    pub fn cli(&self, args: &[&str]) -> Value {
        let mut c = self.command(self.bin.join(exe("thurbox-cli")));
        let out = output_within(c.arg("--json").args(args), &format!("thurbox-cli {args:?}"));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "thurbox-cli {args:?} failed: {text}");
        serde_json::from_str(text.trim()).unwrap_or(Value::Null)
    }

    /// Our binary, with `--home` pointing at the installed extension home.
    pub fn tac(&self, args: &[&str]) -> Output {
        output_within(self.command(BIN).args(args).arg("--home").arg(&self.ext_home), &format!("tac {args:?}"))
    }

    pub fn config(&self, pairs: &[(&str, &str)]) {
        for (k, v) in pairs {
            let out = self.tac(&["config", "set", k, v]);
            assert!(out.status.success(), "config set {k}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// A session running `agent`, its composer up.
    pub fn session(&self, name: &str, agent: &str) -> String {
        let v = self.cli(&[
            "session",
            "create",
            "--name",
            name,
            "--repo-path",
            self.repo.to_str().unwrap(),
            "--agent",
            agent,
        ]);
        let id = v["id"].as_str().unwrap().to_string();
        if self.socket.borrow().is_none() {
            *self.socket.borrow_mut() = v["tmux_socket"].as_str().map(String::from);
        }
        if agent == "claude" {
            self.wait("the fake claude to draw", Duration::from_secs(15), || self.screen(&id).contains("(fake)"));
        }
        id
    }

    pub fn screen(&self, id: &str) -> String {
        self.cli(&["session", "capture", id, "--lines", "60"])["output"].as_str().unwrap_or("").to_string()
    }

    pub fn script(&self, id: &str, replies: &[&str]) {
        let dir = self.ctl.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("script"), replies.join("\n")).unwrap();
    }

    pub fn ctl_file(&self, id: &str, name: &str) -> String {
        std::fs::read_to_string(self.ctl.join(id).join(name)).unwrap_or_default()
    }

    /// Every prompt the fake Claude has received, in order.
    pub fn received(&self, id: &str) -> Vec<String> {
        self.ctl_file(id, "received.log").lines().map(String::from).collect()
    }

    pub fn keys(&self, id: &str) -> Vec<String> {
        self.ctl_file(id, "keys.log").lines().map(String::from).collect()
    }

    /// Type a prompt and submit it, as a user would.
    pub fn prompt(&self, id: &str, text: &str) {
        self.cli(&["session", "send", id, "--", text]);
    }

    pub fn episode(&self, id: &str) -> Option<Value> {
        let v = self.cli(&["session", "meta", "get", id, "auto-continue.episode"]);
        v["value"].as_str().and_then(|s| serde_json::from_str(s).ok())
    }

    pub fn state(&self, id: &str) -> Option<String> {
        self.episode(id).and_then(|e| e["state"].as_str().map(String::from))
    }

    pub fn our_automations(&self) -> Vec<Value> {
        self.cli(&["automation", "list"])
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a["name"].as_str().is_some_and(|n| n.starts_with("auto-continue/")))
            .collect()
    }

    pub fn session_row(&self, id: &str) -> Value {
        self.cli(&["session", "list"]).as_array().unwrap().iter().find(|s| s["id"] == id).cloned().unwrap()
    }

    /// The pass Thurbox's heartbeat runs every minute, run now.
    pub fn tick(&self) -> Value {
        self.cli(&["automation", "tick"])
    }

    /// Tick until `id`'s episode leaves `armed`, the way the heartbeat would.
    pub fn tick_until_fired(&self, id: &str) {
        self.wait("the scheduled send to fire", Duration::from_secs(30), || {
            self.tick();
            !matches!(self.state(id).as_deref(), Some("armed") | Some("claimed"))
        });
    }

    pub fn wait(&self, what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + limit;
        while !done() {
            if Instant::now() >= end {
                self.dump();
                panic!("timed out waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Hit the limit on `id` and wait for the hook to arm the episode.
    pub fn hit_limit(&self, id: &str) -> Value {
        self.prompt(id, "hello");
        // Armed, and the hook has returned: nothing is still writing.
        self.wait("record to arm an episode", Duration::from_secs(30), || {
            self.ctl_file(id, "hooks.log")
                .lines()
                .any(|l| l.starts_with("StopFailure") && l.contains("thurbox-auto-continue"))
                && self.state(id).as_deref() == Some("armed")
        });
        self.episode(id).unwrap()
    }

    /// Everything a failed wait needs to be diagnosed: each pane and each
    /// fake Claude's own logs.
    pub fn dump(&self) {
        for s in self.cli(&["session", "list"]).as_array().cloned().unwrap_or_default() {
            let id = s["id"].as_str().unwrap_or_default();
            eprintln!("--- session {id} hook_state={} ---\n{}", s["hook_state"], self.screen(id));
            for f in ["received.log", "keys.log", "hooks.log", "env.log", "bytes.log", "script"] {
                eprintln!("[{f}]\n{}", self.ctl_file(id, f));
            }
            eprintln!("[meta] {}", self.cli(&["session", "meta", "list", id]));
        }
        eprintln!("[log]\n{}", std::fs::read_to_string(self.ext_home.join("auto-continue.log")).unwrap_or_default());
    }

    pub fn settings(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.home.join(".claude/settings.json")).unwrap()).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let socket = self.socket.borrow().clone().or_else(|| {
            let out =
                self.command(self.bin.join(exe("thurbox-cli"))).args(["--json", "runtime", "status"]).output().ok()?;
            let v: Value = serde_json::from_slice(&out.stdout).ok()?;
            v["tmux_socket"].as_str().map(String::from)
        });
        if let (Some(s), Some(mux)) = (socket, find_mux()) {
            let _ = self.command(mux).args(["-L", &s, "kill-server"]).stderr(Stdio::null()).status();
        }
    }
}

/// How long one command may take before the test fails instead of hanging.
const COMMAND_LIMIT: Duration = Duration::from_secs(90);

/// `Command::output`, bounded, and without waiting for its pipes to close: a
/// multiplexer server the command started can inherit them and hold them open
/// for as long as it lives (psmux does, on Windows). The output is what the
/// command wrote before it exited.
pub fn output_within(c: &mut Command, what: &str) -> Output {
    use std::io::Read;
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let drain = |mut r: Box<dyn Read + Send>| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    };
    let out_rx = drain(Box::new(child.stdout.take().unwrap()));
    let err_rx = drain(Box::new(child.stderr.take().unwrap()));
    let end = Instant::now() + COMMAND_LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            panic!("{what} still running after {COMMAND_LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // What was written before the exit is in the pipe already.
    let collect = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        let mut v = Vec::new();
        while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            v.extend(chunk);
        }
        v
    };
    Output { status, stdout: collect(out_rx), stderr: collect(err_rx) }
}

/// Our hook command, exactly as install merged it into settings.json.
pub fn stopfailure_command(sb: &Sandbox) -> String {
    sb.settings()["hooks"]["StopFailure"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(String::from))
        .find(|c| c.contains("thurbox-auto-continue"))
        .expect("our hook is merged")
}
