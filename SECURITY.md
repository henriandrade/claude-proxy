# Security

## Supported versions

Only the latest `main` is supported. There are no tagged releases yet.

## Report a vulnerability

Report privately through GitHub: the repository's Security tab, "Report a
vulnerability" (https://github.com/ohmaseclaro/claude-proxy/security/advisories/new).
Please do not open a public issue. Include the commit, your OS and the steps to
reproduce. Reports are handled best-effort by the maintainer.

## Scope

In scope: the `claude-proxy` binary, the quota-router plugin (`hooks/`,
`skills/`) and the proxy-runs mod (`mods/proxy-runs/`).

Out of scope: Claude Code itself and Anthropic's services (report those to
Anthropic), attacks that need code already running as your user, and Windows,
which is not supported.

## Threat model

**Shape.** claude-proxy is a local, single-user CLI. It runs no daemon and
listens on no socket. Its only network traffic is HTTPS to Anthropic's OAuth
token and usage endpoints, for quota.

**Trust boundary.** The permissions of `~/.config/claude-proxy` (or
`$XDG_CONFIG_HOME/claude-proxy`). On macOS and Linux its directories are 0700
(existing ones are tightened) and run files 0600. Anyone who can write there,
your own user or root, can read run transcripts and approve a run's tool calls,
because asks and answers are files. claude-proxy does not defend against other
processes running as you.

**Credentials.** Each account is its own Claude profile and Claude does the
login. claude-proxy reads a profile's credential only to refresh its token for
quota lookups, and writes it back to the same store: the macOS login Keychain
through `/usr/bin/security` on stdin, elsewhere `<profile>/.credentials.json`
(0600, atomic write). Tokens never go in argv or logs; errors carry exit or HTTP
codes only. One exception: a credential blob longer than about 4000 bytes is
written with `security add-generic-password -w <blob>` on argv, the same
fallback Claude Code uses, because `security -i` silently truncates longer
lines. While that command runs, other local users can see it in `ps`. Normal
blobs are far smaller.

**Grants.** A run's permission prompts go to `claude-proxy __permit`, which
denies when it cannot record the ask, when the run is stopped, or when Claude
exits. `allow --always` grants the exact Bash command, the WebFetch host (parsed
without userinfo or port), or the tool, and refuses Bash commands containing
`*`, `(` or `)`, which a permission rule reads as syntax; use `--rule` to choose
a pattern deliberately. Grants last for the run, in its `allowed` and `mode`
files, until the run is removed; delete those files to revoke them.

**Prompt injection.** Runs read untrusted content. A child run's output reaches
whoever started it: as a message to a parent run (capped at 4000 characters)
and, through the mod, as `<run-result>` data in the session (capped at 8000).
`<run-result>` tags inside forwarded text are neutralised so they cannot close
the fence, but no fence makes a model immune. Be careful with `--always`,
`acceptEdits` and `--dangerously-skip-permissions` on runs that read untrusted
input. The plugin's SessionStart hook adds a standing delegation policy to every
session; `CLAUDE_PROXY_POLICY=off` turns it off.

**Processes.** `kill` signals a run's process group, never a group id of 1 or
less, and only while the run's drainer holds its lock, so a stale pid is never
trusted. The mod spawns `claude-proxy` with argv arrays (no shell) and only
well-formed run ids. Hook commands quote the plugin path and pass a fixed event
name.
