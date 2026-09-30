//! A sandboxed Thurbox for the end-to-end tests: a real `thurbox-cli` and a
//! real tmux server, every path of theirs under one temporary directory, the
//! TUI never started, and `claude` replaced by `examples/fake_claude.rs`.
//!
//! The `thurbox-cli` comes from `$TAC_THURBOX_CLI`, else from `PATH`. Every
//! command runs with a cleared environment, so nothing reaches the operator's
//! own Thurbox, tmux server or Claude config.

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
    /// tmux servers this sandbox started on a host that does not share its
    /// sessions: the laptop drives them, so the laptop tears them down.
    legacy_sockets: std::cell::RefCell<Vec<String>>,
    extra_env: Vec<(String, String)>,
}

/// How a laptop reaches a host: Thurbox's two transports.
#[derive(Clone, Copy, PartialEq)]
pub enum Link {
    Ssh,
    Wsl,
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
    on_path("thurbox-cli").expect("no thurbox-cli: put one on PATH or set TAC_THURBOX_CLI")
}

fn fake_claude() -> PathBuf {
    // target/<profile>/deps/<test> -> target/<profile>/examples/fake_claude
    let exe = std::env::current_exe().unwrap();
    let profile = exe.parent().unwrap().parent().unwrap();
    let fake = profile.join("examples").join("fake_claude");
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
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let tmp = tempfile::Builder::new().prefix("tac-").tempdir_in(&base).unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        let bin = root.join("bin");
        let ctl = root.join("ctl");
        for d in [&home, &repo, &bin, &ctl, &home.join(".claude")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::os::unix::fs::symlink(thurbox_cli(), bin.join("thurbox-cli")).unwrap();
        std::os::unix::fs::symlink(BIN, bin.join("thurbox-auto-continue")).unwrap();
        // tmux may live outside the sandbox PATH (Homebrew's /opt/homebrew/bin).
        let tmux = on_path("tmux").expect("tests need tmux on PATH");
        std::os::unix::fs::symlink(tmux, bin.join("tmux")).unwrap();
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
            legacy_sockets: Default::default(),
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
        let fake = format!("command = \"{}\"", fake_claude().display());
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
        let _ = std::fs::remove_file(bin.join("thurbox-auto-continue"));
        std::os::unix::fs::symlink(BIN, bin.join("thurbox-auto-continue")).unwrap();
        // And on the user's own PATH, as install.sh links it.
        let local_bin = self.home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let _ = std::fs::remove_file(local_bin.join("thurbox-auto-continue"));
        std::os::unix::fs::symlink(BIN, local_bin.join("thurbox-auto-continue")).unwrap();
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

    /// The whole environment a command in this sandbox runs under, `PATH` first.
    fn env(&self, path: String) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = [
            ("PATH", path),
            ("HOME", self.home.display().to_string()),
            ("THURBOX_CONFIG_DIR", self.root.join("cfg").display().to_string()),
            ("THURBOX_DATA_DIR", self.root.join("data").display().to_string()),
            ("FAKE_CLAUDE_CTL", self.ctl.display().to_string()),
            ("TERM", "xterm-256color".into()),
            ("LANG", "C.UTF-8".into()),
            ("GIT_AUTHOR_NAME", "t".into()),
            ("GIT_AUTHOR_EMAIL", "t@example.com".into()),
            ("GIT_COMMITTER_NAME", "t".into()),
            ("GIT_COMMITTER_EMAIL", "t@example.com".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        env.extend(self.extra_env.iter().cloned());
        env
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear()
            .envs(self.env(format!("{}:/usr/local/bin:/usr/bin:/bin", self.bin.display())))
            .stdin(Stdio::null());
        c
    }

    /// Reach `host` from this sandbox as `name`: an entry in `hosts.toml`, and a
    /// stand-in `ssh` or `wsl.exe` on this sandbox's PATH that runs the command
    /// under the host sandbox's own environment. Only the network is faked:
    /// the host has its own HOME, Thurbox database, tmux server and install.
    ///
    /// The host's PATH over the link is what a non-interactive login gives:
    /// neither `thurbox-cli` nor `thurbox-auto-continue` is on it. They are in
    /// `~/.local/bin` there, as an install leaves them, which the host's login
    /// profile adds; tmux is on it.
    pub fn add_host(&self, name: &str, host: &Sandbox, link: Link, shared: bool) {
        let sshbin = host.root.join("sshbin");
        let local_bin = host.home.join(".local/bin");
        for d in [&sshbin, &local_bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        let _ = std::os::unix::fs::symlink(on_path("tmux").unwrap(), sshbin.join("tmux"));
        let _ = std::os::unix::fs::symlink(thurbox_cli(), local_bin.join("thurbox-cli"));
        // A login shell there puts `~/.local/bin` first, as a user's profile
        // does; Thurbox reads it for what it launches on the host.
        std::fs::write(host.home.join(".profile"), "PATH=\"$HOME/.local/bin:$PATH\"\n").unwrap();
        let env = host
            .env(format!("{}:/usr/local/bin:/usr/bin:/bin", sshbin.display()))
            .iter()
            .map(|(k, v)| quote(&format!("{k}={v}")))
            .collect::<Vec<_>>()
            .join(" ");
        let log = quote(&self.root.join("link.log").display().to_string());
        let (file, script, entry) = match link {
            // ssh joins the words after the destination and the host's login
            // shell runs them.
            Link::Ssh => (
                "ssh",
                format!(
                    "#!/bin/sh\n\
                     while [ \"$#\" -gt 0 ]; do\n\
                     \x20 case \"$1\" in\n\
                     \x20   -[bcDEeFIiJLlmOoPpQRSWw]) shift 2 ;;\n\
                     \x20   -*) shift ;;\n\
                     \x20   *) break ;;\n\
                     \x20 esac\n\
                     done\n\
                     [ \"$#\" -gt 0 ] && shift\n\
                     [ \"$#\" -eq 0 ] && exit 0\n\
                     printf 'ssh %s\\n' \"$*\" >> {log}\n\
                     exec env -i {env} sh -c \"$*\"\n"
                ),
                format!("[[hosts]]\nname = \"{name}\"\ndestination = \"user@{name}\"\n"),
            ),
            // wsl.exe: `-e` hands argv over verbatim; otherwise the in-distro
            // shell reads the joined words, as over ssh.
            Link::Wsl => (
                "wsl.exe",
                format!(
                    "#!/bin/sh\n\
                     case \"$1\" in -l|--list) printf '%s\\n' {name}; exit 0 ;; esac\n\
                     while [ \"$#\" -gt 0 ]; do\n\
                     \x20 case \"$1\" in\n\
                     \x20   -d|--distribution|--cd|-u|--user) shift 2 ;;\n\
                     \x20   -e|--exec) shift; printf 'wsl %s\\n' \"$*\" >> {log}; exec env -i {env} \"$@\" ;;\n\
                     \x20   -*) shift ;;\n\
                     \x20   *) break ;;\n\
                     \x20 esac\n\
                     done\n\
                     [ \"$#\" -eq 0 ] && exit 0\n\
                     printf 'wsl %s\\n' \"$*\" >> {log}\n\
                     exec env -i {env} sh -c \"$*\"\n"
                ),
                format!("[[hosts]]\nname = \"{name}\"\nkind = \"wsl\"\ndistro = \"{name}\"\n"),
            ),
        };
        let path = self.bin.join(file);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755))
            .unwrap();
        let mut entry = entry;
        if !shared {
            // A host driven from here keeps its panes on a server this sandbox
            // names, so nothing lands on a default one.
            let socket = format!("tac-legacy-{}", self.root.file_name().unwrap().to_string_lossy());
            entry.push_str(&format!("share_sessions = false\nsocket = \"{socket}\"\n"));
            self.legacy_sockets.borrow_mut().push(socket);
        }
        let hosts = self.root.join("cfg/hosts.toml");
        let mut text = std::fs::read_to_string(&hosts).unwrap_or_default();
        text.push_str(&entry);
        std::fs::write(&hosts, text).unwrap();
    }

    /// What went over every link, one line per call.
    pub fn link_log(&self) -> String {
        std::fs::read_to_string(self.root.join("link.log")).unwrap_or_default()
    }

    /// A Claude session on `host`, created from here the way the interface
    /// does it: `session create --host`, which the host runs itself when it
    /// shares its sessions. Returns the id, the same on both sides.
    pub fn remote_session(&self, name: &str, host_name: &str, host: &Sandbox) -> String {
        let v = self.cli(&[
            "session",
            "create",
            "--name",
            name,
            "--repo-path",
            host.repo.to_str().unwrap(),
            "--agent",
            "claude",
            "--host",
            host_name,
        ]);
        let id = v["id"].as_str().unwrap().to_string();
        let pane = v["backend_id"].as_str().unwrap_or_default().to_string();
        self.wait("the fake claude to draw on the host", Duration::from_secs(30), || {
            host.screen_if_known(&id).contains("(fake)") || self.legacy_pane(&pane, &[]).contains("(fake)")
        });
        id
    }

    /// On a host that does not share its sessions, Thurbox 2.36.2 neither
    /// captures nor types from here without a CLI there, so the tests go to
    /// the pane on the server this sandbox named. `keys` are typed first.
    /// Returns the pane's text, or "".
    pub fn legacy_pane(&self, pane: &str, keys: &[&str]) -> String {
        let Some(socket) = self.legacy_sockets.borrow().first().cloned() else { return String::new() };
        if !keys.is_empty() {
            let _ = Command::new("tmux").args(["-L", &socket, "send-keys", "-t", pane]).args(keys).status();
        }
        let out = Command::new("tmux").args(["-L", &socket, "capture-pane", "-p", "-t", pane]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The pane's text when this sandbox's Thurbox knows the session, else "".
    pub fn screen_if_known(&self, id: &str) -> String {
        let out =
            self.command(self.bin.join("thurbox-cli")).args(["--json", "session", "capture", id]).output().unwrap();
        serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .and_then(|v| v["output"].as_str().map(String::from))
            .unwrap_or_default()
    }

    /// `thurbox-cli --json <args>`, which must succeed.
    pub fn cli(&self, args: &[&str]) -> Value {
        let out = self.command(self.bin.join("thurbox-cli")).arg("--json").args(args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "thurbox-cli {args:?} failed: {text}");
        serde_json::from_str(text.trim()).unwrap_or(Value::Null)
    }

    /// Our binary, with `--home` pointing at the installed extension home.
    pub fn tac(&self, args: &[&str]) -> Output {
        self.command(BIN).args(args).arg("--home").arg(&self.ext_home).output().unwrap()
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
            for f in ["received.log", "keys.log", "hooks.log", "script"] {
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
            let out = self.command(self.bin.join("thurbox-cli")).args(["--json", "runtime", "status"]).output().ok()?;
            let v: Value = serde_json::from_slice(&out.stdout).ok()?;
            v["tmux_socket"].as_str().map(String::from)
        });
        for s in socket.into_iter().chain(self.legacy_sockets.borrow().iter().cloned()) {
            let _ = Command::new("tmux").args(["-L", &s, "kill-server"]).stderr(Stdio::null()).status();
        }
    }
}

/// One POSIX shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
