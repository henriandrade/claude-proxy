## What and why

## Behaviour changes

Anything a user of the CLI, hooks or mod would notice, or "None".

## Checklist

- [ ] The checks in CONTRIBUTING.md pass:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test --all-targets`
  - `claude plugin validate . --strict`, `claude plugin validate .claude-plugin/plugin.json --strict`, `claude plugin validate skills --strict`, `claude plugin validate mods/proxy-runs --strict`
  - `claude plugin test mods/proxy-runs`
  - `npx -y -p typescript@5 tsc -p mods/proxy-runs --noEmit`
  - `shellcheck hooks/*.sh`
- [ ] Tests added for non-trivial logic, hermetic (no real HOME, keychain or network).
- [ ] README or the mod README updated if behaviour changed.
- [ ] An entry under [Unreleased] in CHANGELOG.md.
- [ ] No secrets, personal emails or real account labels in the diff.
