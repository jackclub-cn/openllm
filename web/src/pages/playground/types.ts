import type { ModelInfo } from '../../api'

export const GATEWAY_KEY_ID = 'openllm-gateway-key-id'
export const SETTINGS_KEY = 'openllm-playground-settings'

export type PersistedSettings = {
  model?: string
  temperature: number
  maxTokens: number | null
  /** Optional per-request USD ceiling forwarded as `x-openllm-budget-usd`. */
  budgetUsd: number | null
}

export function readSettings(): PersistedSettings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<PersistedSettings>
      return {
        model: parsed.model,
        temperature: typeof parsed.temperature === 'number' ? parsed.temperature : 0.7,
        maxTokens: parsed.maxTokens ?? 4096,
        budgetUsd:
          typeof parsed.budgetUsd === 'number' && parsed.budgetUsd > 0
            ? parsed.budgetUsd
            : null,
      }
    }
  } catch {
    // Fall through to defaults on malformed storage.
  }
  return { temperature: 0.7, maxTokens: 4096, budgetUsd: null }
}

export type ModelOption = ModelInfo

export type ToolCallSummary = {
  id: string
  name: string
  arguments: string
}

export type ChatMessage = {
  id: string
  role: 'user' | 'assistant'
  content: string
  error?: boolean
  streaming?: boolean
  toolCalls?: ToolCallSummary[]
}

export type PlaygroundUsage = {
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
}
