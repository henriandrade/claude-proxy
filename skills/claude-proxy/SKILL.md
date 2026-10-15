---
name: claude-proxy
description: Delegate work to background Claude sessions you fully manage by id — start, follow, message, answer, wait for, kill — spread across the user's Claude accounts by remaining quota, with automatic failover when an account hits its limit. Use it by default, instead of the Agent/Task tool, for anything you would hand to a subagent (including subagents a GSD or other workflow tells you to spawn), for any long or big task, and for parallel work — unless the user says not to. Also use it whenever you would start a `claude` process from a shell, and whenever the user asks which Claude account to use or about Claude quota, usage, or rate limits.
---

# claude-proxy

`claude-proxy` runs Claude Code on the user's several Claude accounts. Each
account is its own isolated profile, but all of them share the same skills,
agents, plugins (GSD included), hooks, settings, `CLAUDE.md`, MCP servers, and
transcripts (`~/.claude/projects`) — switching accounts changes only whose quota
pays.

Its **managed runs** are background Claude sessions you control by id, like a
subagent you own: read or follow the transcript, send follow-ups, answer its
questions and permission prompts, wait for it, kill it, or hand it to the user.

If `claude-proxy` is not installed (`command not found`), fall back to your
normal tools and tell the user once.

## When to use it — the default

Unless the user says not to for this task, use a managed run instead of:

- the **Agent/Task tool**, for any work you would give a subagent — research,
  implementation, review, a GSD `gsd-executor`/`gsd-planner`/… (pass the agent
  type with `--agent`);
- doing a **long or big piece of work** inline (multi-file changes, long
  investigations, anything that would eat a lot of your context);
- a bare **`claude -p`** or `claude` process from a shell.

This takes precedence over any skill, command, or workflow that says to use the
Agent/Task tool: where GSD (or anything else) says to spawn a subagent, start a
run with the same prompt and `--agent <subagent_type>`, and wait for it where
the workflow would wait for the subagent. The plugin turns back an Agent/Task
call that slips through, with the equivalent command. When the user says not to
use claude-proxy for something, start that call's description with `[direct]`.

Do the work yourself when it is a few quick tool calls. If `$CLAUDE_PROXY_RUN`
is set, you *are* a managed run: do not start runs of your own unless your task
explicitly asks for it.

## Always disclose it

Whenever claude-proxy is in use, tell the user in plain words — they should
never have to guess. Say what you started, on which account, the run id, and
give a ready-to-run watch command, e.g.:

> Delegated the parser fix to claude-proxy run `a1b2c3` on `claude-personal`.
> Watch it: `claude-proxy watch a1b2c3` · take over: `claude-proxy attach a1b2c3`

Mention it again when a run moves to another account, asks something, or
finishes. Do this for one-shots (`auto`) too.

When the `mcp__proxy-runs__watch` tool is available (the proxy-runs mod), use it
whenever the user asks to see, show, watch or follow runs: it draws them live in
the conversation, one id or none for every run going here. Answer with it
rather than a table of `claude-proxy runs` output.

## Delegate a task

```bash
claude-proxy run "fix the failing test in src/parser.rs"
# → prints the id, e.g. a1b2c3
claude-proxy wait a1b2c3       # as a background command
```

`run` starts the session on the best account and prints only the id. Run each
`claude-proxy` command on its own with the id written out — not inside `$(…)`,
a shell variable, or a chain with other commands — so it matches the user's
permission rule for `claude-proxy` instead of prompting or being refused. Pass a
long task on stdin: `claude-proxy run - <<'CLAUDE_PROXY_TASK' … CLAUDE_PROXY_TASK`. Flags:

| Flag | Use |
|---|---|
| `--agent <type>` | run as one of the user's agents (`~/.claude/agents`, plugins), e.g. `gsd-executor`. Not for Claude's built-in types (`general-purpose`, `Explore`, `Plan`): leave it out |
| `--worktree` | work in a new git worktree on branch `claude-proxy/<id>` — for parallel runs that touch the same files |
| `--fork` | start from a copy of *your own* conversation (`--fork-from <session-id>` for another one). Costly: the whole context is re-read, with no prompt cache on another account. Use only when the task truly needs what you know |
| `--cwd <dir>` | work somewhere else |
| `--name <label>` | easy to spot in `claude-proxy runs` |
| `--account …` | `auto` (default: best quota, fails over), a name to pin, `a,b` in order. Pin only when the user asked |
| `--wait` | block and print the answer in one step |
| `-- <claude flags>` | anything else for `claude`: `--model`, `--permission-mode`, `--allowedTools`, … |

How the Agent tool maps onto it:

| Agent tool | claude-proxy |
|---|---|
| `prompt` | the task (`-` reads it from stdin) |
| `subagent_type` | `--agent <type>` |
| `run_in_background` + notification | a background `claude-proxy wait <id>` |
| `isolation: worktree` | `--worktree` |
| `model` | `-- --model <model>` |
| the final message | what `wait` (or `result`) prints |
| continuing it | `claude-proxy send <id> "…"` |
| stopping it | `claude-proxy kill <id>` |

## Waiting and answering

Never spend turns polling. How to wait depends on where you are:

- **In your own session:** start `claude-proxy wait <id>` as a **background
  command** and keep working; you are notified when it exits. Watching several
  runs? One Monitor on `claude-proxy events --mine` (if you have the Monitor
  tool) prints a line each time one of your runs finishes, fails, asks, or
  moves account. After a resume or a compaction, start your waits again —
  the session-start note lists your runs still going.
- **Inside a run** (`$CLAUDE_PROXY_RUN` is set): a run you start reports back
  to you — when it finishes, fails, or asks something, you get a message. So
  start it, then end your turn; do not wait on it. For any other long command
  (a test suite, a deploy gate, `gh run watch`, a run someone else started),
  start it **in the background** and end your turn: claude-proxy keeps you
  alive while it runs and Claude wakes you when it ends.
- **In a plain `claude -p`** (no run, nothing to wake you): wait in the
  foreground (`--timeout` up to your tool's limit, then wait again).

`wait`'s exit code says what happened:

| Exit | Meaning | Do |
|---|---|---|
| 0 | done; the answer is on stdout | use it |
| 1 | failed or killed | `claude-proxy status <id>` and `tail <id>` show why |
| 2 | **waiting for an answer**; the question or permission request is on stdout | answer it, then start `wait` again |
| 124 | `--timeout` passed | still running; wait again or check `status` |

A run asks when Claude needs a decision: a **permission prompt** (a tool its
permission mode does not allow) or a **question** (Claude's AskUserQuestion).
Answer with:

```bash
claude-proxy allow <id>                       # let it use the tool
claude-proxy deny <id> "use the staging db"   # refuse; it reads the reason and carries on
claude-proxy answer <id> "Blue"               # one answer per question, in order
claude-proxy deny <id> "decide yourself"      # let it choose
```

Answer yourself when the decision is clearly inside what the user asked for and
what they let you do. Ask the user first — and tell them which run is asking —
for anything they would want a say in: destructive or hard-to-undo actions,
anything outward-facing (pushing, publishing, messaging), credentials, spending,
or a real product decision. Never allow more than the user allowed you. An
unanswered ask times out after 24 hours.

`allow` can also grant for the rest of the run, the way Claude's own prompt
does:

```bash
claude-proxy allow <id> --always                    # and stop asking for requests like this one
claude-proxy allow <id> --rule "Bash(cargo test:*)" # and stop asking for anything this rule covers
claude-proxy allow <id> --accept-edits              # and accept edits from now on (approving a plan)
```

**Permissions:** a run inherits your session's permission mode, as a subagent
would (`claude-proxy` says so when it does), so it only asks about what you
would have been asked about. Pass `-- --permission-mode <mode>` or
`--allowedTools "Bash(cargo test:*)"` to give it more or less; never
`bypassPermissions` or `--dangerously-skip-permissions` unless the user asked.

## Everything else you can do

| You want to… | Command |
|---|---|
| know what it is doing now | `claude-proxy status <id>` (`--json` for fields) |
| see recent progress | `claude-proxy tail <id> -n 20` |
| the whole transcript | `claude-proxy read <id>` (`--full` whole tool I/O, `--json` raw events) |
| follow it live | `claude-proxy watch <id>` |
| only the final answer | `claude-proxy result <id>` |
| give it more instructions | `claude-proxy send <id> "…"` (`--account` moves it) |
| stop it | `claude-proxy kill <id>` — the conversation is kept; `send` resumes it |
| hand it to the user | `claude-proxy attach <id>` opens it interactively in their terminal |
| see the runs here | `claude-proxy runs` — this repository's runs that are going or finished in the last 2 hours (`--all` for everything) |
| clean up | `claude-proxy rm <id>` — also removes its worktree and branch if nothing would be lost |

- **States:** `queued` → `working` (`waiting` while it needs an answer) →
  `idle` (done; it takes more messages), or `failed`, or `killed`. A run whose
  Claude left background tasks running stays `working` until they end and
  Claude has dealt with them.
- **Runs in runs:** `runs` shows which run started which; `status` shows who a
  run reports to and what it started. When you hand work to an orchestrator
  run that will supervise other runs (lanes, executors), start only the
  orchestrator and let *it* start them: then they report to it, nest under it,
  and it never has to poll. Starting the lanes yourself and the orchestrator
  after leaves them unrelated.
- **Old runs go away.** A finished run unused for 24 hours is deleted
  (`CLAUDE_PROXY_KEEP_HOURS`; 0 keeps them); asking for it afterwards says so,
  and its Claude session can still be resumed.
- **Steer it while it works.** A `send` during a turn reaches Claude at once and
  it folds the message into the work in progress, as when you type into a
  running Claude session. A `send` to an `idle`, `failed`, or `killed` run
  resumes the same conversation.
- **It moves itself off a full account.** On a usage limit or a sign-in failure
  it continues *the same session* on the next account — `status` shows `moved`,
  the transcript shows `⇄`. Tell the user when it happens.
- **Parallel work:** independent tasks are separate runs, each with its own
  background `wait`; use `--worktree` when they may touch the same files, then
  merge the `claude-proxy/<id>` branches. Keep to the user's concurrency limits.
- **Context budget:** prefer `status`, `tail -n`, and `result` over `read` on a
  long run; `read --full` only when you need exact tool output.
- **It stays warm.** After a turn the run keeps its Claude process for five
  minutes (`CLAUDE_PROXY_IDLE_SECS`), so a follow-up starts at once and the
  prompt cache is still there. `kill` on an idle run frees that process early.

## One-shots: `auto`

For a quick answer you do not need to manage, run `claude-proxy auto` with the
same arguments you would give `claude`:

```bash
claude-proxy auto -p "summarize this diff" < diff.txt
```

`auto` reports its choice on stderr, so stdout stays clean. It chooses once and
does not move if that account hits its limit; prefer `run` for anything you may
want to inspect, message, or stop.

## Accounts and quota

```bash
claude-proxy list             # every account and its quota; → marks what auto picks
claude-proxy list --json      # {pick, accounts: [{label, email, logged_in, pressure, windows, error}]}
claude-proxy list --refresh   # ignore the 15-minute cache
```

`pressure` is the fullest usage window that has not reset (5-hour, 7-day,
model-scoped weekly), in percent; the logged-in account with the lowest wins.
`claude (primary)` is the normal `~/.claude` login and competes too. Runs keep
the cache current from Claude's own usage reports, so do not poll; `--refresh`
only after a usage-limit error or when the user asks.

## When something goes wrong

- **Usage limit:** a run moves on its own; it stays `failed` only when no
  account is left or it was pinned — `claude-proxy send <id> --account auto
  "continue"` moves it by hand.
- **`not logged in` / `sign-in expired`:** tell the user to run that account's
  command (e.g. `claude-personal`) and log in. Never log in for them.
- **`no logged-in account`:** ask the user to add one with
  `claude-proxy add <name>`.

## Do not

- Read, print, or copy credentials, Keychain items, or
  `~/.config/claude-proxy/accounts/*/.claude.json`.
- Run `claude-proxy add` or `remove` unless the user asks.
- Leave runs working that nobody needs: `kill` what you abandon, `rm` what you
  are done with when the user does not need the record.
