export type ProxyRunsRun = {
  id: string
  name: string | null
  state: string
  account: string
  turns: number
  activity: string | null
  cost_usd: number
  asks: { tool: string; input: unknown }[]
  last_result: { text: string; ok: boolean } | null
  session: string | null
}

export type RunTool = {
  kind: 'tool'
  id: string
  tool: string
  input: unknown
  /** The structured result Claude stored (`tool_use_result`), else its text. */
  output?: unknown
  /** The text the model read back. */
  text?: string
  isRunning: boolean
  isErrored: boolean
}

export type RunItem =
  | { kind: 'you'; text: string; duringTurn: boolean }
  | { kind: 'text'; text: string }
  | RunTool
  | { kind: 'ask'; text: string }
  | { kind: 'answer'; text: string }
  | { kind: 'note'; text: string; tone: 'info' | 'warn' | 'error' }
  | { kind: 'done'; ok: boolean; text: string }

declare module 'claude-code' {
  interface PluginState {
    'proxy-runs': {
      runs: ProxyRunsRun[]
      calls: Record<string, string>
      told: Record<string, string>
      selected: string
      problem: string
      items: StateFamily<RunItem[]>
      expanded: StateFamily<boolean>
      group: StateFamily<boolean>
      talking: string
    }
  }
}
