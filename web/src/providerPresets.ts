import type { ProviderInput } from './api'

/**
 * Common providers, pre-filled so adding one is a single click.
 *
 * `base_url` is the value the gateway joins paths onto: OpenAI-compatible
 * vendors expose `/<...>/v1`, while Anthropic's native API is rooted at the
 * host and served at `/v1/messages`.
 */
export type ProviderPreset = {
  /** Stable key, also shown in the picker. */
  key: string
  /** Grouping label in the dropdown. */
  group: string
  label: string
  /** One-line hint about what makes this provider notable. */
  hint: string
  values: Pick<
    ProviderInput,
    'name' | 'provider_type' | 'base_url' | 'model_prefix'
  >
}

export const providerPresets: ProviderPreset[] = [
  {
    key: 'openai',
    group: '国际',
    label: 'OpenAI',
    hint: 'GPT 系列官方接口',
    values: {
      name: 'OpenAI',
      provider_type: 'openai',
      base_url: 'https://api.openai.com/v1',
      model_prefix: 'openai/',
    },
  },
  {
    key: 'anthropic',
    group: '国际',
    label: 'Anthropic',
    hint: 'Claude 官方接口，使用 /v1/messages 原生协议',
    values: {
      name: 'Anthropic',
      provider_type: 'anthropic',
      base_url: 'https://api.anthropic.com',
      model_prefix: 'anthropic/',
    },
  },
  {
    key: 'google',
    group: '国际',
    label: 'Google Gemini',
    hint: 'Gemini 的 OpenAI 兼容端点',
    values: {
      name: 'Google',
      provider_type: 'openai',
      base_url: 'https://generativelanguage.googleapis.com/v1beta/openai',
      model_prefix: 'gemini/',
    },
  },
  {
    key: 'xai',
    group: '国际',
    label: 'xAI Grok',
    hint: 'Grok 系列',
    values: {
      name: 'xAI',
      provider_type: 'openai',
      base_url: 'https://api.x.ai/v1',
      model_prefix: 'xai/',
    },
  },
  {
    key: 'mistral',
    group: '国际',
    label: 'Mistral',
    hint: 'Mistral 官方接口',
    values: {
      name: 'Mistral',
      provider_type: 'openai',
      base_url: 'https://api.mistral.ai/v1',
      model_prefix: 'mistral/',
    },
  },
  {
    key: 'groq',
    group: '国际',
    label: 'Groq',
    hint: '低延迟推理',
    values: {
      name: 'Groq',
      provider_type: 'openai',
      base_url: 'https://api.groq.com/openai/v1',
      model_prefix: 'groq/',
    },
  },
  {
    key: 'openrouter',
    group: '聚合',
    label: 'OpenRouter',
    hint: '聚合多家模型的统一入口',
    values: {
      name: 'OpenRouter',
      provider_type: 'openai',
      base_url: 'https://openrouter.ai/api/v1',
      model_prefix: 'openrouter/',
    },
  },
  {
    key: 'siliconflow',
    group: '聚合',
    label: 'SiliconFlow 硅基流动',
    hint: '国内模型聚合平台',
    values: {
      name: 'SiliconFlow',
      provider_type: 'openai',
      base_url: 'https://api.siliconflow.cn/v1',
      model_prefix: 'siliconflow/',
    },
  },
  {
    key: 'deepseek',
    group: '国内',
    label: 'DeepSeek',
    hint: 'DeepSeek 官方接口',
    values: {
      name: 'DeepSeek',
      provider_type: 'openai',
      base_url: 'https://api.deepseek.com/v1',
      model_prefix: 'deepseek/',
    },
  },
  {
    key: 'moonshot',
    group: '国内',
    label: 'Moonshot Kimi',
    hint: '月之暗面 Kimi 系列',
    values: {
      name: 'Moonshot AI',
      provider_type: 'openai',
      base_url: 'https://api.moonshot.cn/v1',
      model_prefix: 'kimi/',
    },
  },
  {
    key: 'zhipu',
    group: '国内',
    label: '智谱 GLM',
    hint: '智谱开放平台',
    values: {
      name: 'Zhipu AI',
      provider_type: 'openai',
      base_url: 'https://open.bigmodel.cn/api/paas/v4',
      model_prefix: 'glm/',
    },
  },
  {
    key: 'dashscope',
    group: '国内',
    label: '阿里云百炼 Qwen',
    hint: '通义千问 OpenAI 兼容模式',
    values: {
      name: 'Alibaba',
      provider_type: 'openai',
      base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
      model_prefix: 'qwen/',
    },
  },
  {
    key: 'minimax',
    group: '国内',
    label: 'MiniMax',
    hint: 'MiniMax 开放平台',
    values: {
      name: 'MiniMax',
      provider_type: 'openai',
      base_url: 'https://api.minimax.chat/v1',
      model_prefix: 'minimax/',
    },
  },
  {
    key: 'ollama',
    group: '本地',
    label: 'Ollama',
    hint: '本机 Ollama，模型列表读取 /api/tags',
    values: {
      name: 'Ollama',
      provider_type: 'ollama',
      base_url: 'http://localhost:11434/v1',
      model_prefix: 'ollama/',
    },
  },
  {
    key: 'vllm',
    group: '本地',
    label: 'vLLM / LM Studio',
    hint: '本地 OpenAI 兼容服务',
    values: {
      name: 'Local',
      provider_type: 'custom',
      base_url: 'http://localhost:8000/v1',
      model_prefix: 'local/',
    },
  },
]
