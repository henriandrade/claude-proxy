//! Tests of the plugin's hooks: each `hooks.json` command is `claude-proxy __hook
//! <event>`, run here as Claude Code would, without a shell.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const SESSION: &str = r#"{"session_id":"55555555-5555-4555-8555-555555555555","source":"startup"}"#;

/// The argv of the plugin's command for `event`, checked to need no shell.
fn command(event: &str) -> Vec<String> {
    let hooks: Value =
        serde_json::from_slice(&std::fs::read(format!("{ROOT}/hooks/hooks.json")).unwrap())
            .unwrap();
    let line = hooks["hooks"][event][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        !line.contains(['$', '"', '\'', '|', '&', ';', '<', '>', '%']),
        "{event}: {line:?} needs a shell, and Windows may not run one"
    );
    let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    assert_eq!(argv[..2], ["claude-proxy", "__hook"], "{line}");
    argv
}

fn hook(tmp: &Path, argv: &[String], vars: &[(&str, &str)], input: &str) -> Output {
    let home = tmp.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude-proxy"));
    cmd.args(&argv[1..])
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir(tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = std::env::var_os("SystemRoot") {
        cmd.env("SystemRoot", root);
    }
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    // A hook that exits before reading closes the pipe.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_plugin_hooks_reach_the_binary() {
    let tmp = tempfile::tempdir().unwrap();

    let out = hook(tmp.path(), &command("SessionStart"), &[], SESSION);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("claude-proxy is installed."),
        "{}",
        text(&out.stdout)
    );

    let out = hook(
        tmp.path(),
        &command("UserPromptSubmit"),
        &[],
        r#"{"session_id":"55555555-5555-4555-8555-555555555555","permission_mode":"default"}"#,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));

    let out = hook(
        tmp.path(),
        &command("PreToolUse"),
        &[],
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
fn policy_is_silent_inside_a_run_or_when_turned_off() {
    for vars in [
        &[("CLAUDE_PROXY_RUN", "abc123")][..],
        &[("CLAUDE_PROXY_POLICY", "off")][..],
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let out = hook(tmp.path(), &command("SessionStart"), vars, SESSION);
        assert_eq!(out.status.code(), Some(0), "{vars:?}");
        assert!(out.stdout.is_empty(), "{vars:?}: {}", text(&out.stdout));
    }
}

#[test]
fn policy_does_not_depend_on_the_hook_input() {
    let tmp = tempfile::tempdir().unwrap();
    let out = hook(tmp.path(), &command("SessionStart"), &[], "not json");
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with("claude-proxy is installed."));
}

#[test]
fn the_hook_command_ignores_bad_input_and_unknown_events() {
    let tmp = tempfile::tempdir().unwrap();
    let out = hook(tmp.path(), &command("UserPromptSubmit"), &[], "not json");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));

    for event in ["nope", "supported"] {
        let argv = ["claude-proxy", "__hook", event].map(String::from);
        let out = hook(tmp.path(), &argv, &[], "{}");
        assert_eq!(out.status.code(), Some(0), "{event}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{event}");
    }
}
