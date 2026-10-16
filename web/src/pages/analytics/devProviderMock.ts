import type { Overview } from '../../api'

const MOCK_PROVIDERS = [
  {
    provider_id: -101,
    provider_name: 'Command Code',
    weight: 0.34,
    success_rate: 99.4,
    avg_latency_ms: 2140,
    input_ratio: 0.82,
    cost_per_million_tokens: 4.2,
  },
  {
    provider_id: -102,
    provider_name: 'OpenCode Go',
    weight: 0.27,
    success_rate: 98.7,
    avg_latency_ms: 2860,
    input_ratio: 0.76,
    cost_per_million_tokens: 3.6,
  },
  {
    provider_id: -103,
    provider_name: 'DeepSeek',
    weight: 0.19,
    success_rate: 99.1,
    avg_latency_ms: 1780,
    input_ratio: 0.84,
    cost_per_million_tokens: 2.1,
  },
  {
    provider_id: -104,
    provider_name: 'Anthropic',
    weight: 0.12,
    success_rate: 97.9,
    avg_latency_ms: 3420,
    input_ratio: 0.71,
    cost_per_million_tokens: 7.8,
  },
  {
    provider_id: -105,
    provider_name: 'OpenAI',
    weight: 0.08,
    success_rate: 99.6,
    avg_latency_ms: 1920,
    input_ratio: 0.79,
    cost_per_million_tokens: 6.4,
  },
]

function allocate(total: number, weights: number[]) {
  const values = weights.map((weight) => Math.floor(total * weight))
  let remainder = total - values.reduce((sum, value) => sum + value, 0)
  for (let index = 0; remainder > 0; index = (index + 1) % values.length) {
    values[index] += 1
    remainder -= 1
  }
  return values
}

export function withDevProviderMock(data: Overview) {
  if (!import.meta.env.DEV || data.provider_usage.length > 0) {
    return { data, mocked: false as const }
  }

  const weights = MOCK_PROVIDERS.map((provider) => provider.weight)
  const requestTotal = data.range_requests > 0 ? data.range_requests : 96
  const tokenTotal = data.range_tokens > 0 ? data.range_tokens : requestTotal * 850
  const requests = allocate(requestTotal, weights)
  const tokens = allocate(tokenTotal, weights)

  const provider_usage = MOCK_PROVIDERS.map((provider, index) => {
    const providerTokens = tokens[index]
    const promptTokens = Math.round(providerTokens * provider.input_ratio)
    return {
      provider_id: provider.provider_id,
      provider_name: provider.provider_name,
      requests: requests[index],
      tokens: providerTokens,
      prompt_tokens: promptTokens,
      completion_tokens: providerTokens - promptTokens,
      cost_micros: Math.round(providerTokens * provider.cost_per_million_tokens),
      success_rate: provider.success_rate,
      avg_latency_ms: provider.avg_latency_ms,
    }
  })

  const daily_provider_requests = data.daily_usage.flatMap((day) => {
    const dailyRequests = allocate(day.requests, weights)
    return MOCK_PROVIDERS.map((provider, index) => ({
      day: day.day,
      label: provider.provider_name,
      requests: dailyRequests[index],
    }))
  })

  return {
    data: {
      ...data,
      provider_usage,
      daily_provider_requests,
    },
    mocked: true as const,
  }
}
