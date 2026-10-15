//! Decisions a run cannot make alone go through the `__permit` MCP server, which
//! records `asks/<key>.json` and waits for `allow`, `deny` or `answer`.

use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::quota::now_secs;
use crate::runs::{self, marker, run_dir};

pub const SERVER: &str = "claude-proxy";
pub const PROMPT_TOOL: &str = "mcp__claude-proxy__permit";

pub struct Ask {
    pub tool: String,
    pub input: Value,
    key: String,
}

impl Ask {
    pub fn is_question(&self) -> bool {
        self.tool == "AskUserQuestion"
    }

    pub fn questions(&self) -> Vec<String> {
        self.input
            .get("questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|q| q.get("question").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }
}

/// Only meaningful while the run is alive: the drainer clears asks when Claude
/// exits.
pub fn pending(dir: &Path) -> Vec<Ask> {
    let asks = dir.join("asks");
    let mut keys: Vec<String> = fs::read_dir(&asks)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some(e.file_name().to_str()?.strip_suffix(".json")?.to_string()))
        .filter(|k| !asks.join(format!("{k}.answer")).exists())
        .collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|key| {
            let v: Value =
                serde_json::from_slice(&fs::read(asks.join(format!("{key}.json"))).ok()?).ok()?;
            Some(Ask {
                tool: v["tool"].as_str().unwrap_or("tool").to_string(),
                input: v["input"].clone(),
                key,
            })
        })
        .collect()
}

pub fn clear(dir: &Path) {
    let _ = fs::remove_dir_all(dir.join("asks"));
}

pub enum Reply {
    Allow(Grant),
    Deny(String),
    Answer(Vec<String>),
}

#[derive(Default)]
pub struct Grant {
    /// Stop asking for requests like this one.
    pub always: bool,
    pub rule: Option<String>,
    pub accept_edits: bool,
}

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Answers the oldest pending ask. Returns it and what was granted for good, in words.
pub fn reply(id: &str, reply: Reply) -> Result<(Ask, Vec<String>), String> {
    let meta = runs::load(id)?;
    let dir = run_dir(id);
    let ask = pending(&dir)
        .into_iter()
        .next()
        .filter(|_| runs::alive(&dir))
        .ok_or_else(|| {
            format!("run {id} is not waiting for an answer. See:  claude-proxy status {id}")
        })?;
    let mut granted = Vec::new();
    let answer = match reply {
        Reply::Allow(grant) => {
            let updates = grant_updates(&dir, &meta, &ask, grant, &mut granted)?;
            let mut answer = json!({"behavior": "allow"});
            if !updates.is_empty() {
                answer["updatedPermissions"] = Value::Array(updates);
            }
            answer
        }
        Reply::Deny(message) => json!({"behavior": "deny", "message": message}),
        Reply::Answer(texts) => {
            if !ask.is_question() {
                return Err(format!(
                    "run {id} is asking to use {}, not asking a question: \
                     claude-proxy allow {id}  or  claude-proxy deny {id} \"<reason>\"",
                    ask.tool
                ));
            }
            let questions = ask.questions();
            if texts.len() != questions.len() {
                return Err(format!(
                    "run {id} asked {} question(s); give one answer each, in order:\n{}",
                    questions.len(),
                    questions
                        .iter()
                        .map(|q| format!("  {q}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            let answers: Map<String, Value> = questions
                .into_iter()
                .zip(texts.into_iter().map(Value::String))
                .collect();
            json!({"behavior": "allow", "answers": answers})
        }
    };
    write_json(
        &dir.join("asks").join(format!("{}.answer", ask.key)),
        &answer,
    )
    .map_err(|e| format!("could not answer run {id}: {e}"))?;
    Ok((ask, granted))
}

/// Updates for the live Claude process, after recording them for later processes.
fn grant_updates(
    dir: &Path,
    meta: &runs::Meta,
    ask: &Ask,
    grant: Grant,
    granted: &mut Vec<String>,
) -> Result<Vec<Value>, String> {
    let edit = EDIT_TOOLS.contains(&ask.tool.as_str());
    let rule = match grant.rule {
        Some(rule) => Some(rule),
        None if grant.always && !edit => Some(rule_for(&ask.tool, &ask.input).map_err(|why| {
            format!(
                "run {}: {why}. Allow it once:  claude-proxy allow {0}\n\
                 or choose the rule yourself:  claude-proxy allow {0} --rule \"Bash(…)\"",
                meta.id
            )
        })?),
        None => None,
    };
    let mut updates = Vec::new();
    // Native Claude's "don't ask again" for an edit is accepting edits.
    let mode = if grant.accept_edits || (grant.always && rule.is_none() && edit) {
        Some("acceptEdits")
    } else if ask.tool == "ExitPlanMode" && started_in_plan_mode(meta) {
        // Approved: later processes must not restart in plan mode.
        Some("default")
    } else {
        None
    };
    if let Some(mode) = mode {
        crate::paths::write_atomic(&dir.join("mode"), mode.as_bytes(), 0o600)
            .map_err(|e| format!("could not save the mode: {e}"))?;
        updates.push(json!({"type": "setMode", "mode": mode, "destination": "session"}));
        if mode == "acceptEdits" {
            granted.push("accepting edits".into());
        }
    }
    if let Some(rule) = rule {
        let mut rules = allowed_rules(dir);
        if !rules.contains(&rule) {
            rules.push(rule.clone());
        }
        write_json(&dir.join("allowed"), &json!(rules))
            .map_err(|e| format!("could not save the rule: {e}"))?;
        let (tool, content) = match rule.split_once('(') {
            Some((tool, rest)) => (tool, rest.strip_suffix(')')),
            None => (rule.as_str(), None),
        };
        let mut entry = json!({"toolName": tool});
        if let Some(content) = content {
            entry["ruleContent"] = json!(content);
        }
        updates.push(json!({"type": "addRules", "rules": [entry],
                            "behavior": "allow", "destination": "session"}));
        granted.push(rule);
    }
    Ok(updates)
}

fn started_in_plan_mode(meta: &runs::Meta) -> bool {
    meta.claude_args
        .windows(2)
        .any(|w| w[0] == "--permission-mode" && w[1] == "plan")
        || meta
            .claude_args
            .iter()
            .any(|a| a == "--permission-mode=plan")
}

/// The narrowest rule covering the request.
fn rule_for(tool: &str, input: &Value) -> Result<String, String> {
    match tool {
        "Bash" => {
            let command = input["command"].as_str().unwrap_or("");
            if command.contains(['*', '(', ')']) {
                return Err(
                    "a permission rule reads `*`, `(` and `)` as rule syntax, so --always could \
                     allow more than this command"
                        .into(),
                );
            }
            Ok(format!("Bash({command})"))
        }
        "WebFetch" => Ok(format!(
            "WebFetch(domain:{})",
            url_host(input["url"].as_str().unwrap_or(""))
        )),
        _ => Ok(tool.to_string()),
    }
}

fn url_host(url: &str) -> String {
    let authority = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#', '\\'])
        .next()
        .unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = match host.strip_prefix('[') {
        Some(rest) => rest.find(']').map_or(host, |end| &host[..end + 2]),
        None => host.split(':').next().unwrap_or(""),
    };
    host.to_ascii_lowercase()
}

/// For Claude's `--allowedTools`.
pub fn allowed_rules(dir: &Path) -> Vec<String> {
    fs::read(dir.join("allowed"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn granted_mode(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("mode"))
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Newline-delimited JSON-RPC over stdio.
pub fn serve(id: &str) -> Result<(), String> {
    let dir = run_dir(id);
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications carry no id and get no reply.
        let Some(req_id) = req.get("id").cloned() else {
            continue;
        };
        let params = &req["params"];
        let mut reply = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({"result": {
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
            }}),
            "tools/list" => json!({"result": {"tools": [{
                "name": "permit",
                "description": "Asks the agent managing this claude-proxy run for a decision.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tool_name": {"type": "string"},
                        "input": {"type": "object"},
                        "tool_use_id": {"type": "string"},
                    },
                    "required": ["tool_name", "input"],
                },
            }]}}),
            "tools/call" => {
                let decision = decide(&dir, id, &params["arguments"]);
                json!({"result": {"content": [{"type": "text", "text": decision.to_string()}]}})
            }
            "ping" => json!({"result": {}}),
            _ => json!({"error": {"code": -32601, "message": "method not found"}}),
        };
        reply["jsonrpc"] = json!("2.0");
        reply["id"] = req_id;
        let mut bytes = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    Ok(())
}

/// Blocks until answered, the run is stopped, or Claude (our parent) goes away.
fn decide(dir: &Path, id: &str, args: &Value) -> Value {
    let tool = args["tool_name"].as_str().unwrap_or("tool").to_string();
    let input = args.get("input").cloned().unwrap_or_else(|| json!({}));
    let asks = dir.join("asks");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let key = ask_key(nanos);
    let ask =
        json!({"tool": tool, "input": input, "tool_use_id": args["tool_use_id"], "at": now_secs()});
    if crate::paths::private_dir(&asks)
        .and_then(|()| write_json(&asks.join(format!("{key}.json")), &ask))
        .is_err()
    {
        return json!({"behavior": "deny", "message": "claude-proxy could not record the request."});
    }
    marker(
        dir,
        json!({"event": "ask", "run": id, "tool": tool, "input": input}),
    );
    if let Ok(meta) = runs::load(id) {
        let lines = crate::render::ask_lines(id, &tool, &input, true).join("\n");
        runs::notify_parent(&meta, &format!("is waiting for an answer:\n\n{lines}"));
    }

    let parent = parent_id();
    let answer_path = asks.join(format!("{key}.answer"));
    let answer = loop {
        if let Some(a) = fs::read(&answer_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            break a;
        }
        if dir.join("stop").exists() || parent_id() != parent {
            break json!({"behavior": "deny", "message": "The run was stopped."});
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    let _ = fs::remove_file(asks.join(format!("{key}.json")));
    let _ = fs::remove_file(&answer_path);
    marker(
        dir,
        json!({"event": "answer", "behavior": answer["behavior"],
               "message": answer["message"], "answers": answer["answers"],
               "granted": answer["updatedPermissions"]}),
    );

    if answer["behavior"] == "allow" {
        let mut updated = input;
        if let (Some(fields), Some(answers)) = (updated.as_object_mut(), answer.get("answers")) {
            fields.insert("answers".into(), answers.clone());
        }
        let mut decision = json!({"behavior": "allow", "updatedInput": updated});
        if let Some(updates) = answer.get("updatedPermissions") {
            decision["updatedPermissions"] = updates.clone();
        }
        decision
    } else {
        json!({"behavior": "deny", "message": answer["message"].as_str().unwrap_or("Denied.")})
    }
}

#[cfg(unix)]
fn parent_id() -> u32 {
    std::os::unix::process::parent_id()
}

#[cfg(not(unix))]
fn parent_id() -> u32 {
    0
}

/// Unique and ordered by posting time, so same-tick asks never overwrite each
/// other and still list oldest first.
fn ask_key(nanos: u128) -> String {
    static ASKS: AtomicUsize = AtomicUsize::new(0);
    let n = ASKS.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:024}-{n:06}-{}", std::process::id())
}

fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    crate::paths::write_atomic(path, &serde_json::to_vec(value)?, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_allowing_picks_the_narrowest_rule() {
        assert_eq!(
            rule_for("Bash", &json!({"command": "cargo test -p x"})),
            Ok("Bash(cargo test -p x)".into())
        );
        for command in ["rm -rf build/*", "echo $(date)", "(cd x && make)"] {
            assert!(
                rule_for("Bash", &json!({ "command": command })).is_err(),
                "{command}"
            );
        }
        for (url, host) in [
            ("https://docs.rs/serde/latest?x=1", "docs.rs"),
            ("https://user:pw@Docs.rs:443/x", "docs.rs"),
            ("https://good.com@evil.com/", "evil.com"),
            ("http://evil.com\\@good.com/", "evil.com"),
            ("http://[::1]:8080/", "[::1]"),
        ] {
            assert_eq!(
                rule_for("WebFetch", &json!({ "url": url })),
                Ok(format!("WebFetch(domain:{host})")),
                "{url}"
            );
        }
        assert_eq!(rule_for("mcp__x__y", &json!({})), Ok("mcp__x__y".into()));
    }

    #[test]
    fn ask_keys_are_unique_and_keep_posting_order() {
        let a = ask_key(5);
        let b = ask_key(5);
        assert_ne!(a, b);
        assert!(a < b);
        assert!(ask_key(4) < a);
    }
}
