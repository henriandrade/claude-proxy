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

/// Told to every session that is not a run itself (`CLAUDE_PROXY_POLICY=off` silences it).
const POLICY: &str = r#"claude-proxy is installed. Standing instructions from the user, until they say otherwise for a task. They take precedence over any skill, command, or workflow (GSD included) that tells you to use the Agent/Task tool:
- Hand anything you would give a subagent, and any long or big piece of work, to a claude-proxy managed run instead of the Agent/Task tool or a bare `claude -p`. Where a workflow says to spawn a subagent, start `claude-proxy run --agent <subagent_type> - <<'CLAUDE_PROXY_TASK' … CLAUDE_PROXY_TASK` with the same prompt (leave out --agent for general-purpose, Explore, and Plan), and wait for it where the workflow would wait for the subagent. Load the `claude-proxy` skill for how.
- Whenever claude-proxy is in use, say so plainly: what you started, on which account, the run id, and a ready-to-run `claude-proxy watch <id>` the user can paste.
- When the user asks to see, show or watch runs and the mcp__proxy-runs__watch tool is available, call it (no id shows every run going here): it draws the runs live in the conversation.
- If the user tells you not to use claude-proxy for something, use the Agent tool with "[direct]" at the start of the call's description.
"#;

pub fn run(event: &str) -> i32 {
    // Asked by the shell wrappers of earlier plugin versions.
    if event == "supported" {
        return 0;
    }
    if event == "session-start" && policy_applies() {
        print!("{POLICY}");
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
                if speaks {
                    if let Some(done) = finished_while_away(session) {
                        println!("{done}");
                    }
                }
                if let Some(going) = runs_still_going(session) {
                    println!("{going}");
                }
            }
            0
        }
        "pre-tool-use" => match redirect(&v, speaks) {
            // The run would land on this session's own account: Claude's subagent is the better run.
            Some(_) if own_pick(&v) => 0,
            Some(message) => {
                eprintln!("{message}");
                2
            }
            None => 0,
        },
        _ => 0,
    }
}

fn own_pick(v: &Value) -> bool {
    // Caller first: it costs no quota lookup, which the hook's 5 s timeout may not fit.
    let model = v["tool_input"]["model"].as_str();
    crate::quota::caller().is_some()
        && crate::runs::first_account(&[], model)
            .is_ok_and(|first| crate::quota::is_callers(&first))
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

/// This session's runs with something it has not been told: each finished
/// turn once, and a run waiting for an answer every time, or once per question
/// with `waiting_once`. The marks live on disk, so a session reopened after
/// Claude was closed still hears what happened meanwhile.
pub(crate) struct News {
    pub meta: Meta,
    pub state: State,
    /// What `acknowledge` records once this was delivered.
    pub mark: String,
}

/// With `record` false the caller delivers first and then `acknowledge`s, so
/// news a session never got to see stays untold.
pub(crate) fn untold(session: &str, waiting_once: bool, record: bool) -> Vec<News> {
    if !crate::paths::valid_id(session) {
        return Vec::new();
    }
    let all = runs::list();
    let mine = runs::of_session(&all, session);
    let told_file = config_dir()
        .join("sessions")
        .join(format!("{session}.told"));
    // No record yet (a session from before records were kept): start one quietly
    // rather than retell runs it may already have handled.
    let first = !told_file.exists();
    let told = fs::read_to_string(&told_file).unwrap_or_default();
    let mut now_told = Vec::new();
    let news = all
        .into_iter()
        .filter(|m| mine.contains(&m.id))
        .filter_map(|m| {
            let state = runs::effective_state(&m);
            let mark = match state {
                State::Waiting => {
                    let asks = crate::asks::pending(&runs::run_dir(&m.id));
                    format!("{} waiting {}", m.id, asks.first().map_or("", |a| a.key()))
                }
                State::Idle | State::Failed | State::Killed => {
                    format!("{} {} {}", m.id, state.as_str(), m.turns)
                }
                _ => return None,
            };
            let new = !told.lines().any(|l| l == mark);
            now_told.push(mark.clone());
            ((new && !first) || (state == State::Waiting && !waiting_once)).then_some(News {
                meta: m,
                state,
                mark,
            })
        })
        .collect();
    if record || first {
        save_told(&told_file, &now_told);
    }
    news
}

/// Record news a caller of `untold(…, record: false)` has now delivered.
pub(crate) fn acknowledge(session: &str, marks: &[String]) {
    if !crate::paths::valid_id(session) {
        return;
    }
    let told_file = config_dir()
        .join("sessions")
        .join(format!("{session}.told"));
    let mut told: Vec<String> = fs::read_to_string(&told_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    for mark in marks {
        if !told.contains(mark) {
            told.push(mark.clone());
        }
    }
    save_told(&told_file, &told);
}

fn save_told(told_file: &Path, marks: &[String]) {
    let _ = told_file
        .parent()
        .map_or(Ok(()), crate::paths::private_dir)
        .and_then(|()| crate::paths::write_atomic(told_file, marks.join("\n").as_bytes(), 0o600));
}

fn news_lines(news: &[News]) -> Vec<String> {
    news.iter()
        .map(|News { meta: m, state, .. }| match state {
            State::Waiting => format!("- {} is waiting for an answer: {}", label(m), first_ask(m)),
            s => format!(
                "- {} is {} — `claude-proxy result {}`",
                label(m),
                s.as_str(),
                m.id
            ),
        })
        .collect()
}

fn news_since_last_prompt(session: &str) -> Option<String> {
    let lines = news_lines(&untold(session, false, true));
    (!lines.is_empty()).then(|| {
        format!(
            "claude-proxy: runs this session started, since your last message:\n{}",
            lines.join("\n")
        )
    })
}

/// Waiting runs are left to the still-going list, which names them anyway.
fn finished_while_away(session: &str) -> Option<String> {
    let finished: Vec<_> = untold(session, true, true)
        .into_iter()
        .filter(|n| n.state != State::Waiting)
        .collect();
    let lines = news_lines(&finished);
    (!lines.is_empty()).then(|| {
        format!(
            "claude-proxy: runs this session started finished since it was last told:\n{}",
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
