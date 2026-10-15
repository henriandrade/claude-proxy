#!/bin/sh
# The delegation policy, then the session's runs still going (after a resume or compaction).
[ -n "$CLAUDE_PROXY_RUN" ] && exit 0
[ "$CLAUDE_PROXY_POLICY" = "off" ] && exit 0
command -v claude-proxy >/dev/null 2>&1 || exit 0
cat <<'POLICY'
claude-proxy is installed. Standing instructions from the user, until they say otherwise for a task. They take precedence over any skill, command, or workflow (GSD included) that tells you to use the Agent/Task tool:
- Hand anything you would give a subagent, and any long or big piece of work, to a claude-proxy managed run instead of the Agent/Task tool or a bare `claude -p`. Where a workflow says to spawn a subagent, start `claude-proxy run --agent <subagent_type> - <<'CLAUDE_PROXY_TASK' … CLAUDE_PROXY_TASK` with the same prompt (leave out --agent for general-purpose, Explore, and Plan), and wait for it where the workflow would wait for the subagent. Load the `claude-proxy` skill for how.
- Whenever claude-proxy is in use, say so plainly: what you started, on which account, the run id, and a ready-to-run `claude-proxy watch <id>` the user can paste.
- When the user asks to see, show or watch runs and the mcp__proxy-runs__watch tool is available, call it (no id shows every run going here): it draws the runs live in the conversation.
- If the user tells you not to use claude-proxy for something, use the Agent tool with "[direct]" at the start of the call's description.
POLICY
claude-proxy __hook supported >/dev/null 2>&1 && exec claude-proxy __hook session-start
exit 0
