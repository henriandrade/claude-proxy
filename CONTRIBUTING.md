# Contributing

Contributions are welcome. For anything larger than a fix, open an issue first
so we can agree on the shape. Security problems go to [SECURITY.md](SECURITY.md),
not to issues.

## Setup

- Rust 1.89+ (via [rustup](https://rustup.rs)).
- Claude Code 2.1.287+ on `PATH` for the plugin and mod checks
  (`npm i -g @anthropic-ai/claude-code`).
- Node with `npx`, for tsc.
- shellcheck.

```console
$ git clone https://github.com/ohmaseclaro/claude-proxy
$ cd claude-proxy
$ cargo build
```

Try changes with `cargo run -- <args>` or `target/debug/claude-proxy`. Do not
install the dev build over your working claude-proxy unless you mean to.

## Layout

- `src/` is the binary: `main.rs` dispatches on `argv[0]`, `cli.rs` holds the
  commands, the run engine is `runs.rs`, `drainer.rs`, `asks.rs` and
  `agents.rs`, quota is `quota.rs` and `creds.rs`, and `shared.rs`,
  `proxy.rs` and `hook.rs` cover the shared config, the proxy and the plugin's
  hooks.
- `hooks/`, `skills/` and `.claude-plugin/` are the quota-router plugin.
- `mods/proxy-runs/` is the mod.
- `tests/` holds the end-to-end tests.

Read the invariants commented in `runs.rs` and `drainer.rs` before changing
them (for example, `meta.json` is written only under `run.lock`).

## Checks

What CI runs (shellcheck only on Linux), as you would run it locally:

```console
$ cargo fmt --all -- --check
$ cargo clippy --all-targets -- -D warnings
$ cargo test --all-targets
$ cargo build --release
$ claude plugin validate . --strict
$ claude plugin validate .claude-plugin/plugin.json --strict
$ claude plugin validate skills --strict
$ claude plugin validate mods/proxy-runs --strict
$ claude plugin test mods/proxy-runs
$ rm -rf mods/proxy-runs/.claude-plugin/types && env -i PATH="$PATH" HOME=$(mktemp -d) TERM=dumb claude --plugin-dir "$PWD/mods/proxy-runs" -p hi >/dev/null 2>&1; npx -y -p typescript@5 tsc -p mods/proxy-runs --noEmit
$ shellcheck hooks/*.sh
```

tsc needs `mods/proxy-runs/tsconfig.json` and `mods/proxy-runs/.claude-plugin/types/`.
Both are git-ignored and written by a Claude session that loads the mod, which
is what the first half of the tsc line does in a throwaway HOME ("Not logged
in" is expected). If tsc reports `mcp__proxy-runs__*` names, the types came from
a live session; rerun the line. CI runs all of this on ubuntu-latest and
macos-latest.

## Tests

Tests are hermetic: no real HOME, keychain, credential store, network or
`~/.config/claude-proxy`. Use tempfile directories and injected paths.
End-to-end run tests (`tests/runs.rs`) put a fake `claude` on `PATH` in a
throwaway HOME, and `tests/hooks.rs` runs the hook scripts against a stub and
against the built binary. Add a test for any non-trivial logic. Never print
secrets or cat credential files while debugging.

## Code style

cargo fmt and clippy clean. Comments only where the code cannot speak for
itself: a non-obvious constraint, not narration, history or alternatives
considered. Keep changes small and focused, and prefer the standard library and
existing helpers over new dependencies.

## Commits and pull requests

Commit subjects start with a lowercase type and an optional scope (`feat:`,
`fix(runs):`, `docs:`, `chore:`, `test:`, `security:`, `refactor:`,
`mod(proxy-runs):`), then say plainly what changed. The body explains why and
how it was verified. For example:

```text
fix(runs): signal the run's process group with killpg, not `kill`
mod(proxy-runs): talk to a run for one message; space the run's blocks
```

One topic per pull request. Fill in the template, note behaviour changes, add
an entry under [Unreleased] in [CHANGELOG.md](CHANGELOG.md), and keep CI green.

Versions and releases are the maintainer's. Versions bump in `Cargo.toml` and
`.claude-plugin/plugin.json`; a plugin-only change bumps the plugin's patch
version so `claude plugin update` picks it up. Nothing is tagged or published
to crates.io (the name there belongs to another crate).

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
