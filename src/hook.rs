//! The plugin's Claude Code hooks (`claude-proxy __hook <event>`). A session the mod
//! marked (`sessions/<id>.mod`) gets no prompt news or redirect from here.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::paths::config_dir;
use crate::runs::{self, Meta, State};

/// Subagent types Claude defines itself; `--agent` only knows the user's own.
const BUILT_IN_AGENTS: &[&str] = &[
    "general-purpose",
    "Explore",
    "Plan",
    "claude-code-guide",
    "statusline-setup",
    "output-style-setup",
];

pub fn run(event: &str) -> i32 {
    // Lets the plugin's wrapper tell a binary with hooks from one without.
    if event == "supported" {
        return 0;
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Ok(v) = serde_json::from_str::<Value>(&input) else {
        return 0;
    };
    let sessions = config_dir().join("sessions");
    let speaks = policy_applies() && !mod_handles(&sessions, &v);
    match event {
        "prompt" => {
            remember_mode(&v);
            if let (true, Some(session)) = (speaks, v["session_id"].as_str()) {
                if let Some(news) = news_since_last_prompt(session) {
                    println!("{news}");
                }
            }
            0
        }
        "session-start" => {
            if let (true, Some(session)) = (policy_applies(), v["session_id"].as_str()) {
                if let Some(going) = runs_still_going(session) {
                    println!("{going}");
                }
            }
            0
        }
        "pre-tool-use" => match redirect(&v, speaks) {
            Some(message) => {
                eprintln!("{message}");
                2
            }
            None => 0,
        },
        _ => 0,
    }
}

fn remember_mode(v: &Value) {
    let (Some(session), Some(mode)) = (v["session_id"].as_str(), v["permission_mode"].as_str())
    else {
        return;
    };
    if crate::paths::valid_id(session) {
        let dir = config_dir().join("sessions");
        // ponytail: one small file per session, never pruned; prune by age if it ever matters.
        let _ = crate::paths::private_dir(&dir)
            .and_then(|()| crate::paths::write_atomic(&dir.join(session), mode.as_bytes(), 0o600));
    }
}

pub fn session_mode(session: &str) -> Option<String> {
    if !crate::paths::valid_id(session) {
        return None;
    }
    fs::read_to_string(config_dir().join("sessions").join(session))
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

fn label(m: &Meta) -> String {
    match &m.name {
        Some(name) => format!("{} ({name})", m.id),
        None => m.id.clone(),
    }
}

fn first_ask(m: &Meta) -> String {
    crate::asks::pending(&runs::run_dir(&m.id))
        .first()
        .and_then(|a| {
            crate::render::ask_lines(&m.id, &a.tool, &a.input, false)
                .into_iter()
                .next()
        })
        .unwrap_or_default()
}

fn news_since_last_prompt(session: &str) -> Option<String> {
    if !crate::paths::valid_id(session) {
        return None;
    }
    let all = runs::list();
    let mine = runs::of_session(&all, session);
    if mine.is_empty() {
        return None;
    }
    // Each finished turn is told once: remember what was told.
    let told_file = config_dir()
        .join("sessions")
        .join(format!("{session}.told"));
    let told = fs::read_to_string(&told_file).unwrap_or_default();
    let mut now_told = Vec::new();
    let lines: Vec<String> = all
        .iter()
        .filter(|m| mine.contains(&m.id))
        .filter_map(|m| match runs::effective_state(m) {
            State::Waiting => Some(format!(
                "- {} is waiting for an answer: {}",
                label(m),
                first_ask(m)
            )),
            s @ (State::Idle | State::Failed | State::Killed) => {
                let mark = format!("{} {} {}", m.id, s.as_str(), m.turns);
                let new = !told.lines().any(|l| l == mark);
                now_told.push(mark);
                new.then(|| {
                    format!(
                        "- {} is {} — `claude-proxy result {}`",
                        label(m),
                        s.as_str(),
                        m.id
                    )
                })
            }
            _ => None,
        })
        .collect();
    let _ = crate::paths::write_atomic(&told_file, now_told.join("\n").as_bytes(), 0o600);
    (!lines.is_empty()).then(|| {
        format!(
            "claude-proxy: runs this session started, since your last message:\n{}",
            lines.join("\n")
        )
    })
}

fn runs_still_going(session: &str) -> Option<String> {
    let all = runs::list();
    let mine = runs::of_session(&all, session);
    let lines: Vec<String> = all
        .iter()
        .filter(|m| mine.contains(&m.id))
        .filter_map(|m| match runs::effective_state(m) {
            State::Waiting => Some(format!(
                "- {} is waiting for an answer: {}",
                label(m),
                first_ask(m)
            )),
            s @ (State::Working | State::Queued) => {
                Some(format!("- {} is {}", label(m), s.as_str()))
            }
            _ => None,
        })
        .collect();
    (!lines.is_empty()).then(|| {
        format!(
            "claude-proxy runs this session started are still going. Waits you had on them \
             ended with your previous context: start `claude-proxy wait <id>` again in the \
             background for each one you still need.\n{}",
            lines.join("\n")
        )
    })
}

fn mod_handles(sessions: &Path, v: &Value) -> bool {
    v["session_id"].as_str().is_some_and(|s| {
        crate::paths::valid_id(s)
            && fs::read_to_string(sessions.join(format!("{s}.mod"))).is_ok_and(|m| m.trim() == "on")
    })
}

/// Not inside a run (runs do not start runs), and not turned off.
fn policy_applies() -> bool {
    std::env::var_os("CLAUDE_PROXY_RUN").is_none()
        && std::env::var("CLAUDE_PROXY_POLICY").map_or(true, |p| p != "off")
}

fn redirect(v: &Value, applies: bool) -> Option<String> {
    let tool = v["tool_name"].as_str()?;
    if !applies || !matches!(tool, "Agent" | "Task") {
        return None;
    }
    let input = &v["tool_input"];
    let description = input["description"].as_str().unwrap_or("");
    if description.trim_start().starts_with("[direct]") {
        return None;
    }
    let mut cmd = String::from("claude-proxy run");
    if let Some(agent) = input["subagent_type"]
        .as_str()
        .filter(|a| !BUILT_IN_AGENTS.contains(a))
    {
        cmd.push_str(&format!(" --agent {}", shell_quote(agent)));
    }
    if input["isolation"] == "worktree" {
        cmd.push_str(" --worktree");
    }
    if !description.is_empty() {
        cmd.push_str(&format!(" --name {}", shell_quote(description)));
    }
    cmd.push_str(" -");
    if let Some(model) = input["model"].as_str() {
        cmd.push_str(&format!(" -- --model {}", shell_quote(model)));
    }
    Some(format!(
        "The user's claude-proxy policy sends subagent work to managed runs, so this {tool} \
         call was not made. Run instead:\n\n  {cmd} <<'CLAUDE_PROXY_TASK'\n  <the same prompt>\n  CLAUDE_PROXY_TASK\n\n\
         Then run `claude-proxy wait <id>` on its own — in the background if you will be \
         notified when it ends, otherwise in the foreground — where exit code 2 means it is \
         asking you something; and tell the user the run id, its account, and \
         `claude-proxy watch <id>`. If the user said not to use claude-proxy for this, repeat \
         the {tool} call with \"[direct]\" at the start of its description."
    ))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subagent_calls_are_sent_to_runs_unless_direct() {
        let call =
            |input: Value| redirect(&json!({"tool_name": "Agent", "tool_input": input}), true);
        let message = call(
            json!({"subagent_type": "gsd-executor", "description": "fix it",
                                   "isolation": "worktree", "model": "sonnet"}),
        )
        .unwrap();
        assert!(
            message.contains(
                "claude-proxy run --agent gsd-executor --worktree --name 'fix it' - -- --model sonnet"
            ),
            "{message}"
        );
        assert!(message.contains("<<'CLAUDE_PROXY_TASK'"), "{message}");
        let builtin = call(json!({"subagent_type": "Explore", "description": "look"})).unwrap();
        assert!(
            builtin.contains("claude-proxy run --name look -"),
            "{builtin}"
        );
        assert!(call(json!({"description": "[direct] quick look"})).is_none());
        assert!(redirect(&json!({"tool_name": "Bash", "tool_input": {}}), true).is_none());
        let inside_a_run = json!({"tool_name": "Agent", "tool_input": {"description": "x"}});
        assert!(redirect(&inside_a_run, false).is_none());
    }

    #[test]
    fn the_mod_marker_quiets_the_redirect_and_the_news() {
        let dir = tempfile::tempdir().unwrap();
        let call = json!({"session_id": "s-1", "tool_name": "Agent",
                          "tool_input": {"description": "look"}});
        assert!(!mod_handles(dir.path(), &call));
        assert!(redirect(&call, !mod_handles(dir.path(), &call)).is_some());

        fs::write(dir.path().join("s-1.mod"), "on").unwrap();
        assert!(mod_handles(dir.path(), &call));
        assert!(redirect(&call, !mod_handles(dir.path(), &call)).is_none());

        fs::write(dir.path().join("s-1.mod"), "off").unwrap();
        assert!(!mod_handles(dir.path(), &call));
        let escape = json!({"session_id": "../s-1"});
        assert!(!mod_handles(&dir.path().join("x"), &escape));
    }
}
