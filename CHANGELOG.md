# Changelog

All notable changes to this project are documented in this file, in the
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) format.

No version has been tagged. Each section is dated by the commit that bumped the
version in `Cargo.toml` (the claude-proxy binary) or `.claude-plugin/plugin.json`
(the quota-router plugin). Where only one of them bumped, the section says so,
and it lists everything committed since the previous bump. The proxy-runs mod
has its own version and first shipped in 0.8.0.

## [Unreleased]

### Added

- proxy-runs mod: a run picker at the top of the Runs pane, with Talk to it and
  Stop.
- proxy-runs mod: talk to a run from Claude's prompt box, for one message or one
  forwarded command, with a band above the prompt and Back to this chat.
- proxy-runs mod: blank rows between a run's blocks.
- SECURITY.md, CONTRIBUTING.md, CODE_OF_CONDUCT.md, this changelog, issue forms,
  a pull request template and THIRD_PARTY_NOTICES.md.
- Tests for the risky paths (failover by sign-in failure and account order,
  errors that do not move a run, the permission server, asks, pruning) and for
  the plugin's hook scripts. CI validates the plugin and the mod, runs the mod's
  tests, type-checks it with tsc and runs shellcheck.
- Complete crate metadata, with an include list.

### Changed

- `allow --always` refuses Bash commands containing `*`, `(` or `)` and points
  to `--rule`. Nothing is granted and the ask stays pending.
- `allow --always` on WebFetch records the bare host.
- A child's message to its parent run is capped at 4000 characters and cannot
  close a `<run-result>` fence. The mod's `<run-result>` is capped at 8000, with
  the same escaping.
- The mod's watch tool refuses run ids that are not six lowercase hex digits.
- `run` fails when it cannot save the run.
- `kill` reports when a run does not stop within 10 seconds. The run is still
  marked to stop, and a message sent before it stops resumes it.
- A parent run that is stopping is not woken by its children.
- The hook redirect, the policy and the skill suggest the `CLAUDE_PROXY_TASK`
  heredoc delimiter.
- Run ids outside `[A-Za-z0-9-]{1,64}` are rejected.
- A crashing quota lookup shows as that account's error instead of aborting
  `list`, `auto` or failover.
- `send` help text: a message sent while a run works joins the turn in
  progress.
- README rewritten. Windows is declared unsupported, WSL untested. Examples use
  generic labels.

### Fixed

- `attach` reloads the run under its lock, so a released drainer's state is not
  overwritten.
- The install temp file name is unique per process.

### Removed

- The Windows CI job.
- The shell prototype directory.

### Security

- Run state is owner-only: 0700 directories (existing ones are tightened) and
  0600 files for runs, asks, sessions, MCP configs, profiles, `run.lock`,
  markers, `registry.json` and the quota cache.
- One atomic-write helper with unique temp names.
- Ask keys cannot collide.
- `kill` signals a run only while its drainer holds the run's lock.

## [0.8.1] - 2026-10-08

Plugin only.

### Added

- proxy-runs mod: rows drawn like Claude's transcript, with Markdown replies,
  the engine's own tool rows, folded groups, and asks with Allow / Always allow
  / Deny on the row.
- proxy-runs mod: a watch tool for any run by id, or every run going in the
  repository, and `/runs <id>`.
- proxy-runs mod: a `claude-proxy run` Bash row gets the run's card.

### Changed

- The policy and the skill answer "show me the runs" with the watch tool.

### Fixed

- proxy-runs mod: every run is listed at session start.

## [0.8.0] - 2026-10-06

### Added

- The proxy-runs mod 0.1.0: a tool in place of Agent, live rows, a Runs pane, a
  status line and notifications. It refuses Agent at `tool.call`, marks the
  session so quota-router's hooks stay quiet, and adds a policy section.
- Old runs are pruned (`CLAUDE_PROXY_KEEP_HOURS`, default 24, `0` keeps all),
  and a removed run leaves a note saying how to resume its session.

### Changed

- `runs` shows this repository's runs that are going or finished in the last 2
  hours; `--all` shows every run.
- A run's activity shows the current tool when it is newer than Claude's
  summary.
- The skill has an orchestrator start its own lanes.

## [0.7.0] - 2026-10-06

### Added

- Runs report to the run that started them, which wakes it.
- `runs` shows a tree, and `status` shows a run's parent and children.
- Background tasks keep a run alive.
- `events [<id>…] [--mine]`.
- The plugin's hooks report finished or asking runs at each prompt, and list
  runs still going after a resume or compaction.

### Fixed

- A liveness check that could strand a queued message.

## [0.6.1] - 2026-10-05

Binary only.

### Fixed

- Parallel launches no longer race on one MCP temp file.
- Headless sessions are told to wait in the foreground.

### Security

- `kill` uses killpg(2). On Linux, procps `kill -TERM -<pgid>` could signal
  every process the user owned. Group ids 0 and 1 are refused.

## [0.6.0] - 2026-10-05

### Added

- Warm runs (`CLAUDE_PROXY_IDLE_SECS`, default 300): a message sent mid-turn
  steers the work in progress.
- Lasting grants: `allow --always`, `--rule` and `--accept-edits`.
- Runs inherit the starting session's permission mode, recorded by a
  UserPromptSubmit hook.
- A PreToolUse hook turns Agent and Task calls into `claude-proxy run` unless
  they are marked `[direct]`.

### Fixed

- Approving a plan no longer restarts the run in plan mode.
- `kill` on an idle run frees its Claude process.

## [0.5.1] - 2026-10-05

Plugin only.

### Changed

- The skill writes run ids out instead of using command substitution, and waits
  in the foreground when headless.

### Fixed

- A profile's own `settings.json` gives way to the shared one, moved aside as
  `settings.json.pre-shared`.

## [0.5.0] - 2026-10-05

### Added

- Questions and permission prompts: `allow`, `deny`, `answer`, `wait` exiting
  2, and AskUserQuestion.
- `--fork` / `--fork-from`, `--worktree` and `--agent`.
- `attach`.
- User and local MCP servers are passed to profiles through owner-only
  `--mcp-config` files.
- A SessionStart policy hook (`CLAUDE_PROXY_POLICY=off` turns it off).

### Changed

- A host app's session identity is cleared before Claude starts.

### Fixed

- Transcript appends are single writes.
- No panic on a closed stdout.

## [0.4.1] - 2026-10-05

Plugin only.

### Changed

- The README and the skill say `auto` one-shots do not fail over; managed runs
  do.

## [0.4.0] - 2026-10-05

### Added

- Automatic account failover: `--account auto`, `a,b` in order, or a pinned
  name, with ⇄ in transcripts.

### Changed

- Whether a run starts a new session or resumes one is decided by whether the
  session file exists.

### Fixed

- Two races in the drainer.

## [0.3.0] - 2026-10-05

### Added

- Managed runs: `run`, `status`, `read` / `tail` / `watch`, `result`, `send`,
  `wait`, `kill`, `runs` and `rm`.
- Live rate-limit events update the quota cache.
- The skill teaches delegation.

### Fixed

- `list` hides a logged-out account's stale email and shows `<1m` for imminent
  resets.

## [0.2.0] - 2026-10-02

### Added

- `list` shows quota per account (5-hour, 7-day and model-scoped weekly;
  `--json`, `--refresh`; a 15-minute cache), adapted from ai-usagebar.
- `auto`.
- The repository is also the quota-router plugin, with a skill.

### Changed

- Each account is a full Claude profile with its own `CLAUDE_CONFIG_DIR` and
  Claude's own login; claude-proxy no longer handles tokens.
- A shared `~/.claude` allowlist, linked into each profile.
- Proxy commands are symlinks.
- `claude` and `claude-proxy` are reserved labels.

### Removed

- The 0.1.0 token store and login flow.

## [0.1.0] - 2026-10-02

### Added

- First Rust release, replacing a shell prototype: one binary that is the
  manager or a per-account command depending on `argv[0]`; `add`, `list` and
  `remove`; quota from the OAuth usage endpoint; hermetic tests.

[Unreleased]: https://github.com/ohmaseclaro/claude-proxy/compare/2509d53...main
[0.8.1]: https://github.com/ohmaseclaro/claude-proxy/commit/2509d53
[0.8.0]: https://github.com/ohmaseclaro/claude-proxy/commit/90cc411
[0.7.0]: https://github.com/ohmaseclaro/claude-proxy/commit/c7225d0
[0.6.1]: https://github.com/ohmaseclaro/claude-proxy/commit/b2cec7e
[0.6.0]: https://github.com/ohmaseclaro/claude-proxy/commit/6449839
[0.5.1]: https://github.com/ohmaseclaro/claude-proxy/commit/2fe7560
[0.5.0]: https://github.com/ohmaseclaro/claude-proxy/commit/489c362
[0.4.1]: https://github.com/ohmaseclaro/claude-proxy/commit/8ab59e1
[0.4.0]: https://github.com/ohmaseclaro/claude-proxy/commit/97206eb
[0.3.0]: https://github.com/ohmaseclaro/claude-proxy/commit/352dc73
[0.2.0]: https://github.com/ohmaseclaro/claude-proxy/commit/99b0538
[0.1.0]: https://github.com/ohmaseclaro/claude-proxy/commit/8d497a4
