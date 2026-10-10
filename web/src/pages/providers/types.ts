import type { ProviderApiKeyInput, ProviderInput } from '../../api'

export type ProviderStatus = 'all' | 'enabled' | 'disabled'
export type ProviderHealth =
  | 'all'
  | 'healthy'
  | 'failed'
  | 'untested'
  | 'key_error'
  | 'key_untested'

export const providerLabels = {
  openai: 'OpenAI 兼容',
  anthropic: 'Anthropic',
  ollama: 'Ollama',
  custom: '自定义',
}

export const endpointOptions = [
  '/v1/chat/completions',
  '/v1/responses',
  '/v1/completions',
  '/v1/embeddings',
  '/v1/messages',
].map((value) => ({ value, label: value }))

export const syncFieldLabels: Record<string, string> = {
  context_limit: '上下文上限',
  input_limit: '输入上限',
  output_limit: '输出上限',
  supported_endpoints: '支持接口',
  cost: '价格',
  display_name: '显示名',
}

export function formatUnitPrice(value?: number | null) {
  if (value == null) return '-'
  return `$${value.toLocaleString('en-US', { maximumFractionDigits: 6 })}`
}

export type ProviderForm = Omit<ProviderInput, 'api_keys'> & {
  headersText: string
  modelsText: string
  api_keys?: Array<ProviderApiKeyInput & { api_key_suffix?: string }>
}

export type ModelSyncPreview = {
  provider_id: number
  added: string[]
  removed: string[]
  changed: Array<{ model_name: string; fields: string[] }>
  retained: number
  disabled_retained: number
}
