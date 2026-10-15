use std::ffi::OsString;
use std::io::IsTerminal;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::asks::{Grant, Reply};
use crate::install;
use crate::paths::account_config_dir;
use crate::quota::{self, Report};
use crate::registry::Registry;

#[derive(Parser)]
#[command(
    name = "claude-proxy",
    version,
    about = "Run Claude Code under multiple accounts, each its own command."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an isolated profile, sign it in, and install it as a command.
    Add {
        /// The command name to create (e.g. `claude-personal`).
        label: String,
    },
    /// List every account with its remaining quota (cached for 15 minutes).
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Ignore the cache and fetch fresh figures.
        #[arg(long)]
        refresh: bool,
    },
    /// Run `claude` on the account with the most quota left. Every argument is
    /// forwarded unchanged, e.g. `claude-proxy auto -p "fix the test"`.
    #[command(disable_help_flag = true)]
    Auto {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Remove a proxy's command (its profile and login are kept).
    Remove {
        /// The command name to remove.
        label: String,
    },
    /// Start a background Claude session you can manage by id. Prints the id.
    ///
    /// Claude flags go after `--`, e.g.
    /// `claude-proxy run "fix the tests" -- --permission-mode acceptEdits`.
    /// Permission prompts and questions the run cannot settle wait for
    /// `allow`, `deny`, or `answer`; `wait` returns when one comes up.
    Run {
        /// The task. `-` reads it from stdin.
        message: String,
        /// `auto` picks the account with the most quota and, if it hits a
        /// usage limit, continues the session on the next best. A name pins
        /// one account; `a,b,c` tries them in order. `claude` is the primary.
        #[arg(long, default_value = "auto")]
        account: String,
        /// A label to recognise the run by.
        #[arg(long)]
        name: Option<String>,
        /// Working directory for the session (default: the current one).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// Block until it finishes and print its final answer.
        #[arg(long)]
        wait: bool,
        /// Print the run as JSON instead of just its id.
        #[arg(long)]
        json: bool,
        /// Start from a copy of the conversation of the Claude session this
        /// runs inside (`$CLAUDE_CODE_SESSION_ID`).
        #[arg(long)]
        fork: bool,
        /// Start from a copy of this Claude session's conversation.
        #[arg(long, value_name = "SESSION_ID")]
        fork_from: Option<String>,
        /// Work in a new git worktree on its own branch (`claude-proxy/<id>`).
        #[arg(long)]
        worktree: bool,
        /// Run as one of your Claude agents (`~/.claude/agents`), e.g. a GSD one.
        #[arg(long)]
        agent: Option<String>,
        /// Arguments for `claude`, after `--`.
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
    /// List managed runs, most recently active first.
    Runs {
        #[arg(long)]
        json: bool,
        /// Every run, not just this repository's going or recent ones.
        #[arg(long)]
        all: bool,
    },
    /// Show a run's state, current activity, and last result.
    Status {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print a run's transcript.
    Read {
        id: String,
        /// Only the last N entries.
        #[arg(short = 'n', long = "lines")]
        lines: Option<usize>,
        /// Keep printing until the run stops working.
        #[arg(short, long)]
        follow: bool,
        /// Whole tool inputs and outputs instead of one line each.
        #[arg(long)]
        full: bool,
        /// Raw stream-json events.
        #[arg(long)]
        json: bool,
    },
    /// The last entries of a transcript (`read -n 20`).
    Tail {
        id: String,
        #[arg(short = 'n', long = "lines", default_value_t = 20)]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
    },
    /// Follow a run live until it stops working (`read -n 10 -f`).
    Watch {
        id: String,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the final answer of a run's last turn.
    Result { id: String },
    /// Send a follow-up message; sent while it works, Claude folds it into the
    /// turn in progress.
    Send {
        id: String,
        /// The message. `-` reads it from stdin.
        message: String,
        /// Continue on other accounts: `auto`, a name, or `a,b,c` (as `run`).
        #[arg(long)]
        account: Option<String>,
    },
    /// Block until a run has nothing running or queued, then print its answer.
    /// Exits 0 when the last turn succeeded, 1 when it failed or was killed,
    /// 2 when it is waiting for an answer (printed), 124 on timeout.
    Wait {
        id: String,
        /// Give up after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Let a waiting run use the tool it asked for.
    Allow {
        id: String,
        /// And stop asking for requests like this one: the exact command, the
        /// domain, or the tool (for edits: accept edits).
        #[arg(long)]
        always: bool,
        /// And stop asking for requests matching this permission rule, e.g.
        /// `Bash(cargo test:*)`.
        #[arg(long, value_name = "RULE")]
        rule: Option<String>,
        /// And accept file edits without asking from now on (as when
        /// approving a plan in Claude).
        #[arg(long)]
        accept_edits: bool,
    },
    /// Refuse what a waiting run asked for; it reads your reason and carries on.
    Deny {
        id: String,
        /// What to tell it instead.
        reason: Option<String>,
    },
    /// Answer a waiting run's question, one answer per question, in order.
    Answer {
        id: String,
        #[arg(required = true)]
        answers: Vec<String>,
    },
    /// Open a run's session interactively in this terminal.
    Attach { id: String },
    /// Print a line whenever a run changes state (finishes, fails, asks,
    /// moves account), until interrupted. Made for a Monitor tool.
    Events {
        /// Only these runs.
        ids: Vec<String>,
        /// Only the runs this Claude session started, and what they started.
        #[arg(long)]
        mine: bool,
    },
    /// Stop a run's current turn. Queued messages are kept; `send` resumes.
    Kill { id: String },
    /// Delete a run that is not working, and its clean worktree.
    Rm { id: String },
    #[command(name = "__drain", hide = true)]
    Drain { id: String },
    #[command(name = "__permit", hide = true)]
    Permit { id: String },
    #[command(name = "__hook", hide = true)]
    Hook { event: String },
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("claude-proxy: {message}");
            1
        }
    }
}

fn dispatch(command: Command) -> Result<i32, String> {
    match command {
        Command::Add { label } => add_cmd(&label),
        Command::List { json, refresh } => list_cmd(json, refresh),
        Command::Auto { args } => auto_cmd(&args),
        Command::Remove { label } => remove_cmd(&label),
        Command::Run {
            message,
            account,
            name,
            cwd,
            wait,
            json,
            fork,
            fork_from,
            worktree,
            agent,
            claude_args,
        } => crate::agents::run(crate::agents::RunOpts {
            message,
            account,
            name,
            cwd,
            wait,
            json,
            fork: fork_from.or(fork.then(String::new)),
            worktree,
            agent,
            claude_args,
        }),
        Command::Runs { json, all } => crate::agents::runs(json, all),
        Command::Status { id, json } => crate::agents::status(&id, json),
        Command::Read {
            id,
            lines,
            follow,
            full,
            json,
        } => crate::agents::read(&id, lines, follow, full, json),
        Command::Tail {
            id,
            lines,
            follow,
            full,
            json,
        } => crate::agents::read(&id, Some(lines), follow, full, json),
        Command::Watch { id, full, json } => crate::agents::read(&id, Some(10), true, full, json),
        Command::Result { id } => crate::agents::result(&id),
        Command::Send {
            id,
            message,
            account,
        } => crate::agents::send(&id, &message, account.as_deref()),
        Command::Wait { id, timeout } => crate::agents::wait(&id, timeout),
        Command::Allow {
            id,
            always,
            rule,
            accept_edits,
        } => crate::agents::reply(
            &id,
            Reply::Allow(Grant {
                always,
                rule,
                accept_edits,
            }),
        ),
        Command::Deny { id, reason } => crate::agents::reply(
            &id,
            Reply::Deny(reason.unwrap_or_else(|| {
                "Declined. Carry on without it, or say what you need and why.".into()
            })),
        ),
        Command::Answer { id, answers } => crate::agents::reply(&id, Reply::Answer(answers)),
        Command::Attach { id } => crate::agents::attach(&id),
        Command::Events { ids, mine } => crate::agents::events(&ids, mine),
        Command::Kill { id } => crate::agents::kill(&id),
        Command::Rm { id } => crate::agents::rm(&id),
        Command::Drain { id } => crate::drainer::drain(&id).map(|()| 0),
        Command::Permit { id } => crate::asks::serve(&id).map(|()| 0),
        Command::Hook { event } => Ok(crate::hook::run(&event)),
    }
}

fn add_cmd(label: &str) -> Result<i32, String> {
    let self_exe = std::env::current_exe()
        .map_err(|e| format!("could not locate this binary to install: {e}"))?;
    let bin_dir = install::default_bin_dir();
    let interactive = std::io::stdin().is_terminal();

    let added = crate::add::run(&self_exe, &bin_dir, label, interactive)?;

    println!("\n✓ {} is ready.", added.label);
    println!("  installed: {}", added.bin_path.display());
    println!("  run it:    {} [any claude arguments]", added.label);
    if !added.on_path {
        println!(
            "\nNote: {} is not on your PATH. Add it to use the command directly:",
            bin_dir.display()
        );
        println!("  export PATH=\"{}:$PATH\"", bin_dir.display());
    }
    Ok(0)
}

fn list_cmd(as_json: bool, refresh: bool) -> Result<i32, String> {
    let reports = quota::reports(&quota::accounts(), refresh);
    let now = quota::now_secs();
    let pick = quota::choose(&reports, now);

    if as_json {
        let accounts: Vec<_> = reports
            .iter()
            .map(|r| {
                json!({
                    "label": r.label,
                    "primary": r.primary,
                    "email": r.email,
                    "logged_in": r.logged_in,
                    "pressure": r.pressure(now),
                    "windows": r.windows.iter().map(|w| json!({
                        "name": w.name,
                        "used": w.used_at(now),
                        "resets_at": w.resets_at,
                    })).collect::<Vec<_>>(),
                    "fetched_at": r.fetched_at,
                    "error": r.error,
                })
            })
            .collect();
        let out = json!({
            "pick": pick.map(|i| reports[i].label.clone()),
            "cache_ttl_secs": quota::CACHE_TTL_SECS,
            "accounts": accounts,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(0);
    }

    let names: Vec<String> = reports.iter().map(display_name).collect();
    let name_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let email_w = reports
        .iter()
        .map(|r| r.email.as_deref().unwrap_or("-").chars().count())
        .max()
        .unwrap_or(0);
    let bin_dir = install::default_bin_dir();
    for (i, r) in reports.iter().enumerate() {
        let marker = if pick == Some(i) { "→" } else { " " };
        let mut line = format!(
            "{marker} {:name_w$}  {:email_w$}  {}",
            names[i],
            r.email.as_deref().unwrap_or("-"),
            quota_text(r, now),
        );
        if !r.primary && !bin_dir.join(&r.label).exists() {
            line.push_str("  (command missing — `claude-proxy add` to reinstall)");
        }
        println!("{}", line.trim_end());
    }
    println!(
        "\n→ = what `claude-proxy auto` would use · (…) = time to reset · \
         cached {} min (--refresh to update)",
        quota::CACHE_TTL_SECS / 60
    );
    Ok(0)
}

fn auto_cmd(args: &[OsString]) -> Result<i32, String> {
    let reports = quota::reports(&quota::accounts(), false);
    let now = quota::now_secs();
    let pick = quota::choose(&reports, now)
        .ok_or("no logged-in account to use. Add one with:  claude-proxy add <name>")?;
    let r = &reports[pick];
    eprintln!("claude-proxy: auto → {} ({})", r.label, quota_text(r, now));
    if r.pressure(now).is_some_and(|p| p >= 100.0) {
        eprintln!("claude-proxy: warning — every account is at a usage limit");
    }
    if r.primary {
        crate::proxy::run_primary(args)
    } else {
        crate::proxy::run(&r.label, args)
    }
}

fn display_name(r: &Report) -> String {
    if r.primary {
        format!("{} (primary)", r.label)
    } else {
        r.label.clone()
    }
}

fn quota_text(r: &Report, now: i64) -> String {
    if !r.logged_in {
        return "not logged in".into();
    }
    if r.windows.is_empty() {
        return match &r.error {
            Some(e) => format!("quota unknown — {e}"),
            None => "quota unknown".into(),
        };
    }
    let mut text = r
        .windows
        .iter()
        .map(|w| match w.resets_at.filter(|t| *t > now) {
            Some(t) => format!(
                "{} {:.0}% ({})",
                w.name,
                w.used_at(now),
                quota::short_duration(t - now)
            ),
            None => format!("{} {:.0}%", w.name, w.used_at(now)),
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if let Some(at) = r.fetched_at.filter(|at| now - at >= quota::CACHE_TTL_SECS) {
        text.push_str(&format!(" [as of {} ago]", quota::short_duration(now - at)));
    }
    if let Some(e) = &r.error {
        text.push_str(&format!(" [{e}]"));
    }
    text
}

fn remove_cmd(label: &str) -> Result<i32, String> {
    let mut registry = Registry::load();
    if !registry.has(label) {
        return Err(format!("no proxy named {label:?}. See:  claude-proxy list"));
    }
    let bin_dir = install::default_bin_dir();
    install::uninstall(&bin_dir, label)
        .map_err(|e| format!("could not remove the {label:?} command: {e}"))?;
    registry.remove(label);
    registry
        .save()
        .map_err(|e| format!("could not update the registry: {e}"))?;

    println!("Removed the {label} command.");
    println!(
        "Its profile (and login) is kept at:\n  {}",
        account_config_dir(label).display()
    );
    println!("Delete that directory yourself if you want it gone.");
    Ok(0)
}
