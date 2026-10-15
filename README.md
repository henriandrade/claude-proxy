# claude-proxy

[![CI](https://github.com/ohmaseclaro/claude-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/ohmaseclaro/claude-proxy/actions/workflows/ci.yml) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Run [Claude Code](https://docs.claude.com/en/docs/claude-code) on several accounts at once.
Each account gets its own command, `claude-proxy auto` starts on whichever has the most quota left,
and background runs your agents manage by id move to another account when one hits its limit.

```console
$ claude-proxy add claude-personal     # creates a profile, logs it in, installs a `claude-personal` command
$ claude-personal                      # a full Claude Code session on that account
$ claude-proxy list                    # every account and its remaining quota
$ claude-proxy auto -p "fix the test"  # runs on the account with the most room
$ claude                               # your original login is untouched
```

<!--
Visuals: capture these before launch, save them under docs/images/, then replace
this comment with a "## Screenshots" section.

1. docs/images/list.png: `claude-proxy list` with the primary `claude` login,
   claude-personal and claude-work (example.com emails), the → pick marker and
   reset times.
2. docs/images/watch-failover.png: `claude-proxy watch <id>` on a run that hits a
   usage limit on claude-work and carries on on claude-personal, showing the ⇄
   line, then ✓ done.
3. docs/images/mod-runs-pane.png: the proxy-runs Runs pane (`/runs`) with the run
   picker at the top, Talk to it / Stop, and a run's transcript.
4. docs/images/mod-ask-row.png: a run's row in the conversation with a pending
   permission ask showing Allow / Always allow / Deny.
5. docs/images/mod-talk-band.png: the band above Claude's prompt box while
   talking to a run, with Back to this chat.

Use generic labels and example.com emails in every shot.
-->

## Why

Claude Code keeps one logged-in account per machine, and each account's quota
runs out on its own schedule. Working across a personal and a work account
means logging in and out.

claude-proxy gives each account its own command and its own Claude profile.
Logins stay independent, while your skills, plugins, hooks, settings,
`CLAUDE.md` and transcripts are shared, so every account behaves like your
normal `claude`. Managed runs let an agent hand work to background sessions it
controls like subagents, spread across your accounts.

## Install

Requires macOS or Linux (see [Platform support](#platform-support)) and Claude
Code on your `PATH`.

### 1. The claude-proxy command

```console
$ cargo install --git https://github.com/ohmaseclaro/claude-proxy
```

Needs Rust 1.89+. The `claude-proxy` crate on crates.io is a different
project, so install from Git.

The manager lands in `~/.cargo/bin`. The per-account commands it creates go in
`~/.local/bin`, so put that on your `PATH` too (`add` reminds you if it is
missing):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

To upgrade, run the same command with `--force`. The per-account commands
follow, because they are symlinks to the manager. From a clone, use
`cargo install --path .`.

### 2. The quota-router plugin (for agents)

```console
$ claude plugin marketplace add ohmaseclaro/claude-proxy
$ claude plugin install quota-router@quota-router
```

It adds a skill that teaches agents to use managed runs, and hooks that make
managed runs the default for subagent-style work. It needs the command from
step 1; without it, its hooks stay silent. Install it once: every profile sees
it, since plugins are shared.

### 3. The proxy-runs mod (optional)

Needs Claude Code 2.1.287+ and claude-proxy 0.8.0+. The mod lives in this
repository, so clone it:

```console
$ git clone https://github.com/ohmaseclaro/claude-proxy
$ claude --plugin-dir /path/to/claude-proxy/mods/proxy-runs   # one session
```

For every session, the desktop Code tab included, set `CLAUDE_CODE_PLUGIN_DIRS`
to that folder and `CLAUDE_CODE_PLUGIN_DIR_WATCH` to `"1"` in the `env` block of
`~/.claude/settings.json` (the JSON is in the [mod README](mods/proxy-runs/README.md),
along with what the mod does and its limits).

## Quickstart

```console
$ claude-proxy add claude-personal
$ claude-proxy add claude-work
```

Each `add` opens Claude's own login in a fresh profile. Log in as a different
account each time, then type `/exit`.

```console
$ claude-proxy list
  claude (primary)  you@example.com       5h 40% (2h10m) · 7d 61% (3d4h)
→ claude-personal   personal@example.com  5h 17% (3h34m) · 7d 22% (3d13h)
  claude-work       work@example.com      5h 0% · 7d 95% (4d8h)

→ = what `claude-proxy auto` would use · (…) = time to reset · cached 15 min (--refresh to update)
```

```console
$ claude-proxy auto -p "summarize the failing tests"
claude-proxy: auto → claude-personal (5h 17% (3h34m) · 7d 22% (3d13h))
```

`auto` forwards every argument to `claude`. Its pick goes to stderr, so stdout
stays clean for `-p` output.

```console
$ id=$(claude-proxy run "fix the failing test in src/parser.rs")
$ claude-proxy watch $id                                 # follow it live
$ claude-proxy send $id "add a regression test for it"   # same conversation
$ claude-proxy wait $id                                  # blocks, prints the final answer
```

```console
$ claude-proxy remove claude-work
```

`remove` deletes the command and keeps the profile and its login; it prints
where the profile is so you can delete it yourself.

## How it works

### Accounts

One binary, two roles. claude-proxy looks at `argv[0]`: under its own name it is
the manager, under a name it installed it is the proxy. Each command is a
symlink to the manager.

Each account is its own `CLAUDE_CONFIG_DIR` under
`~/.config/claude-proxy/accounts/<name>` (or `$XDG_CONFIG_HOME/claude-proxy`),
and Claude's own login stores its credentials. An allowlist of `~/.claude`
entries (`settings.json`, `CLAUDE.md`, `skills`, `agents`, `commands`,
`plugins`, `hooks`, `projects` and similar) is linked into each profile. A copy
a profile made before an entry was shared is merged in or moved aside as
`<name>.pre-shared`, never deleted. User and local MCP servers live next to the
login in `~/.claude.json`, so each launch passes them with `--mcp-config` from
owner-only files. The proxy clears inherited credentials and any host app's
session identity, then execs `claude`.

The primary `~/.claude` login takes part in `list` and `auto` whenever it is
logged in. `auto` picks the account under the least pressure: the fullest usage
window (5-hour, 7-day, or a model-scoped weekly cap) that has not reset yet, ties
going to the emptier 7-day window. It chooses once, at launch.

### Quota

claude-proxy asks the same OAuth usage endpoint Claude Code uses. When a token
is about to expire it refreshes it under a lock and writes it back to Claude's
own store. Results are cached for 15 minutes per account (`list --refresh`
skips the cache, `list --json` is machine-readable), and live usage reported
during runs updates the cache. The approach is adapted from
[ai-usagebar](https://github.com/akitaonrails/ai-usagebar); see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

### Managed runs

`run` starts Claude in the background on the best account and prints an id.
From then on the session is yours to manage, like a subagent:

```console
$ claude-proxy watch $id
▶ you
  fix the failing test in src/parser.rs
● claude-personal · turn 1
  ⚙ Bash  cargo test parser
    ↳ test result: FAILED. 11 passed; 1 failed  (+38 lines)
  ⚙ Edit  src/parser.rs
  Fixed the off-by-one in `split_header`; the suite passes now.
✓ done · 6 steps · 48s · $0.31
```

| | |
|---|---|
| `claude-proxy run "<task>" [-- <claude flags>]` | start; `-` reads the task from stdin. `--account` (`auto`, a name to pin, or `a,b` in order), `--agent <type>`, `--worktree`, `--fork` / `--fork-from <session>`, `--name`, `--cwd`, `--wait`, `--json` |
| `claude-proxy status <id>` | state, what it is doing, what it is asking, Claude's summary of its last turn, cost (`--json`) |
| `claude-proxy read <id>` | the transcript (`-n N`, `-f` to follow, `--full`, `--json` raw events) |
| `claude-proxy tail <id>` / `watch <id>` | the last entries / follow live until it stops working |
| `claude-proxy result <id>` | the final answer of the last turn |
| `claude-proxy send <id> "<message>"` | a follow-up; sent while it works, Claude folds it into the turn in progress. `--account` moves the run to other accounts |
| `claude-proxy wait <id>` | block until it is done (exit 0 done, 1 failed or killed, 2 waiting for an answer, 124 on `--timeout`) |
| `claude-proxy allow <id>` / `deny <id> ["why"]` | settle a permission prompt; `allow --always`, `--rule "<rule>"` or `--accept-edits` also stop it asking again |
| `claude-proxy answer <id> "<answer>"…` | answer its question, one answer per question |
| `claude-proxy attach <id>` | open the session interactively in your terminal |
| `claude-proxy kill <id>` | stop the current turn (the conversation is kept; `send` resumes it); on an idle run, free its Claude process |
| `claude-proxy runs` / `rm <id>` | this repository's runs, as a tree of who started what (`--all` for every run) / delete one and its clean worktree |
| `claude-proxy events [<id>…] [--mine]` | a line each time a run finishes, fails, asks or moves account; made for a Monitor tool |

Each run pins one Claude session, so the conversation carries over across
Claude processes and onto other accounts, since transcripts are shared. A
background process keeps a headless stream-json Claude open for the run and
keeps it warm for `CLAUDE_PROXY_IDLE_SECS` seconds after a turn (default 300),
so a message sent mid-turn steers the work in progress and a quick follow-up
skips Claude's startup. Then both exit, so an idle run costs nothing.

### Failover

When the run's account hits a usage limit or cannot sign in, the same session
resumes on the next account. `--account auto` (the default) moves by quota,
`--account a,b` tries the list in order, and a single name pins the run. Each
account is tried at most once per message. If the failed attempt never reached
the session, the message is sent again; otherwise the new account is asked to
carry on. The transcript marks each move with `⇄`, and `status` shows where it
moved from. Overloaded or server errors are not the account's fault and do not
move the run.

### Questions and permission prompts

When a run needs a decision, a tool its permission mode does not allow or a
question Claude asks with AskUserQuestion, it waits: `status` shows `waiting`,
and `wait` exits 2 and prints the ask.

```console
$ claude-proxy wait $id
? permission to use Bash  cargo publish --dry-run
    claude-proxy allow 4f2a9c   or   claude-proxy deny 4f2a9c "<reason>"
$ claude-proxy allow $id && claude-proxy wait $id
```

`deny` with a reason tells Claude what to do instead; `answer "…"` answers a
question. `allow` can grant more than the one request:

- `--always` grants the exact Bash command, the WebFetch host, or the tool (for
  edits, accepting edits). It refuses a Bash command containing `*`, `(` or `)`,
  because a permission rule reads those as syntax and would allow more than that
  command. Allow it once, or choose the rule yourself with
  `--rule "Bash(cargo test:*)"`.
- `--accept-edits` accepts file edits from now on, as when approving a plan.

Grants last for the run. A run starts with the permission mode of the session
that started it (the plugin's hook records it), unless you pass one after `--`.
Asks wait up to a day.

### Worktrees, forks, agents

`--worktree` puts the run in a new git worktree on branch `claude-proxy/<id>`;
`rm` removes the worktree if it is clean and the branch if it is merged.
`--fork` starts from a copy of the calling session's conversation
(`--fork-from <session>` for any other). It re-reads the whole conversation, on
another account without the prompt cache. `--agent <type>` runs as one of your
agents in `~/.claude/agents`.

### Runs that start runs, and waiting

A run started inside a run reports to it: the parent is woken when a child
finishes, fails or asks something (a parent that is stopping is not). A long
command a run starts in the background keeps its Claude alive until it ends. In
your own session, a background `claude-proxy wait` or a Monitor on
`claude-proxy events --mine` notifies the agent, and the plugin's hooks report
finished or asking runs at each prompt.

### Old runs

`runs` shows this repository's runs that are going or finished in the last 2
hours; `--all` shows every run. Finished runs unused for
`CLAUDE_PROXY_KEEP_HOURS` (default 24; `0` keeps all) are deleted when `run` or
`runs` is used. A run that is going, has queued messages, kept a worktree, or
started a run still going is never deleted. Looking one up afterwards says how
to resume its Claude session, which stays.

### The quota-router plugin

Its SessionStart hook adds the delegation policy to each session, ahead of
workflows that say to use the Agent tool. An Agent or Task call that slips
through is turned back with the equivalent `claude-proxy run`, unless its
description starts with `[direct]`. `CLAUDE_PROXY_POLICY=off` keeps the skill
and drops the default. Inside a run the hooks stay silent.

Agents call claude-proxy through Bash, so each call asks for permission unless
you add `"Bash(claude-proxy:*)"` to `permissions.allow`. That also lets an agent
approve its runs' permission prompts, so allow it only if you would let the
agent take those actions itself.

### The proxy-runs mod

A Claude Code mod that gives agents a tool in place of Agent and draws each run
as a live row like Claude's own, with its asks on the row. It adds a Runs pane,
lets you talk to a run from Claude's prompt box, and notifies the session when
a run finishes or asks, without polling. See its
[README](mods/proxy-runs/README.md).

## Platform support

macOS and Linux are supported, and CI runs the test suite on both. Credentials
live in the login Keychain through `/usr/bin/security` on macOS and in
`<profile>/.credentials.json` on Linux.

Windows is not supported: runs, `kill` and the plugin's hooks rely on Unix
process groups and signals and on `sh`. WSL may work but is untested.

## Known limits

- `auto` chooses once. Use `run --wait` for a one-shot that should survive a
  usage limit.
- Each account asks once to trust a folder the first time you open it
  interactively there. Runs never ask.
- `claude-personal mcp list` does not show the MCP servers claude-proxy passes
  in, though sessions have them. MCP servers that need a sign-in ask once per
  account.
- Quota comes from the OAuth usage endpoint Claude Code itself calls, and runs
  drive Claude Code's headless stream-json mode and `--permission-prompt-tool`.
  A Claude Code release can change either.
- The mod's limits are in its [README](mods/proxy-runs/README.md).

## Security

- Logins go through Claude's own flow, and Claude stores each account's
  credentials as it does for a normal login.
- Tokens are sent only to Anthropic's usage and token endpoints and are never
  printed. They stay off command lines, with one rare macOS exception described
  in SECURITY.md.
- Run state lives in owner-only directories (0700) and files (0600) under
  `~/.config/claude-proxy`.
- A run's asks are answered only through `allow`, `deny` and `answer`, by
  whoever can write to that directory.

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md),
not in a public issue.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and the checks to run, the
[Code of Conduct](CODE_OF_CONDUCT.md), and [CHANGELOG.md](CHANGELOG.md).

## License

MIT, see [LICENSE](LICENSE). Code adapted from ai-usagebar is credited in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
