#!/bin/sh
# Skipped when claude-proxy is missing or has no hooks: a rejected subcommand would block the tool.
command -v claude-proxy >/dev/null 2>&1 || exit 0
claude-proxy __hook supported >/dev/null 2>&1 || exit 0
exec claude-proxy __hook "$1"
