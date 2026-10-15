# proxy-runs

A Claude Code mod (function-hooks plugin, Claude Code 2.1.287+) that makes
`claude-proxy` managed runs feel like local subagents.

- **A tool in place of Agent.** `mcp__proxy-runs__run` takes Agent's inputs
  (`prompt`, `description`, `subagent_type`, `model`, `isolation: "worktree"`,
  `cwd`, `name`), starts `claude-proxy run - --json …` with the task on stdin
  (`--agent` only for non-built-in types, `-- --model` for the model) and
  returns at once with the run id, its account and `claude-proxy watch <id>`.
- **A live row, drawn like Claude's own.** The tool's transcript row draws the
  run the way Claude draws a session: its name and `id · state on account ·
  turn · $`, the run's replies as Markdown, your messages to it, and its tool
  calls as Claude's own tool rows (the engine draws each one, Bash, Read, Edit
  and the rest, with the call's input and result). Consecutive calls fold into
  one line, "Ran 3 commands, read 2 files", that opens into the rows; the live
  group keeps its running call in view. Asks show Allow / Always allow / Deny
  and an answer field on the row. Collapsed, the row shows the last three
  blocks; `Show N earlier` opens the rest (last 400 events).
- **Any run, on request.** `mcp__proxy-runs__watch` takes a run id and draws
  that run's row in the conversation, another session's included; with no id
  it draws every run going in the repository. A run id is six lowercase hex
  characters, as `claude-proxy run` prints it; a malformed id is refused. The
  policy section (and quota-router's) tells the model to call it whenever you
  ask to see, show, watch or follow runs. `/runs <id>` opens the pane on that run.
- **Runs started through Bash.** A `claude-proxy run` Bash row gets the run's
  row under the command's own, from the id the command printed.
- **The notice reads as one line.** The prompt the mod submits when a run
  finishes is drawn as `✓ <name> finished · <id> on <account>` and the run's
  answer as Markdown, not the raw `<run-result>` text the model reads.
- **A Runs pane.** `/runs` opens it. A run picker (a native dropdown where the
  surface has one) stays at the top; under it, Talk to it / Stop, then the run's
  transcript with its asks (Allow / Always allow / Deny / Answer). The pane cannot
  borrow Claude's tool rows, so its calls are the mod's own: a title that opens
  to the command and its output, or the edit as a diff.
- **Talk to a run from the prompt box.** Message it (pane), Reply (a run's row)
  or Answer (an ask) points Claude's own prompt box at that run for one message,
  and a band above the prompt says which run, with Back to this chat. That
  message is sent to the run (`claude-proxy send`), or answers its question
  (`claude-proxy answer`, `|` between answers); your skills and custom commands
  (`/name args`) are sent to the run as typed, so they run there. Built-in
  commands stay in this session. Attachments are not sent.
- **A status line.** `2 runs · 1 waiting` while any run is active.
- **Notifications without polling.** At session start the mod follows
  `claude-proxy events --mine`. When a run it started finishes, fails, is
  killed or asks something, it appends a notice row and submits a prompt with
  the result or the question, which starts a turn once the session is idle,
  like a background subagent's task notification.
- **Agent calls redirected.** A `tool.call` hook on Agent (and Task) refuses
  the call and points to the tool, unless the description starts with
  `[direct]`, `CLAUDE_PROXY_POLICY=off`, or `CLAUDE_PROXY_RUN` is set.
- **Policy.** A short system-prompt section says to delegate with the tool
  rather than `claude-proxy run` through Bash, and that `claude-proxy`
  status / answer / allow through Bash still apply.

Inside a run the mod does nothing at all: no tool, no watcher, no redirect, no
policy section, no marker.

## Enable

The folder comes from a clone of this repository
(`git clone https://github.com/ohmaseclaro/claude-proxy`); `/path/to/claude-proxy`
below stands for that clone.

Terminal, one session:

```console
$ claude --plugin-dir /path/to/claude-proxy/mods/proxy-runs
```

Every session, the desktop app's Code tab included: add the folder to
`CLAUDE_CODE_PLUGIN_DIRS` in the `env` block of `~/.claude/settings.json`:

```json
{
  "env": {
    "CLAUDE_CODE_PLUGIN_DIRS": "/path/to/claude-proxy/mods/proxy-runs",
    "CLAUDE_CODE_PLUGIN_DIR_WATCH": "1"
  }
}
```

A terminal session reloads the mod when its files change. A desktop session is
a long-lived headless one and reloads only with `CLAUDE_CODE_PLUGIN_DIR_WATCH=1`.
Both variables are read when a session's process starts, so a session open
before they were set needs one restart (quit and reopen the app; conversations
resume) to load the mod or start watching it.

Runs inherit that setting (every profile shares `settings.json`), which is why
the mod checks `CLAUDE_PROXY_RUN` and stays inert inside them.

Needs `claude-proxy` 0.8.0 or later on `PATH`, because the mod lists runs with
`claude-proxy runs --json --all`. Install it as in the main README
(`cargo install --git https://github.com/ohmaseclaro/claude-proxy`).

## With the quota-router plugin

The two can be enabled together:

- The mod's Agent refusal runs in `tool.call`, above the `PreToolUse` settings
  hooks, so its message (pointing to `mcp__proxy-runs__run`) is the one the
  model gets; quota-router's shell hook never sees the call.
- At session start (and at each prompt, which covers `/clear`) the mod writes
  `~/.config/claude-proxy/sessions/<session_id>.mod` with `on`. While that
  marker reads `on`, `claude-proxy __hook prompt` does not report runs (the mod
  already does) and `__hook pre-tool-use` does not redirect. `session-start`
  still lists runs in progress. At session end the mod removes the marker
  (`rm`), or writes `off` where it cannot start a process, since `$.fs` has no
  delete.
- quota-router's SessionStart policy text still tells the model to use
  `claude-proxy run` through Bash; the mod's policy section, later in the
  prompt, says to use the tool instead.

## Limits

- **No native tasks dialog.** The mod API cannot add entries to Claude Code's
  background-tasks dialog; the Runs pane stands in for it.
- **No ctrl+o expansion on the row.** `ToolUse` props carry no `isExpanded` on
  2.1.288 (only `UserMessage` and `ToolGroup` do), so the row has its own
  `Show N earlier` button.
- **Group lines are Buttons.** A mod cannot draw Claude's `ToolGroup`, so the
  "Ran 3 commands" line is a plain, dim Button; the desktop draws it as its
  own button.
- **One row id.** The engine rows inside a run's row all carry that row's
  `tool_use_id` (it is read-only), so a surface that keys row state on it may
  open or close them together.
- **Process access.** `$.process` is declared "CLI only". The desktop Code tab
  runs the CLI, so it should work there, but that is not confirmed. If the
  engine refuses it, the tool refuses with a clear reason, the pane and row
  show a yellow line saying so and read `meta.json` / `events.jsonl` from
  `~/.config/claude-proxy/runs` instead (no asks, re-read every 3 s), and the
  pane's buttons tell you the command to type in a terminal.
- **Headless.** Under `claude -p` the session ends with its turn, so the
  wake-up prompt only arrives in an interactive session or a long-lived SDK /
  stream-json one.
- **`/clear`.** The events watcher keeps following the session id it started
  under; runs started after a `/clear` show up once the session restarts.
- Notifications cover runs started through the tool, and runs this session
  watches. Runs started through Bash still show in the pane and status line;
  wait on those as before.
- A row whose run this session never loaded (a resumed session, an older
  conversation) loads it once when drawn; a run already pruned shows its id
  only.

## Develop

```console
$ claude plugin validate mods/proxy-runs --strict
$ claude plugin test mods/proxy-runs
$ npx -y -p typescript@5 tsc -p mods/proxy-runs --noEmit
```

tsc needs the git-ignored types a session writes when it loads the mod; see
[CONTRIBUTING.md](../../CONTRIBUTING.md) for how to regenerate them.
