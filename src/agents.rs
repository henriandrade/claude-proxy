//! The run commands: presentation on top of the run engine in `runs`.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::asks::{self, Reply};
use crate::paths::home;
use crate::quota::{now_secs, short_duration};
use crate::render::{ask_lines, render_line};
use crate::runs::{self, Meta, NewRun, State, Waited};

pub struct RunOpts {
    pub message: String,
    pub account: String,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub wait: bool,
    pub json: bool,
    /// `Some("")` forks the calling Claude session.
    pub fork: Option<String>,
    pub worktree: bool,
    pub agent: Option<String>,
    pub claude_args: Vec<String>,
}

pub fn run(opts: RunOpts) -> Result<i32, String> {
    let message = read_message(&opts.message)?;
    runs::prune();
    let pool = runs::parse_pool(&opts.account)?;
    let fork_from = opts.fork.map(fork_source).transpose()?;
    let first = runs::first_account(&pool)?;
    let cwd = match opts.cwd {
        Some(dir) => dir,
        None => std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?,
    };
    let cwd = cwd
        .canonicalize()
        .map_err(|e| format!("bad --cwd {}: {e}", cwd.display()))?;
    let mut claude_args = opts.claude_args;
    if let Some(agent) = opts.agent {
        claude_args.extend(["--agent".to_string(), agent]);
    }
    if let Some(mode) = inherited_mode(&claude_args) {
        eprintln!("claude-proxy: inheriting your session's permission mode ({mode})");
        claude_args.extend(["--permission-mode".to_string(), mode]);
    }
    // Set by `runs::turn_command` inside a run.
    let parent = std::env::var("CLAUDE_PROXY_RUN")
        .ok()
        .filter(|id| runs::load(id).is_ok());
    let session = std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let meta = runs::start(NewRun {
        account: first,
        pool,
        name: opts.name,
        cwd: cwd.to_string_lossy().into_owned(),
        claude_args,
        message,
        fork_from,
        worktree: opts.worktree,
        parent,
        session,
    })?;
    eprintln!(
        "claude-proxy: run {} on {} — follow it with:  claude-proxy watch {}",
        meta.id, meta.account, meta.id
    );
    if let Some(parent) = &meta.parent {
        eprintln!(
            "claude-proxy: it reports to run {parent}: when it finishes, fails, or asks \
             something, that run gets a message — end your turn instead of waiting"
        );
    }
    if let Some(w) = &meta.worktree {
        eprintln!("claude-proxy: working in {} (branch {})", w.path, w.branch);
    }
    if opts.json {
        println!("{}", status_json(&meta));
    } else if !opts.wait {
        println!("{}", meta.id);
    }
    if opts.wait {
        return wait_and_print(&meta.id, None);
    }
    Ok(0)
}

/// The calling session's mode as the plugin's `prompt` hook recorded it: what a
/// subagent would inherit.
fn inherited_mode(claude_args: &[String]) -> Option<String> {
    let explicit = claude_args
        .iter()
        .any(|a| a.starts_with("--permission-mode") || a == "--dangerously-skip-permissions");
    if explicit {
        return None;
    }
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .and_then(|s| crate::hook::session_mode(&s))
        .filter(|m| m != "default")
}

fn fork_source(id: String) -> Result<String, String> {
    let id = if id.is_empty() {
        std::env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(
                "--fork copies the Claude session it runs inside, \
                 but CLAUDE_CODE_SESSION_ID is not set; use --fork-from <session-id>",
            )?
    } else {
        id
    };
    if runs::find_session(&home().join(".claude").join("projects"), &id).is_none() {
        return Err(format!("no session {id} in ~/.claude/projects to fork"));
    }
    Ok(id)
}

/// Runs from this repository that are going or finished within this long are
/// listed by default.
const RECENT_SECS: i64 = 2 * 3600;

pub fn runs(as_json: bool, everything: bool) -> Result<i32, String> {
    runs::prune();
    let every = runs::list();
    let total = every.len();
    let here = project_root();
    let now = now_secs();
    let all: Vec<Meta> = if everything {
        every
    } else {
        every
            .into_iter()
            .filter(|m| {
                let live = !matches!(
                    runs::effective_state(m),
                    State::Idle | State::Failed | State::Killed
                );
                (live || now - m.updated_at < RECENT_SECS)
                    && here.as_deref().is_none_or(|root| in_project(m, root))
            })
            .collect()
    };
    let hidden = total - all.len();
    let footer = || {
        if hidden > 0 {
            println!(
                "\n{hidden} more (finished over {}h ago, or in other projects):  claude-proxy runs --all",
                RECENT_SECS / 3600
            );
        }
    };
    if as_json {
        let rows: Vec<_> = all.iter().map(status_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return Ok(0);
    }
    if all.is_empty() {
        if hidden > 0 {
            println!("Nothing going or recently finished here.");
            footer();
        } else {
            println!("No runs yet. Start one with:  claude-proxy run \"<task>\"");
        }
        return Ok(0);
    }
    let now = now_secs();
    let account_w = all
        .iter()
        .map(|m| m.account.len())
        .max()
        .unwrap_or(7)
        .max(7);
    println!(
        "{:<10}  {:<7}  {:<account_w$}  {:>5}  {:>9}  WHAT",
        "ID", "STATE", "ACCOUNT", "TURNS", "ACTIVE"
    );
    for (depth, m) in runs::tree(all) {
        let what = m
            .activity
            .clone()
            .or_else(|| m.summary.clone())
            .or_else(|| m.name.clone())
            .unwrap_or_default();
        let id = if depth == 0 {
            m.id.clone()
        } else {
            format!("{}└ {}", "  ".repeat(depth - 1), m.id)
        };
        println!(
            "{:<10}  {:<7}  {:<account_w$}  {:>5}  {:>9}  {}",
            id,
            runs::effective_state(&m).as_str(),
            m.account,
            m.turns,
            format!("{} ago", short_duration(now - m.updated_at)),
            truncate(&what, 70),
        );
    }
    footer();
    Ok(0)
}

/// For a worktree, its main checkout; outside git, the directory itself.
fn project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
    let common = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(&cwd)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    match common {
        Some(git_dir) if git_dir.ends_with(".git") => git_dir
            .parent()
            .map(Path::to_path_buf)
            .and_then(|p| p.canonicalize().ok()),
        _ => Some(cwd),
    }
}

fn in_project(m: &Meta, root: &Path) -> bool {
    Path::new(&m.cwd).starts_with(root)
        || m.worktree
            .as_ref()
            .is_some_and(|w| Path::new(&w.repo).starts_with(root))
}

pub fn status(id: &str, as_json: bool) -> Result<i32, String> {
    let m = runs::load(id)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status_json(&m)).unwrap_or_default()
        );
        return Ok(0);
    }
    let now = now_secs();
    let state = runs::effective_state(&m);
    let title = match &m.name {
        Some(name) => format!("run {} · {name}", m.id),
        None => format!("run {}", m.id),
    };
    println!("{title}");
    let mut state_line = format!("{} · turn {}", state.as_str(), m.turns);
    if state == State::Working {
        if let Some(t) = m.turn_started_at {
            state_line.push_str(&format!(" · {}", short_duration(now - t)));
        }
    } else {
        state_line.push_str(&format!(" · {} ago", short_duration(now - m.updated_at)));
    }
    let row = |k: &str, v: &str| println!("  {k:<8} {v}");
    row("state", &state_line);
    if state == State::Waiting {
        for ask in asks::pending(&runs::run_dir(id)) {
            for line in ask_lines(id, &ask.tool, &ask.input, true) {
                println!("    {line}");
            }
        }
    }
    let failover = match m.pool.as_slice() {
        [] => "auto failover".to_string(),
        [_] => "pinned".to_string(),
        pool => format!("fallback {}", pool.join(" → ")),
    };
    row("account", &format!("{} · {failover}", m.account));
    if let Some(moved) = &m.moved {
        row("moved", moved);
    }
    if let Some(a) = &m.activity {
        row("doing", a);
    }
    if m.background > 0 {
        row(
            "waiting",
            &format!(
                "on {} background task(s); Claude resumes when they end",
                m.background
            ),
        );
    }
    if let Some(parent) = &m.parent {
        row("reports", &format!("to run {parent}"));
    }
    let children: Vec<String> = runs::list()
        .into_iter()
        .filter(|c| c.parent.as_deref() == Some(id))
        .map(|c| format!("{} {}", c.id, runs::effective_state(&c).as_str()))
        .collect();
    if !children.is_empty() {
        row("started", &children.join(", "));
    }
    if let Some(s) = &m.summary {
        row("last", s);
    }
    if let Some(n) = &m.needs_action {
        row("needs", n);
    }
    if let Some((text, ok)) = runs::last_result(id) {
        let label = if ok { "result" } else { "error" };
        row(label, &truncate(text.lines().next().unwrap_or(""), 100));
    }
    let queued = runs::queued(&runs::run_dir(id));
    if queued > 0 {
        row("queued", &format!("{queued} message(s)"));
    }
    row("cost", &format!("${:.2}", m.cost_usd));
    row("cwd", &m.cwd);
    if let Some(w) = &m.worktree {
        row("worktree", &format!("branch {} (of {})", w.branch, w.repo));
    }
    if let Some(parent) = &m.fork_from {
        row("forked", &format!("from session {parent}"));
    }
    row("session", &m.session_id);
    row("watch", &format!("claude-proxy watch {id}"));
    row(
        "take over",
        &format!("claude-proxy attach {id}  (when it is not working)"),
    );
    Ok(0)
}

fn status_json(m: &Meta) -> serde_json::Value {
    let dir = runs::run_dir(&m.id);
    json!({
        "id": m.id,
        "name": m.name,
        "state": runs::effective_state(m).as_str(),
        "account": m.account,
        "pool": m.pool,
        "moved": m.moved,
        "turns": m.turns,
        "activity": m.activity,
        "summary": m.summary,
        "needs_action": m.needs_action,
        "last_result": runs::last_result(&m.id).map(|(text, ok)| json!({"text": text, "ok": ok})),
        "queued": runs::queued(&dir),
        "working": matches!(runs::effective_state(m), State::Working | State::Waiting),
        "cost_usd": m.cost_usd,
        "cwd": m.cwd,
        "worktree": m.worktree,
        "fork_from": m.fork_from,
        "parent": m.parent,
        "session": m.session,
        "background": m.background,
        "asks": asks::pending(&dir).iter().map(|a| json!({"tool": a.tool, "input": a.input})).collect::<Vec<_>>(),
        "session_id": m.session_id,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
        "turn_started_at": m.turn_started_at,
        "transcript": runs::events_path(&m.id),
    })
}

pub fn read(
    id: &str,
    lines: Option<usize>,
    follow: bool,
    full: bool,
    as_json: bool,
) -> Result<i32, String> {
    runs::load(id)?;
    let path = runs::events_path(id);
    let render = |line: &str| -> Vec<String> {
        if as_json {
            vec![line.to_string()]
        } else {
            render_line(line, full)
        }
    };

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let complete = content.rfind('\n').map_or(0, |i| i + 1);
    let blocks: Vec<Vec<String>> = content[..complete]
        .lines()
        .map(render)
        .filter(|b| !b.is_empty())
        .collect();
    let skip = lines.map_or(0, |n| blocks.len().saturating_sub(n));
    for block in &blocks[skip..] {
        for line in block {
            println!("{line}");
        }
    }
    if !follow {
        return Ok(0);
    }

    let mut offset = complete as u64;
    let mut pending = String::new();
    loop {
        // Decide before reading, so the last lines are printed after it ends.
        let done = runs::finished(id)?;
        if let Ok(mut f) = std::fs::File::open(&path) {
            let mut fresh = String::new();
            if f.seek(SeekFrom::Start(offset)).is_ok() && f.read_to_string(&mut fresh).is_ok() {
                offset += fresh.len() as u64;
                pending.push_str(&fresh);
            }
        }
        while let Some(i) = pending.find('\n') {
            let line: String = pending.drain(..=i).collect();
            for out in render(line.trim_end()) {
                println!("{out}");
            }
        }
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    if !as_json {
        let m = runs::load(id)?;
        eprintln!(
            "— run {} is {} (`claude-proxy send {} \"…\"` to continue)",
            id,
            runs::effective_state(&m).as_str(),
            id
        );
    }
    Ok(0)
}

pub fn result(id: &str) -> Result<i32, String> {
    runs::load(id)?;
    match runs::last_result(id) {
        Some((text, ok)) => {
            println!("{text}");
            Ok(if ok { 0 } else { 1 })
        }
        None => Err(format!(
            "run {id} has no result for its last turn yet. See:  claude-proxy status {id}"
        )),
    }
}

pub fn send(id: &str, message: &str, account: Option<&str>) -> Result<i32, String> {
    let message = read_message(message)?;
    if let Some(spec) = account {
        runs::parse_pool(spec)?;
    }
    runs::send(id, &message, account)?;
    let to = account.map(|a| format!(" on {a}")).unwrap_or_default();
    eprintln!("claude-proxy: queued for run {id}{to} — `claude-proxy watch {id}` to follow");
    Ok(0)
}

pub fn wait(id: &str, timeout: Option<u64>) -> Result<i32, String> {
    wait_and_print(id, timeout.map(Duration::from_secs))
}

fn wait_and_print(id: &str, timeout: Option<Duration>) -> Result<i32, String> {
    match runs::wait(id, timeout)? {
        Waited::Done(m) => {
            let state = runs::effective_state(&m);
            if let Some((text, _)) = runs::last_result(id) {
                println!("{text}");
            }
            eprintln!("claude-proxy: run {id} is {}", state.as_str());
            Ok(if state == State::Idle { 0 } else { 1 })
        }
        Waited::Asking(pending) => {
            for ask in &pending {
                for line in ask_lines(id, &ask.tool, &ask.input, true) {
                    println!("{line}");
                }
            }
            eprintln!(
                "claude-proxy: run {id} is waiting for an answer; reply, then \
                 `claude-proxy wait {id}` again"
            );
            Ok(2)
        }
        Waited::TimedOut(m) => {
            eprintln!(
                "claude-proxy: run {id} is still {} (timed out)",
                runs::effective_state(&m).as_str()
            );
            Ok(124)
        }
    }
}

pub fn reply(id: &str, reply: Reply) -> Result<i32, String> {
    let (ask, granted) = asks::reply(id, reply)?;
    let what = if ask.is_question() {
        "question".to_string()
    } else {
        format!("request to use {}", ask.tool)
    };
    let forever = if granted.is_empty() {
        String::new()
    } else {
        format!(" and, from now on, {}", granted.join(", "))
    };
    eprintln!(
        "claude-proxy: answered run {id}'s {what}{forever} — `claude-proxy wait {id}` to continue waiting"
    );
    Ok(0)
}

pub fn attach(id: &str) -> Result<i32, String> {
    runs::attach(id)
}

/// One line per state change, for a Monitor tool. `mine`: the runs the calling
/// Claude session started, and what they started.
pub fn events(ids: &[String], mine: bool) -> Result<i32, String> {
    let session = if mine {
        Some(
            std::env::var("CLAUDE_CODE_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or("--mine needs CLAUDE_CODE_SESSION_ID; run it from inside Claude Code")?,
        )
    } else {
        None
    };
    let mut seen: std::collections::HashMap<String, String> = Default::default();
    let mut first = true;
    loop {
        let all = runs::list();
        let wanted = session.as_deref().map(|s| runs::of_session(&all, s));
        for m in &all {
            if !ids.is_empty() && !ids.contains(&m.id) {
                continue;
            }
            if wanted.as_ref().is_some_and(|w| !w.contains(&m.id)) {
                continue;
            }
            let line = event_line(m);
            if seen.get(&m.id) == Some(&line) {
                continue;
            }
            let state = runs::effective_state(m);
            // Start from what is live; finished runs from before are history.
            if !(first && matches!(state, State::Idle | State::Failed | State::Killed)) {
                println!("{line}");
            }
            seen.insert(m.id.clone(), line);
        }
        first = false;
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn event_line(m: &Meta) -> String {
    let state = runs::effective_state(m);
    let name = m
        .name
        .as_deref()
        .map(|n| format!(" {n}"))
        .unwrap_or_default();
    let detail = match state {
        State::Waiting => asks::pending(&runs::run_dir(&m.id))
            .first()
            .map(|a| {
                ask_lines(&m.id, &a.tool, &a.input, false)
                    .first()
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
        State::Idle | State::Failed => runs::last_result(&m.id)
            .map(|(text, _)| truncate(text.lines().next().unwrap_or(""), 120))
            .unwrap_or_default(),
        _ => m.activity.clone().unwrap_or_default(),
    };
    let moved = m
        .moved
        .as_deref()
        .map(|x| format!(" (moved {x})"))
        .unwrap_or_default();
    format!(
        "{}{name} · {} · turn {} on {}{moved} — {}",
        m.id,
        state.as_str(),
        m.turns,
        m.account,
        truncate(&detail, 140)
    )
}

pub fn kill(id: &str) -> Result<i32, String> {
    if runs::kill(id)? {
        println!(
            "Killed run {id}. Queued messages are kept; `claude-proxy send {id} \"…\"` resumes it."
        );
    } else {
        println!("Run {id} is not running.");
    }
    Ok(0)
}

pub fn rm(id: &str) -> Result<i32, String> {
    let kept = runs::remove(id)?;
    println!("Removed run {id}.");
    for k in kept {
        println!("Kept {k}.");
    }
    Ok(0)
}

fn read_message(message: &str) -> Result<String, String> {
    let text = if message == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("could not read the message from stdin: {e}"))?;
        buf
    } else {
        message.to_string()
    };
    if text.trim().is_empty() {
        return Err("the message is empty".into());
    }
    Ok(text)
}

fn truncate(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}
