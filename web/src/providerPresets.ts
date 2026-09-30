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
  > &
    Partial<Pick<ProviderInput, 'health_check_model' | 'tool_search_supported'>>
}

export const providerPresets: ProviderPreset[] = [
  {
    key: 'command-code-goat',
    group: 'Coding Plan',
    label: 'Command Code GOAT',
    hint: 'GOAT 套餐，支持 5 小时、周、月额度查询',
    values: {
      name: 'Command Code',
      provider_type: 'openai',
      base_url: 'https://api.commandcode.ai/provider/v1',
      model_prefix: 'commandcode/',
      health_check_model: 'deepseek/deepseek-v4-flash',
    },
  },
  {
    key: 'kimi-code-cn',
    group: 'Coding Plan',
    label: 'Kimi Code 国内',
    hint: 'Kimi For Coding，使用 api.kimi.com/coding/v1',
    values: {
      name: 'Kimi Code',
      provider_type: 'openai',
      base_url: 'https://api.kimi.com/coding/v1',
      model_prefix: 'kimi/',
      health_check_model: 'kimi-for-coding',
    },
  },
  {
    key: 'kimi-code-global',
    group: 'Coding Plan',
    label: 'Kimi Code 国际',
    hint: 'Kimi For Coding，使用 api.kimi.ai/coding/v1',
    values: {
      name: 'Kimi Code Global',
      provider_type: 'openai',
      base_url: 'https://api.kimi.ai/coding/v1',
      model_prefix: 'kimi/',
      health_check_model: 'kimi-for-coding',
    },
  },
  {
    key: 'glm-token-plan',
    group: 'Coding Plan',
    label: 'GLM Token Plan',
    hint: '智谱 Coding Plan，使用专属 coding 端点',
    values: {
      name: 'GLM Token Plan',
      provider_type: 'openai',
      base_url: 'https://open.bigmodel.cn/api/coding/paas/v4',
      model_prefix: 'glm/',
      health_check_model: 'glm-5.3',
    },
  },
  {
    key: 'volcengine-token-plan',
    group: 'Coding Plan',
    label: '火山引擎 Token Plan',
    hint: '火山方舟 Coding Plan 专属端点',
    values: {
      name: '火山引擎 Token Plan',
      provider_type: 'openai',
      base_url: 'https://ark.cn-beijing.volces.com/api/coding/v3',
      model_prefix: 'volcengine/',
      health_check_model: 'doubao-seed-2.0-lite',
    },
  },
  {
    key: 'opencode-zen',
    group: 'Coding Plan',
    label: 'OpenCode Zen',
    hint: 'OpenCode Zen 统一模型入口',
    values: {
      name: 'OpenCode Zen',
      provider_type: 'openai',
      base_url: 'https://opencode.ai/zen/v1',
      model_prefix: 'opencode-zen/',
      health_check_model: 'gpt-5.2-codex',
    },
  },
  {
    key: 'opencode-go',
    group: 'Coding Plan',
    label: 'OpenCode Go',
    hint: 'OpenCode Go，支持 5 小时、周、月额度查询',
    values: {
      name: 'OpenCode Go',
      provider_type: 'openai',
      base_url: 'https://opencode.ai/zen/go/v1',
      model_prefix: 'opencode-go/',
      health_check_model: 'deepseek-v4-flash',
    },
  },
  {
    key: 'custom-openai-responses',
    group: '自定义协议',
    label: 'OpenAI Responses 兼容',
    hint: '接入任意兼容 /v1/responses 的服务',
    values: {
      name: 'OpenAI Responses',
      provider_type: 'custom',
      base_url: '',
      model_prefix: 'responses/',
    },
  },
  {
    key: 'custom-openai-completions',
    group: '自定义协议',
    label: 'OpenAI Completion 兼容',
    hint: '接入兼容 /v1/completions 或 Chat Completions 的服务',
    values: {
      name: 'OpenAI Completion',
      provider_type: 'custom',
      base_url: '',
      model_prefix: 'completion/',
    },
  },
  {
    key: 'custom-anthropic',
    group: '自定义协议',
    label: 'Anthropic 兼容',
    hint: '接入原生 /v1/messages Anthropic 协议',
    values: {
      name: 'Anthropic Compatible',
      provider_type: 'anthropic',
      base_url: '',
      model_prefix: 'anthropic/',
    },
  },
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
      health_check_model: 'deepseek-v4-flash',
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
