//! Renders a run's `events.jsonl` one line at a time, so `--follow` can render incrementally.

use serde_json::Value;

const ONE_LINE_MAX: usize = 140;

/// `full` keeps tool inputs, output and messages whole; otherwise one line each.
pub fn render_line(line: &str, full: bool) -> Vec<String> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![format!("  ! {}", one_line(line))];
    };
    let str_of = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("");
    match str_of("type") {
        "claude_proxy" => marker(&v, full),
        "assistant" => blocks(&v)
            .iter()
            .flat_map(|b| assistant_block(b, full))
            .collect(),
        "user" => blocks(&v)
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .flat_map(|b| tool_result(b, full))
            .collect(),
        "result" => vec![result_line(&v)],
        "rate_limit_event" => rate_limit(&v).into_iter().collect(),
        "system" if full && str_of("subtype") == "post_turn_summary" => {
            let detail = str_of("status_detail");
            if detail.is_empty() {
                Vec::new()
            } else {
                vec![format!("  ≡ {detail}")]
            }
        }
        _ => Vec::new(),
    }
}

fn marker(v: &Value, full: bool) -> Vec<String> {
    match v.get("event").and_then(Value::as_str) {
        Some("message") => {
            let text = v.get("text").and_then(Value::as_str).unwrap_or("");
            let mut out = vec![if v["during_turn"] == true {
                "▶ you (while it works)".to_string()
            } else {
                "▶ you".to_string()
            }];
            out.extend(indented(text, "  ", full));
            out
        }
        Some("turn_start") => vec![format!(
            "● {} · turn {}",
            v.get("account").and_then(Value::as_str).unwrap_or("?"),
            v.get("turn").and_then(Value::as_u64).unwrap_or(0)
        )],
        Some("failover") => {
            let from = v.get("from").and_then(Value::as_str).unwrap_or("?");
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or("failed");
            match v.get("to").and_then(Value::as_str) {
                Some(to) => vec![format!("⇄ {from} {reason} — continuing on {to}")],
                None => vec![format!(
                    "⇄ {from} {reason} — no other account to continue on"
                )],
            }
        }
        Some("ask") => ask_lines(
            v.get("run").and_then(Value::as_str).unwrap_or("<id>"),
            v.get("tool").and_then(Value::as_str).unwrap_or("tool"),
            v.get("input").unwrap_or(&Value::Null),
            full,
        ),
        Some("answer") => match (v["behavior"].as_str(), v["answers"].as_object()) {
            (Some("allow"), Some(answers)) if !answers.is_empty() => answers
                .iter()
                .map(|(q, a)| format!("  ↪ {q} → {}", a.as_str().unwrap_or("")))
                .collect(),
            (Some("allow"), _) => vec![format!("  ↪ allowed{}", granted(&v["granted"]))],
            _ => vec![format!(
                "  ↪ denied: {}",
                v["message"].as_str().unwrap_or("")
            )],
        },
        Some("note") => vec![format!(
            "  · {}",
            v.get("text").and_then(Value::as_str).unwrap_or("")
        )],
        Some("turn_end") => {
            let killed = v.get("killed").and_then(Value::as_bool).unwrap_or(false);
            match v.get("exit").and_then(Value::as_i64) {
                _ if killed => vec!["■ killed".into()],
                Some(0) => Vec::new(),
                Some(code) => vec![format!("✗ claude exited with code {code}")],
                None => match v.get("error").and_then(Value::as_str) {
                    Some(e) => vec![format!("✗ {e}")],
                    None => vec!["✗ claude was stopped by a signal".into()],
                },
            }
        }
        _ => Vec::new(),
    }
}

fn granted(updates: &Value) -> String {
    let parts: Vec<String> = updates
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| match u["type"].as_str() {
            Some("setMode") if u["mode"] == "acceptEdits" => Some("accepting edits".to_string()),
            Some("addRules") => u["rules"].as_array().map(|rules| {
                rules
                    .iter()
                    .map(|r| match r["ruleContent"].as_str() {
                        Some(c) => format!("{}({c})", r["toolName"].as_str().unwrap_or("")),
                        None => r["toolName"].as_str().unwrap_or("").to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · from now on: {}", parts.join(", "))
    }
}

pub fn ask_lines(id: &str, tool: &str, input: &Value, full: bool) -> Vec<String> {
    if tool == "AskUserQuestion" {
        let mut out: Vec<String> = input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| {
                let options: Vec<&str> = q["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o.as_str().or_else(|| o["label"].as_str()))
                    .collect();
                let mut line = format!("? {}", q["question"].as_str().unwrap_or(""));
                if !options.is_empty() {
                    line.push_str(&format!("  [{}]", options.join(" | ")));
                }
                if q["multiSelect"].as_bool() == Some(true) {
                    line.push_str("  (any of them)");
                }
                line
            })
            .collect();
        let slots = vec!["\"…\""; out.len().max(1)].join(" ");
        out.push(format!(
            "    claude-proxy answer {id} {slots}   or let it decide:  claude-proxy deny {id} \"decide yourself\""
        ));
        return out;
    }
    let mut out = vec![format!(
        "? permission to use {tool}  {}",
        tool_summary(tool, input)
    )];
    if full {
        let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
        let mut lines = indented(&pretty, "      ", true);
        if lines.len() > 40 {
            let more = lines.len() - 40;
            lines.truncate(40);
            lines.push(format!(
                "      (+{more} lines: claude-proxy status {id} --json)"
            ));
        }
        out.extend(lines);
    }
    let always = if tool == "ExitPlanMode" {
        format!("claude-proxy allow {id} --accept-edits")
    } else {
        format!("claude-proxy allow {id} --always")
    };
    out.push(format!(
        "    claude-proxy allow {id}   or   {always}   or   claude-proxy deny {id} \"<reason>\""
    ));
    out
}

fn blocks(v: &Value) -> Vec<Value> {
    v.pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn assistant_block(b: &Value, full: bool) -> Vec<String> {
    match b.get("type").and_then(Value::as_str) {
        Some("text") => indented(
            b.get("text").and_then(Value::as_str).unwrap_or(""),
            "  ",
            true,
        ),
        Some("tool_use") => {
            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
            let input = b.get("input").cloned().unwrap_or(Value::Null);
            if full {
                let pretty = serde_json::to_string_pretty(&input).unwrap_or_default();
                let mut out = vec![format!("  ⚙ {name}")];
                out.extend(indented(&pretty, "      ", true));
                out
            } else {
                vec![format!("  ⚙ {name}  {}", tool_summary(name, &input))]
            }
        }
        _ => Vec::new(),
    }
}

pub fn tool_summary(name: &str, input: &Value) -> String {
    let pick = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_string);
    let picked = match name {
        "Bash" => pick("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => pick("file_path"),
        "Grep" | "Glob" => pick("pattern"),
        "WebFetch" => pick("url"),
        "WebSearch" => pick("query"),
        "Agent" | "Task" => pick("description"),
        "Skill" => pick("skill"),
        _ => None,
    };
    one_line(&picked.unwrap_or_else(|| input.to_string()))
}

fn tool_result(b: &Value, full: bool) -> Vec<String> {
    let text = match b.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    let arrow = if error { "    ✗ " } else { "    ↳ " };
    if full {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        let mut out = vec![format!("{arrow}{first}")];
        out.extend(lines.map(|l| format!("      {l}")));
        return out;
    }
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("(no output)");
    let rest = lines.count();
    let more = if rest > 0 {
        format!("  (+{rest} lines)")
    } else {
        String::new()
    };
    vec![format!("{arrow}{}{more}", one_line(first))]
}

fn result_line(v: &Value) -> String {
    let ok = v.get("subtype").and_then(Value::as_str) == Some("success")
        && !v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    // An API error arrives as subtype `success` with `is_error`; its text was
    // already shown as the assistant's message.
    let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("error");
    let mut parts = vec![if ok {
        "✓ done".to_string()
    } else if subtype == "success" {
        "✗ failed".to_string()
    } else {
        format!("✗ {subtype}")
    }];
    if let Some(n) = v.get("num_turns").and_then(Value::as_u64) {
        parts.push(format!("{n} steps"));
    }
    if let Some(ms) = v.get("duration_ms").and_then(Value::as_f64) {
        parts.push(format!("{:.0}s", ms / 1000.0));
    }
    if let Some(cost) = v.get("total_cost_usd").and_then(Value::as_f64) {
        parts.push(format!("${cost:.2}"));
    }
    let mut line = parts.join(" · ");
    if !ok && subtype != "success" {
        if let Some(text) = v.get("result").and_then(Value::as_str) {
            line.push_str(&format!("\n  {}", one_line(text)));
        }
    }
    line
}

fn rate_limit(v: &Value) -> Option<String> {
    let info = v.get("rate_limit_info")?;
    let status = info.get("status").and_then(Value::as_str).unwrap_or("");
    if status.is_empty() || status == "allowed" {
        return None;
    }
    let kind = info
        .get("rateLimitType")
        .and_then(Value::as_str)
        .unwrap_or("usage");
    let used = info
        .get("utilization")
        .and_then(Value::as_f64)
        .map(|u| format!(" {:.0}%", u * 100.0))
        .unwrap_or_default();
    Some(format!("  ⚠ rate limit: {kind}{used} ({status})"))
}

fn indented(text: &str, prefix: &str, full: bool) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|l| format!("{prefix}{l}")).collect();
    if !full && lines.len() > 3 {
        let more = lines.len() - 3;
        lines.truncate(3);
        lines.push(format!("{prefix}(+{more} lines)"));
    }
    lines
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > ONE_LINE_MAX {
        let cut: String = flat.chars().take(ONE_LINE_MAX).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(line: &str) -> Vec<String> {
        render_line(line, false)
    }

    #[test]
    fn renders_messages_turns_tools_and_results() {
        assert_eq!(
            r(r#"{"type":"claude_proxy","event":"message","text":"fix the tests"}"#),
            ["▶ you", "  fix the tests"]
        );
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"turn_start","turn":2,"account":"claude-personal"}"#
            ),
            ["● claude-personal · turn 2"]
        );
        assert_eq!(
            r(r#"{"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":""},
                {"type":"tool_use","name":"Bash","input":{"command":"cargo test","description":"x"}},
                {"type":"text","text":"All green."}]}}"#),
            ["  ⚙ Bash  cargo test", "  All green."]
        );
        assert_eq!(
            r(r#"{"type":"user","message":{"content":[
                {"type":"tool_result","content":"line one\nline two\n\nline three","is_error":false}]}}"#),
            ["    ↳ line one  (+2 lines)"]
        );
        assert_eq!(
            r(
                r#"{"type":"result","subtype":"success","is_error":false,"num_turns":3,
                "duration_ms":42000,"total_cost_usd":0.256,"result":"done"}"#
            ),
            ["✓ done · 3 steps · 42s · $0.26"]
        );
    }

    #[test]
    fn failures_kills_and_noise() {
        assert_eq!(
            r(r#"{"type":"claude_proxy","event":"turn_end","exit":null,"killed":true}"#),
            ["■ killed"]
        );
        assert_eq!(
            r(r#"{"type":"claude_proxy","event":"turn_end","exit":1,"killed":false}"#),
            ["✗ claude exited with code 1"]
        );
        assert!(r(r#"{"type":"claude_proxy","event":"turn_end","exit":0}"#).is_empty());
        assert_eq!(
            r(r#"{"type":"result","subtype":"success","is_error":true,
                "result":"Not logged in · Please run /login"}"#),
            ["✗ failed"]
        );
        assert_eq!(
            r(
                r#"{"type":"result","subtype":"error_max_turns","is_error":true,
                "result":"stopped after 10 turns"}"#
            ),
            ["✗ error_max_turns\n  stopped after 10 turns"]
        );
        assert!(r(r#"{"type":"system","subtype":"hook_started"}"#).is_empty());
        assert_eq!(
            r("Not logged in · Please run /login"),
            ["  ! Not logged in · Please run /login"]
        );
        assert_eq!(
            r(
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning",
                "rateLimitType":"seven_day","utilization":0.97}}"#
            ),
            ["  ⚠ rate limit: seven_day 97% (allowed_warning)"]
        );
        assert!(
            r(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#).is_empty()
        );
    }

    #[test]
    fn failover_markers() {
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"failover","from":"a","to":"b",
                "reason":"hit its usage limit"}"#
            ),
            ["⇄ a hit its usage limit — continuing on b"]
        );
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"failover","from":"a","to":null,
                "reason":"could not sign in"}"#
            ),
            ["⇄ a could not sign in — no other account to continue on"]
        );
    }

    #[test]
    fn asks_and_answers() {
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"ask","run":"abc123","tool":"AskUserQuestion",
                "input":{"questions":[{"question":"Which colour?","options":[{"label":"Red"},{"label":"Blue"}],
                "multiSelect":false}]}}"#
            ),
            [
                "? Which colour?  [Red | Blue]",
                "    claude-proxy answer abc123 \"…\"   or let it decide:  claude-proxy deny abc123 \"decide yourself\""
            ]
        );
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"ask","run":"abc123","tool":"Bash",
                "input":{"command":"cargo publish"}}"#
            ),
            [
                "? permission to use Bash  cargo publish",
                "    claude-proxy allow abc123   or   claude-proxy allow abc123 --always   or   claude-proxy deny abc123 \"<reason>\""
            ]
        );
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"answer","behavior":"allow",
                "answers":{"Which colour?":"blue"}}"#
            ),
            ["  ↪ Which colour? → blue"]
        );
        assert_eq!(
            r(r#"{"type":"claude_proxy","event":"answer","behavior":"deny","message":"no"}"#),
            ["  ↪ denied: no"]
        );
        assert_eq!(
            r(
                r#"{"type":"claude_proxy","event":"answer","behavior":"allow","granted":[
                {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cargo test:*"}]},
                {"type":"setMode","mode":"acceptEdits"}]}"#
            ),
            ["  ↪ allowed · from now on: Bash(cargo test:*), accepting edits"]
        );
    }

    #[test]
    fn full_mode_keeps_everything() {
        let out = render_line(
            r#"{"type":"user","message":{"content":[
                {"type":"tool_result","content":[{"type":"text","text":"a\nb"}],"is_error":true}]}}"#,
            true,
        );
        assert_eq!(out, ["    ✗ a", "      b"]);
    }

    #[test]
    fn tool_summary_picks_the_telling_argument() {
        let input = serde_json::json!({"file_path": "/x/y.rs", "old_string": "a"});
        assert_eq!(tool_summary("Edit", &input), "/x/y.rs");
        let other = serde_json::json!({"k": 1});
        assert_eq!(tool_summary("Mystery", &other), r#"{"k":1}"#);
    }
}
