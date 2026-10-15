//! Tests of the plugin's hook scripts and their wiring to the binary.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const SESSION: &str = r#"{"session_id":"55555555-5555-4555-8555-555555555555","source":"startup"}"#;

/// Stands in for claude-proxy: logs its arguments, answers `__hook supported`
/// with `$STUB_SUPPORTED`, otherwise echoes the event and stdin and exits `$STUB_EXIT`.
const STUB: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_LOG"
[ "$2" = supported ] && exit "${STUB_SUPPORTED:-0}"
echo "hook $2"
cat
exit "${STUB_EXIT:-0}"
"#;

fn sh(tmp: &Path, args: &[&str], path: &str, vars: &[(&str, &str)], input: &str) -> Output {
    let home = tmp.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(args)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", path)
        .current_dir(tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    // A script that exits before reading closes the pipe.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

fn script(name: &str) -> String {
    format!("{ROOT}/hooks/{name}")
}

fn real_bin(tmp: &Path) -> String {
    let bin = tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The binary dispatches on argv[0], so the name must be exactly this.
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_claude-proxy"), bin.join("claude-proxy"))
        .unwrap();
    format!("{}:/usr/bin:/bin", bin.display())
}

fn stub_bin(tmp: &Path) -> (String, PathBuf) {
    let dir = tmp.join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("claude-proxy");
    std::fs::write(&stub, STUB).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    (
        format!("{}:/usr/bin:/bin", dir.display()),
        tmp.join("stub.log"),
    )
}

fn empty_path(tmp: &Path) -> String {
    let dir = tmp.join("empty");
    std::fs::create_dir_all(&dir).unwrap();
    dir.display().to_string()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn log_lines(log: &Path) -> Vec<String> {
    text(&std::fs::read(log).unwrap())
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_plugin_hooks_reach_the_real_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let path = real_bin(tmp.path());
    let hooks: Value =
        serde_json::from_slice(&std::fs::read(format!("{ROOT}/hooks/hooks.json")).unwrap())
            .unwrap();
    let run = |event: &str, input: &str| {
        let command = hooks["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        sh(
            tmp.path(),
            &["-c", command],
            &path,
            &[("CLAUDE_PLUGIN_ROOT", ROOT)],
            input,
        )
    };

    let out = run("SessionStart", SESSION);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("claude-proxy is installed."),
        "{}",
        text(&out.stdout)
    );

    let out = run(
        "UserPromptSubmit",
        r#"{"session_id":"55555555-5555-4555-8555-555555555555","permission_mode":"default"}"#,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));

    // Exit 2 also shows the `__hook supported` probe left stdin for the real call.
    let out = run(
        "PreToolUse",
        r#"{"tool_name":"Agent","tool_input":{"subagent_type":"gsd-executor","description":"fix it","prompt":"p"}}"#,
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("claude-proxy run --agent gsd-executor --name 'fix it' -"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn policy_is_silent_inside_a_run_when_turned_off_or_without_claude_proxy() {
    for vars in [
        &[("CLAUDE_PROXY_RUN", "abc123")][..],
        &[("CLAUDE_PROXY_POLICY", "off")][..],
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (path, log) = stub_bin(tmp.path());
        let log_var = log.display().to_string();
        let vars = [vars, &[("STUB_LOG", log_var.as_str())]].concat();
        let out = sh(tmp.path(), &[&script("policy.sh")], &path, &vars, SESSION);
        assert_eq!(out.status.code(), Some(0), "{vars:?}");
        assert!(out.stdout.is_empty(), "{vars:?}: {}", text(&out.stdout));
        assert!(!log.exists(), "{vars:?} still called claude-proxy");
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = empty_path(tmp.path());
    let out = sh(tmp.path(), &[&script("policy.sh")], &path, &[], SESSION);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
}

#[test]
fn policy_prints_then_lists_runs_only_with_a_claude_proxy_that_has_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, log) = stub_bin(tmp.path());
    let log_var = log.display().to_string();
    let out = sh(
        tmp.path(),
        &[&script("policy.sh")],
        &path,
        &[("STUB_LOG", &log_var)],
        SESSION,
    );
    assert_eq!(out.status.code(), Some(0));
    let printed = text(&out.stdout);
    assert!(
        printed.starts_with("claude-proxy is installed."),
        "{printed}"
    );
    assert!(
        printed.ends_with(&format!("hook session-start\n{SESSION}")),
        "{printed}"
    );
    assert_eq!(
        log_lines(&log),
        ["__hook supported", "__hook session-start"]
    );

    std::fs::remove_file(&log).unwrap();
    let out = sh(
        tmp.path(),
        &[&script("policy.sh")],
        &path,
        &[("STUB_LOG", &log_var), ("STUB_SUPPORTED", "1")],
        SESSION,
    );
    assert_eq!(out.status.code(), Some(0));
    let printed = text(&out.stdout);
    assert!(
        printed.starts_with("claude-proxy is installed."),
        "{printed}"
    );
    assert!(!printed.contains("hook session-start"), "{printed}");
    assert_eq!(log_lines(&log), ["__hook supported"]);
}

#[test]
fn the_wrapper_skips_a_missing_or_hookless_claude_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = script("claude-proxy-hook.sh");
    let out = sh(
        tmp.path(),
        &[&wrapper, "prompt"],
        &empty_path(tmp.path()),
        &[],
        SESSION,
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());

    let (path, log) = stub_bin(tmp.path());
    let log_var = log.display().to_string();
    let out = sh(
        tmp.path(),
        &[&wrapper, "prompt"],
        &path,
        &[("STUB_LOG", &log_var), ("STUB_SUPPORTED", "1")],
        SESSION,
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(log_lines(&log), ["__hook supported"]);
}

#[test]
fn the_wrapper_passes_the_event_input_and_exit_code_through() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, log) = stub_bin(tmp.path());
    let log_var = log.display().to_string();
    let input = r#"{"tool_name":"Agent"}"#;
    let out = sh(
        tmp.path(),
        &[&script("claude-proxy-hook.sh"), "pre-tool-use"],
        &path,
        &[("STUB_LOG", &log_var), ("STUB_EXIT", "2")],
        input,
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), format!("hook pre-tool-use\n{input}"));
    assert_eq!(log_lines(&log), ["__hook supported", "__hook pre-tool-use"]);
}

#[test]
fn the_hook_command_ignores_bad_input_and_unknown_events() {
    let tmp = tempfile::tempdir().unwrap();
    let path = real_bin(tmp.path());
    let out = sh(
        tmp.path(),
        &["-c", "claude-proxy __hook prompt"],
        &path,
        &[],
        "not json",
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));

    let out = sh(
        tmp.path(),
        &["-c", "claude-proxy __hook nope"],
        &path,
        &[],
        "{}",
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}
