// A run's events.jsonl: Claude's stream-json plus claude-proxy's markers.
import type { JsonValue } from 'claude-code'

import type { RunItem, RunTool } from '../types'

/** Strings past this are cut: state stays small and Code/Markdown accept it. */
const CAP = 4000
/** Only the newest calls keep their output. */
const OUTPUTS_KEPT = 80
const ITEMS_KEPT = 400

function clip(v: unknown, depth = 0): JsonValue {
  if (typeof v === 'string') return v.length > CAP ? `${v.slice(0, CAP)}…` : v
  if (typeof v === 'number' || typeof v === 'boolean' || v === null) return v
  if (Array.isArray(v)) return v.slice(0, 200).map(x => clip(x, depth + 1))
  if (typeof v === 'object' && depth < 8) {
    return Object.fromEntries(Object.entries(v as object).map(([k, x]) => [k, clip(x, depth + 1)]))
  }
  return null
}

function resultText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content
    .map(c => (c && typeof c === 'object' && typeof (c as { text?: unknown }).text === 'string' ? (c as { text: string }).text : ''))
    .join('\n')
}

const str = (v: unknown) => (typeof v === 'string' ? v : '')

/** The one argument that says what a call does, as Claude's rows show it. */
export function summary(tool: string, input: unknown): string {
  const i = (input ?? {}) as Record<string, unknown>
  const picked =
    tool === 'Bash'
      ? str(i.description) || str(i.command)
      : ['Read', 'Write', 'Edit', 'MultiEdit', 'NotebookEdit'].includes(tool)
        ? str(i.file_path)
        : tool === 'Grep' || tool === 'Glob'
          ? str(i.pattern)
          : tool === 'WebFetch'
            ? str(i.url)
            : tool === 'WebSearch'
              ? str(i.query)
              : tool === 'Agent' || tool === 'Task'
                ? str(i.description)
                : ''
  const text = picked || JSON.stringify(input ?? {})
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > 160 ? `${flat.slice(0, 160)}…` : flat
}

function askText(tool: unknown, input: unknown): string {
  if (tool === 'AskUserQuestion') {
    const qs = ((input as { questions?: { question?: string }[] })?.questions ?? [])
      .map(q => q.question ?? '')
      .filter(Boolean)
    return `asks: ${qs.join(' · ')}`
  }
  const what = tool === 'Bash' ? str((input as { command?: unknown })?.command) : summary(str(tool), input)
  return `asks to use ${str(tool) || 'a tool'}: ${what}`
}

function answerText(v: Record<string, unknown>): string {
  const answers = v.answers as Record<string, string> | null | undefined
  if (answers && typeof answers === 'object' && Object.keys(answers).length > 0) {
    return Object.entries(answers)
      .map(([q, a]) => `${q} → ${a}`)
      .join(' · ')
  }
  return v.behavior === 'allow' ? 'allowed' : `denied${v.message ? `: ${str(v.message)}` : ''}`
}

function doneText(v: Record<string, unknown>): string {
  const parts: string[] = []
  if (typeof v.num_turns === 'number') parts.push(`${v.num_turns} step${v.num_turns === 1 ? '' : 's'}`)
  if (typeof v.duration_ms === 'number') parts.push(duration(v.duration_ms / 1000))
  if (typeof v.total_cost_usd === 'number') parts.push(`$${v.total_cost_usd.toFixed(2)}`)
  return parts.join(' · ')
}

function duration(secs: number): string {
  if (secs < 60) return `${Math.round(secs)}s`
  const m = Math.floor(secs / 60)
  return m < 60 ? `${m}m ${Math.round(secs % 60)}s` : `${Math.floor(m / 60)}h ${m % 60}m`
}

export function parse(lines: readonly string[]): RunItem[] {
  const items: RunItem[] = []
  const toolAt = new Map<string, number>()
  for (const line of lines) {
    let v: Record<string, unknown>
    try {
      v = JSON.parse(line)
    } catch {
      // A line cut short (a truncated read) is dropped; anything else is shown as said.
      if (line.trim() && !line.trimStart().startsWith('{')) items.push({ kind: 'note', text: line.trim(), tone: 'error' })
      continue
    }
    if (v.type === 'claude_proxy') {
      if (v.event === 'message') {
        items.push({ kind: 'you', text: str(v.text), duringTurn: v.during_turn === true })
      } else if (v.event === 'failover') {
        const to = str(v.to)
        items.push({
          kind: 'note',
          tone: 'warn',
          text: `${str(v.from)} ${str(v.reason)} — ${to ? `continuing on ${to}` : 'no other account to continue on'}`,
        })
      } else if (v.event === 'note') {
        items.push({ kind: 'note', tone: 'info', text: str(v.text) })
      } else if (v.event === 'ask') {
        items.push({ kind: 'ask', text: askText(v.tool, v.input) })
      } else if (v.event === 'answer') {
        items.push({ kind: 'answer', text: answerText(v) })
      } else if (v.event === 'turn_end') {
        const text = v.killed
          ? 'Stopped'
          : v.error
            ? str(v.error)
            : typeof v.exit === 'number' && v.exit !== 0
              ? `Claude exited with code ${v.exit}`
              : ''
        if (text) items.push({ kind: 'note', tone: 'error', text })
      }
      continue
    }
    const content = (v.message as { content?: unknown } | undefined)?.content
    if (v.type === 'assistant' && Array.isArray(content)) {
      for (const b of content as Record<string, unknown>[]) {
        if (b.type === 'text' && str(b.text).trim()) items.push({ kind: 'text', text: str(b.text).trim() })
        if (b.type === 'tool_use') {
          toolAt.set(str(b.id), items.length)
          items.push({
            kind: 'tool',
            id: str(b.id),
            tool: str(b.name),
            input: clip(b.input),
            isRunning: true,
            isErrored: false,
          })
        }
      }
      continue
    }
    if (v.type === 'user' && v.isReplay !== true && Array.isArray(content)) {
      let first = true
      for (const b of content as Record<string, unknown>[]) {
        if (b.type !== 'tool_result') continue
        const at = toolAt.get(str(b.tool_use_id))
        if (at === undefined) continue
        const text = resultText(b.content)
        const isErrored = b.is_error === true
        // An error's output is the text the model read, as Claude stores it.
        const structured = first && !isErrored && v.tool_use_result && typeof v.tool_use_result === 'object'
        items[at] = {
          ...(items[at] as RunTool),
          isRunning: false,
          isErrored,
          output: structured ? clip(v.tool_use_result) : clip(text),
          text: clip(text) as string,
        }
        first = false
      }
      continue
    }
    if (v.type === 'result') {
      items.push({ kind: 'done', ok: v.subtype === 'success' && v.is_error !== true, text: doneText(v) })
    }
  }
  const kept = items.slice(-ITEMS_KEPT)
  let outputs = 0
  for (let i = kept.length - 1; i >= 0; i--) {
    const item = kept[i]
    if (item?.kind !== 'tool') continue
    if (++outputs > OUTPUTS_KEPT) kept[i] = { ...item, output: undefined, text: undefined }
  }
  return kept
}

export type Block = { kind: 'group'; id: string; tools: RunTool[] } | Exclude<RunItem, RunTool>

export function blocks(items: readonly RunItem[]): Block[] {
  const out: Block[] = []
  for (const item of items) {
    const last = out.at(-1)
    if (item.kind !== 'tool') out.push(item)
    else if (last?.kind === 'group') last.tools.push(item)
    else out.push({ kind: 'group', id: item.id, tools: [item] })
  }
  return out
}

const KINDS: [RegExp, string, string][] = [
  [/^(Bash|BashOutput|KillShell|KillBash|Monitor)$/, 'ran', 'command'],
  [/^Read$/, 'read', 'file'],
  [/^(Grep|Glob|LS)$/, 'searched for', 'pattern'],
  [/^(Edit|MultiEdit|NotebookEdit)$/, 'edited', 'file'],
  [/^Write$/, 'wrote', 'file'],
  [/^WebFetch$/, 'fetched', 'page'],
  [/^WebSearch$/, 'searched the web for', 'query'],
  [/^(Agent|Task)$/, 'ran', 'agent'],
]

export function groupLabel(tools: readonly RunTool[]): string {
  const counts = new Map<string, { verb: string; noun: string; n: number }>()
  for (const t of tools) {
    const [, verb, noun] = KINDS.find(([re]) => re.test(t.tool)) ?? [/./, 'used', 'other tool']
    const key = `${verb} ${noun}`
    const c = counts.get(key) ?? { verb, noun, n: 0 }
    c.n += 1
    counts.set(key, c)
  }
  const plural = (noun: string, n: number) => (n === 1 ? noun : noun === 'query' ? 'queries' : `${noun}s`)
  const text = [...counts.values()].map(({ verb, noun, n }) => `${verb} ${n} ${plural(noun, n)}`).join(', ')
  const failed = tools.filter(t => t.isErrored).length
  return `${text.charAt(0).toUpperCase()}${text.slice(1)}${failed ? ` · ${failed} failed` : ''}`
}

export function editDiff(input: unknown): string | undefined {
  const i = (input ?? {}) as Record<string, unknown>
  const edits =
    Array.isArray(i.edits) ? (i.edits as Record<string, unknown>[]) : [{ old_string: i.old_string, new_string: i.new_string }]
  const hunks = edits
    .filter(e => typeof e.old_string === 'string' && typeof e.new_string === 'string')
    .map(e => {
      const before = str(e.old_string).split('\n')
      const after = str(e.new_string).split('\n')
      return [
        `@@ -1,${before.length} +1,${after.length} @@`,
        ...before.map(l => `-${l}`),
        ...after.map(l => `+${l}`),
      ].join('\n')
    })
  const diff = hunks.join('\n')
  return diff && diff.length <= 9000 ? diff : undefined
}

export function head(text: string, n: number): [string, number] {
  const lines = text.split('\n')
  return [lines.slice(0, n).join('\n'), Math.max(0, lines.length - n)]
}
