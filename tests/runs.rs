//! End-to-end tests of managed runs against a fake `claude` on PATH, in a
//! throwaway HOME.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Speaks Claude's stream-json: echoes each message (`isReplay`), plays one turn,
/// and appends to the session file. `$FAKE_LOG` gets `<mode> <sid> <account>
/// <prompt>`, mode being `new`, `resume`, `fork:<parent>` or `warm`. Prompts:
/// - `sleep`: hangs (a turn to kill)
/// - `nap`: takes 2s (a turn to message while it works)
/// - `limited`: usage limit on `acct-a`
/// - `overloaded`: a non-account API error on `acct-a`
/// - `ask`: a Bash permission prompt through `__permit`; the decision is echoed
/// - `ask question` / `ask plan` / `ask star`: AskUserQuestion / ExitPlanMode / a `*` command
/// - `bgtask`: a background task ending 2s later; Claude wakes and answers `bg done`
/// - `$FAKE_LOGGED_OUT`: an account that is not logged in
const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "run=${CLAUDE_PROXY_RUN:-} $*" >> "$FAKE_ARGS_LOG"
sid=""; mode=""; fork=""; cfg=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id) mode="new"; sid="$2"; shift ;;
    --resume) mode="resume"; sid="$2"; shift ;;
    --fork-session) fork="$sid" ;;
    --mcp-config=*) cfg="${1#--mcp-config=}" ;;
  esac
  shift
done
[ -n "$fork" ] && mode="fork:$fork"
acct=$(basename "${CLAUDE_CONFIG_DIR:-primary}")
if [ "$acct" = "${FAKE_LOGGED_OUT:-}" ]; then echo 'Not logged in · Please run /login'; exit 1; fi
while IFS= read -r line; do
  prompt=$(printf '%s\n' "$line" | sed 's/.*"content":"\(.*\)","role":"user".*/\1/')
  echo "$mode $sid $acct $prompt" >> "$FAKE_LOG"
  mode="warm"
  mkdir -p "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/fake"
  echo "$prompt" >> "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/fake/$sid.jsonl"
  echo '{"type":"system","subtype":"init","session_id":"'"$sid"'"}'
  printf '%s\n' '{"type":"user","isReplay":true,"message":{"role":"user","content":"'"$prompt"'"}}'
  case "$prompt" in
    sleep*) echo '{"type":"system","subtype":"task_summary","detail":"sleeping"}'; sleep 30 ;;
    nap*) echo '{"type":"system","subtype":"task_summary","detail":"napping"}'; sleep 2 ;;
    bgtask*)
      echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"t1"}]}'
      ( sleep 2
        echo '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
        echo '{"type":"system","subtype":"task_notification","task_id":"t1","status":"completed"}'
        echo '{"type":"assistant","message":{"content":[{"type":"text","text":"bg done"}]}}'
        echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"bg done"}' ) & ;;
    limited*)
      if [ "$acct" = "acct-a" ]; then
        echo '{"type":"assistant","message":{"content":[{"type":"text","text":"usage limit reached"}]},"error":"rate_limit"}'
        echo '{"type":"result","subtype":"success","is_error":true,"result":"usage limit reached"}'
        exit 1
      fi ;;
    overloaded*)
      if [ "$acct" = "acct-a" ]; then
        echo '{"type":"assistant","message":{"content":[{"type":"text","text":"overloaded"}]},"error":"overloaded"}'
        echo '{"type":"result","subtype":"success","is_error":true,"result":"overloaded"}'
        exit 1
      fi ;;
    ask*)
      cmd=$(sed 's/.*"command":"\([^"]*\)".*/\1/' "$cfg")
      id=$(sed 's/.*"__permit","\([^"]*\)".*/\1/' "$cfg")
      tool=Bash; input='{"command":"rm -rf build"}'
      case "$prompt" in
        *question*) tool=AskUserQuestion
          input='{"questions":[{"question":"Which colour?","options":[{"label":"Red"},{"label":"Blue"}]}]}' ;;
        *plan*) tool=ExitPlanMode; input='{"plan":"do it"}' ;;
        *star*) input='{"command":"rm -rf build/*"}' ;;
      esac
      prompt=$(printf '%s\n' \
        '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
        '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
        '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"permit","arguments":{"tool_name":"'"$tool"'","input":'"$input"',"tool_use_id":"t1"}}}' \
        | "$cmd" __permit "$id" | tail -n 1 | sed 's/.*"text":"\(.*\)","type":"text".*/\1/') ;;
  esac
  echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"echo hi"}}]}}'
  echo '{"type":"user","message":{"content":[{"type":"tool_result","content":"hi","is_error":false}]}}'
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"echo: '"$prompt"'"}]}}'
  printf '%s\n' '{"type":"system","subtype":"post_turn_summary","status_detail":"echoed '"$prompt"'","needs_action":""}'
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":5,"total_cost_usd":0.01,"result":"echo: '"$prompt"'"}'
done
"#;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    path: String,
    log: PathBuf,
    args_log: PathBuf,
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let registry = home.join(".config/claude-proxy/registry.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, r#"{"labels":["acct-a","acct-b"]}"#).unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    Env {
        path: format!("{}:/usr/bin:/bin", bin.display()),
        log: tmp.path().join("claude.log"),
        args_log: tmp.path().join("args.log"),
        home,
        bin,
        _tmp: tmp,
    }
}

fn cp(env: &Env, args: &[&str]) -> Output {
    run_as(
        env,
        Path::new(env!("CARGO_BIN_EXE_claude-proxy")),
        args,
        &[],
    )
}

fn run_as(env: &Env, exe: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .env("PATH", &env.path)
        .env("FAKE_LOG", &env.log)
        .env("FAKE_ARGS_LOG", &env.args_log)
        // Drainers exit at once unless a test asks for a warm one.
        .env("CLAUDE_PROXY_IDLE_SECS", "0")
        .current_dir(&env.home);
    for (k, v) in vars {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn cp_stdin(env: &Env, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-proxy"))
        .args(args)
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .env("PATH", &env.path)
        .env("FAKE_LOG", &env.log)
        .env("FAKE_ARGS_LOG", &env.args_log)
        .env("CLAUDE_PROXY_IDLE_SECS", "0")
        .current_dir(&env.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A command that exits before reading closes the pipe.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn status(env: &Env, id: &str) -> Value {
    serde_json::from_slice(&cp(env, &["status", id, "--json"]).stdout).unwrap()
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_run_is_started_messaged_read_killed_resumed_and_removed() {
    let env = setup();

    let out = cp(&env, &["run", "--account", "claude", "hello"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = stdout(&out);
    assert_eq!(id.len(), 6, "run prints just the id: {id:?}");

    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "echo: hello");

    assert!(cp(&env, &["send", &id, "again"]).status.success());
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(stdout(&out), "echo: again");
    let log = std::fs::read_to_string(&env.log).unwrap();
    let calls: Vec<Vec<&str>> = log.lines().map(|l| l.split(' ').collect()).collect();
    assert_eq!(calls[0][0], "new");
    assert_eq!(calls[1][0], "resume");
    assert_eq!(calls[0][1], calls[1][1], "same session id");

    let text = stdout(&cp(&env, &["read", &id]));
    for needle in [
        "▶ you",
        "  hello",
        "● claude · turn 1",
        "  ⚙ Bash  echo hi",
        "    ↳ hi",
        "  echo: again",
        "✓ done · 1 steps",
    ] {
        assert!(
            text.contains(needle),
            "transcript lacks {needle:?}:\n{text}"
        );
    }
    let tail = stdout(&cp(&env, &["tail", &id, "-n", "1"]));
    assert!(tail.starts_with("✓ done"), "{tail}");

    let s = status(&env, &id);
    assert_eq!(s["state"], "idle");
    assert_eq!(s["turns"], 2);
    assert_eq!(s["summary"], "echoed again");
    assert_eq!(s["last_result"]["text"], "echo: again");

    assert!(cp(&env, &["send", &id, "sleep please"]).status.success());
    wait_until("the turn is working", || {
        let s = status(&env, &id);
        s["state"] == "working" && s["activity"] == "sleeping"
    });
    let out = cp(&env, &["kill", &id]);
    assert!(stdout(&out).starts_with("Killed run"), "{}", stdout(&out));
    assert_eq!(status(&env, &id)["state"], "killed");
    let out = cp(&env, &["wait", &id, "--timeout", "10"]);
    assert_eq!(out.status.code(), Some(1), "a killed run waits as failed");
    assert!(stdout(&cp(&env, &["read", &id])).contains("■ killed"));

    assert!(cp(&env, &["send", &id, "back"]).status.success());
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(stdout(&out), "echo: back");
    assert_eq!(status(&env, &id)["state"], "idle");

    let runs: Value = serde_json::from_slice(&cp(&env, &["runs", "--json"]).stdout).unwrap();
    assert_eq!(runs[0]["id"], id.as_str());

    assert!(cp(&env, &["rm", &id]).status.success());
    assert!(!cp(&env, &["status", &id]).status.success());
}

#[test]
fn run_wait_blocks_and_prints_the_answer() {
    let env = setup();
    let out = cp(&env, &["run", "--account", "claude", "--wait", "quick"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "echo: quick");
}

#[test]
fn a_message_sent_while_working_is_delivered_as_the_next_turn() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "nap first"]));
    wait_until("the first turn is working", || {
        status(&env, &id)["activity"] == "napping"
    });

    assert!(cp(&env, &["send", &id, "then this"]).status.success());
    assert_eq!(status(&env, &id)["queued"], 1);

    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(stdout(&out), "echo: then this");
    let log = std::fs::read_to_string(&env.log).unwrap();
    let prompts: Vec<&str> = log
        .lines()
        .map(|l| l.splitn(4, ' ').nth(3).unwrap_or(""))
        .collect();
    assert_eq!(prompts, ["nap first", "then this"]);
    assert_eq!(status(&env, &id)["turns"], 2);
}

#[test]
fn a_usage_limit_moves_the_session_to_the_next_account() {
    let env = setup();
    let id = stdout(&cp(
        &env,
        &["run", "--account", "acct-a,acct-b", "limited job"],
    ));
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        stdout(&cp(&env, &["read", &id]))
    );
    assert!(
        stdout(&out).starts_with(
            "echo: (claude-proxy) Your previous turn stopped because the Claude account \
             acct-a hit its usage limit."
        ),
        "{}",
        stdout(&out)
    );

    // The message had already reached the session, so it is not sent twice.
    let log = std::fs::read_to_string(&env.log).unwrap();
    let calls: Vec<Vec<&str>> = log.lines().map(|l| l.splitn(4, ' ').collect()).collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0][..3], ["new", calls[0][1], "acct-a"]);
    assert_eq!(calls[0][3], "limited job");
    assert_eq!(calls[1][..3], ["resume", calls[0][1], "acct-b"]);

    let s = status(&env, &id);
    assert_eq!(s["state"], "idle");
    assert_eq!(s["account"], "acct-b");
    assert_eq!(s["moved"], "from acct-a, which hit its usage limit");
    assert!(stdout(&cp(&env, &["read", &id]))
        .contains("⇄ acct-a hit its usage limit — continuing on acct-b"));

    assert!(cp(&env, &["send", &id, "and then"]).status.success());
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: and then"
    );
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert!(log.lines().last().unwrap().contains(" acct-b and then"));
}

#[test]
fn a_pinned_account_does_not_move() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "acct-a", "limited job"]));
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(1));
    let s = status(&env, &id);
    assert_eq!(s["state"], "failed");
    assert_eq!(s["account"], "acct-a");
    assert!(stdout(&cp(&env, &["read", &id]))
        .contains("⇄ acct-a hit its usage limit — no other account to continue on"));
    assert!(cp(&env, &["send", &id, "--account", "acct-b", "go on"])
        .status
        .success());
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: go on"
    );
    assert_eq!(status(&env, &id)["account"], "acct-b");
}

#[test]
fn wait_after_send_reports_the_new_turn_even_under_status_probes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let env = Arc::new(setup());
    let id = stdout(&cp(&env, &["run", "--account", "acct-a", "limited start"]));
    // Each `status` briefly takes the run lock; the drainer and `wait` must not
    // trip over these probes.
    let stop = Arc::new(AtomicBool::new(false));
    let probers: Vec<_> = (0..3)
        .map(|_| {
            let (env, id, stop) = (Arc::clone(&env), id.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let out = cp(&env, &["status", &id]);
                    assert!(
                        out.status.success(),
                        "status read an unreadable meta.json: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            })
        })
        .collect();

    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(1)
    );
    for i in 0..12 {
        // Alternate failing (pinned, no failover) and passing turns, so most
        // sends land on a failed run.
        let fails = i % 2 == 1;
        let msg = if fails {
            format!("limited {i}")
        } else {
            format!("ok {i}")
        };
        assert!(cp(&env, &["send", &id, &msg]).status.success());
        let out = cp(&env, &["wait", &id, "--timeout", "30"]);
        if fails {
            assert_eq!(out.status.code(), Some(1), "step {i}");
            assert_eq!(stdout(&out), "usage limit reached", "step {i}");
        } else {
            assert_eq!(out.status.code(), Some(0), "step {i}");
            assert_eq!(stdout(&out), format!("echo: {msg}"), "step {i}");
        }
    }

    stop.store(true, Ordering::Relaxed);
    for p in probers {
        p.join().unwrap();
    }
}

#[test]
fn a_permission_prompt_waits_for_allow_or_deny() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "ask first"]));

    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let asked = stdout(&out);
    assert!(
        asked.contains("? permission to use Bash  rm -rf build"),
        "{asked}"
    );
    assert!(
        asked.contains(&format!("claude-proxy allow {id}")),
        "{asked}"
    );
    let s = status(&env, &id);
    assert_eq!(s["state"], "waiting");
    assert_eq!(s["asks"][0]["tool"], "Bash");
    assert_eq!(s["asks"][0]["input"]["command"], "rm -rf build");

    assert!(cp(&env, &["allow", &id]).status.success());
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        r#"echo: {"behavior":"allow","updatedInput":{"command":"rm -rf build"}}"#
    );
    assert!(!cp(&env, &["allow", &id]).status.success());

    assert!(cp(&env, &["send", &id, "ask again"]).status.success());
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    assert!(cp(&env, &["deny", &id, "not in this repo"])
        .status
        .success());
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        r#"echo: {"behavior":"deny","message":"not in this repo"}"#
    );

    let text = stdout(&cp(&env, &["read", &id]));
    for needle in [
        "? permission to use Bash  rm -rf build",
        "  ↪ allowed",
        "  ↪ denied: not in this repo",
    ] {
        assert!(
            text.contains(needle),
            "transcript lacks {needle:?}:\n{text}"
        );
    }
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    assert!(
        args.contains(&format!("run={id} ")),
        "the run's id reaches Claude's environment: {args}"
    );
    assert!(args.contains("--permission-prompt-tool mcp__claude-proxy__permit"));
}

#[test]
fn a_question_is_answered_with_answer() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "ask question"]));
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).contains("? Which colour?  [Red | Blue]"));
    assert!(!cp(&env, &["answer", &id, "a", "b"]).status.success());
    assert!(cp(&env, &["answer", &id, "blue"]).status.success());
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains(r#""answers":{"Which colour?":"blue"}"#),
        "{}",
        stdout(&out)
    );
    assert!(stdout(&cp(&env, &["read", &id])).contains("  ↪ Which colour? → blue"));
}

#[test]
fn a_fork_starts_from_a_copy_of_another_session() {
    let env = setup();
    let parent = "11111111-2222-4333-8444-555555555555";
    let projects = env.home.join(".claude/projects/elsewhere");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(projects.join(format!("{parent}.jsonl")), "{}\n").unwrap();

    // `--fork` with no id forks the calling session.
    let out = run_as(
        &env,
        Path::new(env!("CARGO_BIN_EXE_claude-proxy")),
        &["run", "--account", "claude", "--fork", "go"],
        &[("CLAUDE_CODE_SESSION_ID", parent)],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = stdout(&out);
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: go"
    );
    assert!(cp(&env, &["send", &id, "more"]).status.success());
    cp(&env, &["wait", &id, "--timeout", "30"]);

    let log = std::fs::read_to_string(&env.log).unwrap();
    let calls: Vec<Vec<&str>> = log.lines().map(|l| l.splitn(4, ' ').collect()).collect();
    assert_eq!(calls[0][0], format!("fork:{parent}"));
    assert_ne!(calls[0][1], parent, "the fork gets its own session id");
    assert_eq!(calls[1][..2], ["resume", calls[0][1]]);
    assert_eq!(status(&env, &id)["fork_from"], parent);

    assert!(!cp(&env, &["run", "--fork-from", "nope", "go"])
        .status
        .success());
}

#[test]
fn a_worktree_run_works_on_its_own_branch_and_is_cleaned_up() {
    let env = setup();
    let repo = env.home.join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::write(repo.join("sub/file.txt"), "x").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&repo)
            .env("HOME", &env.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let sub = repo.join("sub");
    let start = || {
        stdout(&cp(
            &env,
            &[
                "run",
                "--account",
                "claude",
                "--worktree",
                "--cwd",
                sub.to_str().unwrap(),
                "hi",
            ],
        ))
    };

    let id = start();
    assert_eq!(id.len(), 6, "{id}");
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let s = status(&env, &id);
    let tree = PathBuf::from(s["worktree"]["path"].as_str().unwrap());
    assert!(s["cwd"].as_str().unwrap().ends_with("/sub"), "{s}");
    assert!(tree.join("sub/file.txt").exists());
    assert_eq!(s["worktree"]["branch"], format!("claude-proxy/{id}"));

    assert!(cp(&env, &["rm", &id]).status.success());
    assert!(!tree.exists());
    assert!(git(&["branch", "--list", &format!("claude-proxy/{id}")]).is_empty());

    let id = start();
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let tree = PathBuf::from(status(&env, &id)["worktree"]["path"].as_str().unwrap());
    std::fs::write(tree.join("new.txt"), "work").unwrap();
    let out = cp(&env, &["rm", &id]);
    assert!(
        stdout(&out).contains("Kept its worktree"),
        "{}",
        stdout(&out)
    );
    assert!(tree.join("new.txt").exists());
}

#[test]
fn profiles_get_the_users_mcp_servers() {
    let env = setup();
    std::fs::write(
        env.home.join(".claude.json"),
        r#"{"mcpServers":{"demo":{"command":"demo-server"}}}"#,
    )
    .unwrap();
    let config_of = |id: &str| -> (Value, u32) {
        let path = env
            .home
            .join(format!(".config/claude-proxy/runs/{id}/mcp.json"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        (
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap(),
            mode & 0o777,
        )
    };

    let id = stdout(&cp(&env, &["run", "--account", "acct-a", "hello"]));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let (config, mode) = config_of(&id);
    assert_eq!(config["mcpServers"]["demo"]["command"], "demo-server");
    assert_eq!(config["mcpServers"]["claude-proxy"]["args"][0], "__permit");
    assert_eq!(mode, 0o600, "server entries can carry API keys");

    // The primary profile has them already.
    let id = stdout(&cp(&env, &["run", "--account", "claude", "hello"]));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    assert!(config_of(&id).0["mcpServers"].get("demo").is_none());

    // An account's own command gets them too.
    let proxy = env.bin.join("acct-a");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_claude-proxy"), &proxy).unwrap();
    assert!(run_as(&env, &proxy, &["mcp", "list"], &[]).status.success());
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    let last = args.lines().last().unwrap();
    assert!(last.starts_with("run= --mcp-config="), "{last}");
    assert!(last.ends_with(" mcp list"), "{last}");
}

#[test]
fn a_warm_claude_takes_follow_ups_without_restarting() {
    let env = setup();
    let warm = [("CLAUDE_PROXY_IDLE_SECS", "60")];
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let id = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "hello"],
        &warm,
    ));
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: hello"
    );
    // Idle, yet still holding its Claude: `wait` returned all the same.
    assert_eq!(status(&env, &id)["state"], "idle");

    assert!(cp(&env, &["send", &id, "again"]).status.success());
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: again"
    );
    let log = std::fs::read_to_string(&env.log).unwrap();
    let modes: Vec<&str> = log.lines().map(|l| l.split(' ').next().unwrap()).collect();
    assert_eq!(modes, ["new", "warm"], "one process for both messages");

    // `kill` on an idle run only lets go of its Claude.
    let out = cp(&env, &["kill", &id]);
    assert!(stdout(&out).contains("is not running"), "{}", stdout(&out));
    assert_eq!(status(&env, &id)["state"], "idle");
    assert!(cp(&env, &["send", &id, "later"]).status.success());
    assert_eq!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])),
        "echo: later"
    );
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert!(log.lines().last().unwrap().starts_with("resume "), "{log}");
    assert!(
        cp(&env, &["rm", &id]).status.success(),
        "rm lets go of a warm run"
    );
}

#[test]
fn attach_keeps_what_the_released_drainer_saved() {
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let id = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "hello"],
        &[("CLAUDE_PROXY_IDLE_SECS", "60")],
    ));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let path = env
        .home
        .join(format!(".config/claude-proxy/runs/{id}/meta.json"));
    let meta = || -> Value { serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap() };
    assert!(meta()["turn_pid"].is_number(), "the warm Claude is up");

    let out = cp(&env, &["attach", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    settled(&env, &id);
    let after = meta();
    assert!(after["turn_pid"].is_null(), "{after}");
    assert!(after["activity"].is_null(), "{after}");
}

#[test]
fn always_and_accept_edits_outlive_the_request() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "ask first"]));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    assert!(cp(&env, &["allow", &id, "--always"]).status.success());
    let out = stdout(&cp(&env, &["wait", &id, "--timeout", "30"]));
    assert!(
        out.contains(r#""updatedPermissions":[{"behavior":"allow","destination":"session","rules":[{"ruleContent":"rm -rf build","toolName":"Bash"}],"type":"addRules"}]"#),
        "{out}"
    );
    assert!(
        stdout(&cp(&env, &["read", &id])).contains("↪ allowed · from now on: Bash(rm -rf build)")
    );
    assert!(cp(&env, &["send", &id, "hi"]).status.success());
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    assert!(
        args.lines()
            .last()
            .unwrap()
            .contains("--allowedTools Bash(rm -rf build) --permission-prompt-tool"),
        "{args}"
    );

    // Approving a plan: later processes accept edits instead of planning again.
    let id = stdout(&cp(
        &env,
        &[
            "run",
            "--account",
            "claude",
            "ask plan",
            "--",
            "--permission-mode",
            "plan",
        ],
    ));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    assert!(cp(&env, &["allow", &id, "--accept-edits"]).status.success());
    assert!(
        stdout(&cp(&env, &["wait", &id, "--timeout", "30"])).contains(r#""mode":"acceptEdits""#)
    );
    assert!(cp(&env, &["send", &id, "go"]).status.success());
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    assert!(
        args.lines()
            .last()
            .unwrap()
            .ends_with("--permission-mode plan --permission-mode acceptEdits"),
        "{args}"
    );
}

#[test]
fn a_run_inherits_the_permission_mode_of_the_session_starting_it() {
    use std::io::Write;
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let session = "11111111-2222-4333-8444-555555555555";
    let mut hook = Command::new(exe)
        .args(["__hook", "prompt"])
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(
            format!(r#"{{"session_id":"{session}","permission_mode":"acceptEdits"}}"#).as_bytes(),
        )
        .unwrap();
    assert!(hook.wait().unwrap().success());

    let vars = [("CLAUDE_CODE_SESSION_ID", session)];
    let id = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "hi"],
        &vars,
    ));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    assert!(
        args.lines()
            .last()
            .unwrap()
            .ends_with("--permission-mode acceptEdits"),
        "{args}"
    );

    // An explicit mode wins.
    let id = stdout(&run_as(
        &env,
        exe,
        &[
            "run",
            "--account",
            "claude",
            "hi",
            "--",
            "--permission-mode",
            "plan",
        ],
        &vars,
    ));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    let args = std::fs::read_to_string(&env.args_log).unwrap();
    let last = args.lines().last().unwrap();
    assert!(
        last.ends_with("--permission-mode plan") && !last.contains("acceptEdits"),
        "{last}"
    );
}

#[test]
fn the_hook_turns_subagent_calls_into_runs() {
    use std::io::Write;
    let env = setup();
    let hook = |vars: &[(&str, &str)], description: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude-proxy"));
        cmd.args(["__hook", "pre-tool-use"])
            .env_clear()
            .env("HOME", &env.home)
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let input = format!(
            r#"{{"tool_name":"Agent","tool_input":{{"subagent_type":"gsd-executor","description":"{description}","prompt":"p"}}}}"#
        );
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let out = hook(&[], "fix it");
    assert_eq!(out.status.code(), Some(2), "blocked");
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("claude-proxy run --agent gsd-executor --name 'fix it' -"));
    assert_eq!(hook(&[], "[direct] fix it").status.code(), Some(0));
    assert_eq!(
        hook(&[("CLAUDE_PROXY_RUN", "abc123")], "fix it")
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        hook(&[("CLAUDE_PROXY_POLICY", "off")], "fix it")
            .status
            .code(),
        Some(0)
    );
}

fn hook(env: &Env, event: &str, input: &str) -> Output {
    cp_stdin(env, &["__hook", event], input)
}

#[test]
fn a_child_run_wakes_the_run_that_started_it() {
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let parent = stdout(&cp(
        &env,
        &["run", "--account", "claude", "--name", "orch", "plan it"],
    ));
    cp(&env, &["wait", &parent, "--timeout", "30"]);

    let inside = [("CLAUDE_PROXY_RUN", parent.as_str())];
    let child = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "--name", "lane", "build it"],
        &inside,
    ));
    assert_eq!(status(&env, &child)["parent"], parent.as_str());
    cp(&env, &["wait", &child, "--timeout", "30"]);

    let out = stdout(&cp(&env, &["wait", &parent, "--timeout", "30"]));
    assert!(
        out.starts_with(&format!(
            "echo: (claude-proxy) Run {child} (lane) finished:"
        )),
        "{out}"
    );
    assert!(out.contains("echo: build it"), "{out}");

    let tree = stdout(&cp(&env, &["runs"]));
    let rows: Vec<&str> = tree.lines().collect();
    let p = rows.iter().position(|r| r.starts_with(&parent)).unwrap();
    assert!(rows[p + 1].starts_with(&format!("└ {child}")), "{tree}");

    let asking = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "ask first"],
        &inside,
    ));
    assert_eq!(
        cp(&env, &["wait", &asking, "--timeout", "30"])
            .status
            .code(),
        Some(2)
    );
    let out = stdout(&cp(&env, &["wait", &parent, "--timeout", "30"]));
    assert!(
        out.contains(&format!("Run {asking} is waiting for an answer")),
        "{out}"
    );
    assert!(cp(&env, &["deny", &asking, "no"]).status.success());
    cp(&env, &["wait", &asking, "--timeout", "30"]);
}

#[test]
fn a_run_waits_for_its_background_tasks() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "bgtask"]));
    // Its turn ends first; the run is not done until Claude wakes on the
    // finished task, even with an idle timeout of zero.
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    if stdout(&out) != "bg done" {
        eprintln!("DEBUG status: {}", status(&env, &id));
        eprintln!(
            "DEBUG events:\n{}",
            stdout(&cp(&env, &["read", &id, "--json"]))
        );
    }
    assert_eq!(stdout(&out), "bg done");
    let s = status(&env, &id);
    assert_eq!(s["state"], "idle");
    assert_eq!(s["background"], 0);
    assert_eq!(s["turns"], 2);
    assert!(stdout(&cp(&env, &["read", &id])).contains("· woken by a background task"));
}

#[test]
fn events_print_each_change() {
    let env = setup();
    let id = stdout(&cp(
        &env,
        &["run", "--account", "claude", "--name", "ev", "nap a bit"],
    ));
    let mut events = Command::new(env!("CARGO_BIN_EXE_claude-proxy"))
        .args(["events", &id])
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", env.home.join(".config"))
        .env("PATH", &env.path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    cp(&env, &["wait", &id, "--timeout", "30"]);
    std::thread::sleep(Duration::from_millis(1500));
    events.kill().unwrap();
    let out = String::from_utf8_lossy(&events.wait_with_output().unwrap().stdout).to_string();
    assert!(out.contains(&format!("{id} ev · working")), "{out}");
    assert!(
        out.contains(&format!(
            "{id} ev · idle · turn 1 on claude — echo: nap a bit"
        )),
        "{out}"
    );
}

#[test]
fn hooks_tell_a_session_about_its_runs() {
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let session = "22222222-2222-4333-8444-555555555555";
    let mine = [("CLAUDE_CODE_SESSION_ID", session)];
    let prompt = format!(r#"{{"session_id":"{session}","permission_mode":"default"}}"#);

    let done = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "hello"],
        &mine,
    ));
    cp(&env, &["wait", &done, "--timeout", "30"]);
    let news = stdout(&hook(&env, "prompt", &prompt));
    assert!(news.contains(&format!("- {done} is idle")), "{news}");
    assert!(
        stdout(&hook(&env, "prompt", &prompt)).is_empty(),
        "told once"
    );

    let going = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "sleep please"],
        &mine,
    ));
    wait_until("it works", || status(&env, &going)["state"] == "working");
    let resumed = stdout(&hook(&env, "session-start", &prompt));
    assert!(
        resumed.contains(&format!("- {going} is working")),
        "{resumed}"
    );
    assert!(!resumed.contains(&done), "{resumed}");
    cp(&env, &["kill", &going]);
}

/// Wait until no drainer holds the run, so nothing rewrites its meta.json.
fn settled(env: &Env, id: &str) {
    let lock = env
        .home
        .join(format!(".config/claude-proxy/runs/{id}/run.lock"));
    let lock = std::fs::File::open(lock).unwrap();
    wait_until("the drainer let go", || {
        lock.try_lock().is_ok() && lock.unlock().is_ok()
    });
}

/// Make a run look last used `secs` ago.
fn age(env: &Env, id: &str, secs: i64) {
    settled(env, id);
    let path = env
        .home
        .join(format!(".config/claude-proxy/runs/{id}/meta.json"));
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    meta["updated_at"] = (now - secs).into();
    std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
}

#[test]
fn old_runs_leave_the_list_then_go_away() {
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let stderr = |out: &Output| String::from_utf8_lossy(&out.stderr).to_string();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "hello"]));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    assert!(stdout(&cp(&env, &["runs"])).contains(&id));

    // Finished three hours ago: out of the default list, still in --all.
    age(&env, &id, 3 * 3600);
    let listed = stdout(&cp(&env, &["runs"]));
    assert!(!listed.contains(&id), "{listed}");
    assert!(listed.contains("1 more"), "{listed}");
    assert!(stdout(&cp(&env, &["runs", "--all"])).contains(&id));

    let other = env.home.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let elsewhere = stdout(&cp(
        &env,
        &[
            "run",
            "--account",
            "claude",
            "--cwd",
            other.to_str().unwrap(),
            "hi",
        ],
    ));
    cp(&env, &["wait", &elsewhere, "--timeout", "30"]);
    assert!(!stdout(&cp(&env, &["runs"])).contains(&elsewhere));

    // Unused for a day: removed, unless retention is off.
    age(&env, &id, 25 * 3600);
    run_as(&env, exe, &["runs"], &[("CLAUDE_PROXY_KEEP_HOURS", "0")]);
    assert!(cp(&env, &["status", &id]).status.success(), "kept with 0");
    cp(&env, &["runs"]);
    let gone = cp(&env, &["status", &id]);
    assert!(!gone.status.success());
    assert!(
        stderr(&gone).contains("was removed") && stderr(&gone).contains("automatically"),
        "{}",
        stderr(&gone)
    );
    assert!(
        stderr(&gone).contains("claude --resume"),
        "{}",
        stderr(&gone)
    );

    assert!(cp(&env, &["rm", &elsewhere]).status.success());
    assert!(stderr(&cp(&env, &["watch", &elsewhere])).contains("with `claude-proxy rm`"));
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn run_state_is_private_to_the_user() {
    let env = setup();
    let root = env.home.join(".config/claude-proxy");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "ask first"]));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    let run = root.join("runs").join(&id);
    for dir in [&root, &root.join("runs"), &run, &run.join("inbox")] {
        assert_eq!(mode(dir), 0o700, "{}", dir.display());
    }
    assert_eq!(mode(&run.join("meta.json")), 0o600);
    assert_eq!(mode(&run.join("asks")), 0o700);
    let ask = std::fs::read_dir(run.join("asks"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "json"))
        .unwrap();
    assert_eq!(mode(&ask), 0o600);
    for file in ["events.jsonl", "stderr.log", "mcp.json", "run.lock"] {
        assert_eq!(mode(&run.join(file)), 0o600, "{file}");
    }

    assert!(cp(&env, &["allow", &id, "--always", "--accept-edits"])
        .status
        .success());
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(0)
    );
    for file in ["allowed", "mode"] {
        assert_eq!(mode(&run.join(file)), 0o600, "{file}");
    }

    let session = "33333333-3333-4333-8444-555555555555";
    hook(
        &env,
        "prompt",
        &format!(r#"{{"session_id":"{session}","permission_mode":"default"}}"#),
    );
    assert_eq!(mode(&root.join("sessions")), 0o700);
    assert_eq!(mode(&root.join("sessions").join(session)), 0o600);
}

#[test]
fn always_refuses_a_wildcard_command() {
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "ask star"]));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    let out = cp(&env, &["allow", &id, "--always"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--rule"), "{err}");
    let run = env.home.join(".config/claude-proxy/runs").join(&id);
    assert!(!run.join("allowed").exists());
    assert!(!run.join("mode").exists());
    assert_eq!(status(&env, &id)["state"], "waiting");

    assert!(cp(&env, &["allow", &id]).status.success());
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(0)
    );
}

#[test]
fn kill_never_signals_a_pid_a_dead_drainer_left() {
    use std::os::unix::process::CommandExt;
    let env = setup();
    let id = stdout(&cp(&env, &["run", "--account", "claude", "hello"]));
    cp(&env, &["wait", &id, "--timeout", "30"]);
    settled(&env, &id);

    let mut bystander = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let run = env.home.join(".config/claude-proxy/runs").join(&id);
    let path = run.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    meta["turn_pid"] = bystander.id().into();
    std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
    std::fs::write(run.join("pending"), "").unwrap();

    let alive = std::thread::scope(|s| {
        let killing = s.spawn(|| cp(&env, &["kill", &id]));
        std::thread::sleep(Duration::from_secs(1));
        let alive = bystander.try_wait().unwrap().is_none();
        std::fs::remove_file(run.join("pending")).unwrap();
        killing.join().unwrap();
        alive
    });
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(alive, "kill signalled a pid no drainer held");
}

#[test]
fn a_kill_that_times_out_leaves_the_run_stopping() {
    let env = setup();
    let exe = Path::new(env!("CARGO_BIN_EXE_claude-proxy"));
    let parent = stdout(&cp(
        &env,
        &["run", "--account", "claude", "--name", "orch", "plan it"],
    ));
    cp(&env, &["wait", &parent, "--timeout", "30"]);
    settled(&env, &parent);
    let run = env.home.join(".config/claude-proxy/runs").join(&parent);
    // No drainer ever takes the lock, so the kill cannot finish.
    std::fs::write(run.join("pending"), "").unwrap();

    let out = cp(&env, &["kill", &parent]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("still marked to stop"), "{err}");
    assert_eq!(mode(&run.join("stop")), 0o600);

    let inside = [("CLAUDE_PROXY_RUN", parent.as_str())];
    let child = stdout(&run_as(
        &env,
        exe,
        &["run", "--account", "claude", "build it"],
        &inside,
    ));
    cp(&env, &["wait", &child, "--timeout", "30"]);
    settled(&env, &child);
    assert!(run.join("stop").exists(), "a child resumed a stopping run");
    assert_eq!(status(&env, &parent)["queued"], 0);
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert!(
        !log.contains(&format!("(claude-proxy) Run {child}")),
        "{log}"
    );
}

#[test]
fn an_account_that_cannot_sign_in_hands_the_message_to_the_next_in_the_list() {
    let env = setup();
    std::fs::write(
        env.home.join(".config/claude-proxy/registry.json"),
        r#"{"labels":["acct-a","acct-b","acct-c"]}"#,
    )
    .unwrap();
    let id = stdout(&run_as(
        &env,
        Path::new(env!("CARGO_BIN_EXE_claude-proxy")),
        &["run", "--account", "acct-a,acct-c,acct-b", "hello"],
        &[("FAKE_LOGGED_OUT", "acct-a")],
    ));
    let out = cp(&env, &["wait", &id, "--timeout", "30"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        stdout(&cp(&env, &["read", &id]))
    );
    // It never reached acct-a, so it goes over as it was, with no notice.
    assert_eq!(stdout(&out), "echo: hello");

    let s = status(&env, &id);
    assert_eq!(s["account"], "acct-c");
    assert_eq!(s["moved"], "from acct-a, which could not sign in");
    let log = std::fs::read_to_string(&env.log).unwrap();
    let calls: Vec<Vec<&str>> = log.lines().map(|l| l.splitn(4, ' ').collect()).collect();
    assert_eq!(calls.len(), 1, "{log}");
    assert_eq!(calls[0][0], "new");
    assert_eq!(calls[0][1].len(), 36, "{log}");
    assert_eq!(calls[0][2..], ["acct-c", "hello"]);
    assert!(stdout(&cp(&env, &["read", &id]))
        .contains("⇄ acct-a could not sign in — continuing on acct-c"));
}

#[test]
fn an_error_that_is_not_the_accounts_does_not_move_the_session() {
    let env = setup();
    let id = stdout(&cp(
        &env,
        &["run", "--account", "acct-a,acct-b", "overloaded job"],
    ));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(1)
    );
    let s = status(&env, &id);
    assert_eq!(s["state"], "failed");
    assert_eq!(s["account"], "acct-a");
    assert!(s["moved"].is_null(), "{s}");
    let log = std::fs::read_to_string(&env.log).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(!stdout(&cp(&env, &["read", &id])).contains('⇄'));
}

#[test]
fn the_permit_server_lists_its_tool_and_denies_what_it_cannot_record() {
    let env = setup();
    let run = env.home.join(".config/claude-proxy/runs/abc123");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("asks"), "").unwrap();
    std::fs::set_permissions(run.join("asks"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
    let call = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"permit","arguments":{"tool_name":"Bash","input":{"command":"rm -rf build"},"tool_use_id":"t1"}}}"#;
    let replies = |lines: &[&str]| -> Vec<Value> {
        let out = cp_stdin(&env, &["__permit", "abc123"], &(lines.join("\n") + "\n"));
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let decision = |reply: &Value| -> Value {
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };

    let got = replies(&[
        init,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#,
        call,
    ]);
    let ids: Vec<&Value> = got.iter().map(|r| &r["id"]).collect();
    assert_eq!(ids, [1, 2, 3, 4], "{got:?}");
    let tool = &got[1]["result"]["tools"][0];
    assert_eq!(tool["name"], "permit");
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&"tool_name".into()) && required.contains(&"input".into()));
    assert_eq!(got[2]["error"]["code"], -32601);
    let d = decision(&got[3]);
    assert_eq!(d["behavior"], "deny");
    assert_eq!(d["message"], "claude-proxy could not record the request.");

    std::fs::remove_file(run.join("asks")).unwrap();
    std::fs::write(run.join("stop"), "").unwrap();
    let got = replies(&[init, call]);
    assert_eq!(got.len(), 2, "{got:?}");
    let d = decision(&got[1]);
    assert_eq!(d["behavior"], "deny");
    assert_eq!(d["message"], "The run was stopped.");
    let left = std::fs::read_dir(run.join("asks"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    assert_eq!(left, 0);
}

#[test]
fn a_permission_ask_reaches_its_session_and_only_a_question_takes_an_answer() {
    let env = setup();
    let session = "44444444-4444-4444-8444-555555555555";
    let id = stdout(&run_as(
        &env,
        Path::new(env!("CARGO_BIN_EXE_claude-proxy")),
        &["run", "--account", "claude", "ask first"],
        &[("CLAUDE_CODE_SESSION_ID", session)],
    ));
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(2)
    );
    let out = cp(&env, &["answer", &id, "yes"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not asking a question"), "{err}");

    let news = stdout(&hook(
        &env,
        "prompt",
        &format!(r#"{{"session_id":"{session}","permission_mode":"default"}}"#),
    ));
    assert!(
        news.contains(&format!(
            "- {id} is waiting for an answer: ? permission to use Bash"
        )),
        "{news}"
    );
    assert!(cp(&env, &["deny", &id, "no"]).status.success());
    assert_eq!(
        cp(&env, &["wait", &id, "--timeout", "30"]).status.code(),
        Some(0)
    );
}

#[test]
fn pruning_spares_queued_messages_live_children_and_kept_worktrees() {
    let env = setup();
    let repo = env.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("file.txt"), "x").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&repo)
            .env("HOME", &env.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);

    // Every run exists before any is aged: `run` prunes too.
    let start = |args: &[&str]| {
        let id = stdout(&cp(&env, args));
        cp(&env, &["wait", &id, "--timeout", "30"]);
        id
    };
    let done = start(&["run", "--account", "claude", "hello"]);
    let failed = start(&["run", "--account", "acct-a", "limited job"]);
    let parent = start(&["run", "--account", "claude", "--name", "orch", "plan it"]);
    let tree = start(&[
        "run",
        "--account",
        "claude",
        "--worktree",
        "--cwd",
        repo.to_str().unwrap(),
        "hi",
    ]);
    let child = stdout(&run_as(
        &env,
        Path::new(env!("CARGO_BIN_EXE_claude-proxy")),
        &["run", "--account", "claude", "sleep please"],
        &[("CLAUDE_PROXY_RUN", parent.as_str())],
    ));
    wait_until("the child works", || {
        status(&env, &child)["state"] == "working"
    });

    for id in [&done, &failed, &parent, &tree] {
        age(&env, id, 25 * 3600);
    }
    let inbox = env
        .home
        .join(format!(".config/claude-proxy/runs/{failed}/inbox"));
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("000000000000000000000001-00000000000000ab.txt"),
        "later",
    )
    .unwrap();

    cp(&env, &["runs"]);
    assert!(!cp(&env, &["status", &done]).status.success(), "not pruned");
    for id in [&failed, &parent, &tree] {
        assert!(
            cp(&env, &["status", id]).status.success(),
            "{id} was pruned"
        );
    }
    cp(&env, &["kill", &child]);
}
