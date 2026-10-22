export type ModelCapabilities = {
  context_limit?: number
  output_limit?: number
  input_limit?: number
  attachment?: boolean
  reasoning?: boolean
  reasoning_efforts?: string[]
  tool_call?: boolean
  structured_output?: boolean
  temperature?: boolean
  open_weights?: boolean
  input_modalities?: string[]
  output_modalities?: string[]
  cost?: Record<string, unknown>
  family?: string
  knowledge?: string
  release_date?: string
  last_updated?: string
  canonical_model_id?: string
}

export type ModelInfo = {
  id: string
  object: string
  created: number
  owned_by: string
  provider?: string
  upstream_model?: string
  capabilities?: ModelCapabilities
  target_count?: number
  limits_verified?: boolean
  context_length?: number
  max_input_tokens?: number
  max_output_tokens?: number
  max_completion_tokens?: number
  supported_endpoints?: string[]
}
