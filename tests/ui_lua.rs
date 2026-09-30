//! The Thurbox plugin's Lua suites (tests/ui/test_*.lua), run by `cargo test`
//! so the one gate covers them: the pure model, the pane and the badge, each
//! against the real `lib/` of the thurbox-cli under test (see tests/ui/run.sh).

use std::path::Path;
use std::process::Command;

fn suite(name: &str) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui/run.sh");
    let mut cmd = Command::new("sh");
    cmd.arg(&script).arg(name).env("CARGO_TARGET_DIR", env!("CARGO_TARGET_TMPDIR"));
    if let Some(lua) = std::env::var_os("TAC_LUA") {
        cmd.env("LUA", lua);
    }
    let out = cmd.output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "tests/ui/run.sh {name} failed:\n{text}");
}

#[test]
fn lua_model() {
    suite("model");
}

#[test]
fn lua_pane() {
    suite("pane");
}

#[test]
fn lua_badge() {
    suite("badge");
}
