import { useCallback, useEffect, useRef, useState } from 'react'
import { Alert, App } from 'antd'
import { api, formatError, getAdminToken, type ApiKey } from '../api'
import PlaygroundChat from './playground/PlaygroundChat'
import PlaygroundSettings from './playground/PlaygroundSettings'
import PlaygroundToolbar from './playground/PlaygroundToolbar'
import {
  GATEWAY_KEY_ID,
  SETTINGS_KEY,
  readSettings,
  type ChatMessage,
  type ModelOption,
  type PlaygroundUsage,
  type ToolCallSummary,
} from './playground/types'

export default function Playground() {
  const { message } = App.useApp()
  const initial = useRef(readSettings())
  const [models, setModels] = useState<ModelOption[]>([])
  const [model, setModel] = useState<string | undefined>(initial.current.model)
  const [apiKeys, setApiKeys] = useState<ApiKey[]>([])
  const [gatewayKeyId, setGatewayKeyId] = useState<number | undefined>(() => {
    const value = Number(localStorage.getItem(GATEWAY_KEY_ID))
    return Number.isInteger(value) && value > 0 ? value : undefined
  })
  const [input, setInput] = useState('')
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [running, setRunning] = useState(false)
  const [temperature, setTemperature] = useState(initial.current.temperature)
  const [maxTokens, setMaxTokens] = useState<number | null>(initial.current.maxTokens)
  const [budgetUsd, setBudgetUsd] = useState<number | null>(initial.current.budgetUsd)
  const [usage, setUsage] = useState<PlaygroundUsage>()
  const [latency, setLatency] = useState<number>()
  const [loadError, setLoadError] = useState('')
  const abortRef = useRef<AbortController | undefined>(undefined)
  const selected = models.find((item) => item.id === model)
  const capabilities = selected?.capabilities
  const selectableApiKeys = apiKeys.filter(
    (key) =>
      key.enabled && (!key.expires_at || new Date(key.expires_at).getTime() > Date.now()),
  )
  const selectedApiKey = selectableApiKeys.find((key) => key.id === gatewayKeyId)
  // Never let the form submit above what the model (or route barrel) accepts;
  // the gateway clamps too, but surfacing the real ceiling avoids a surprise.
  const outputLimit = capabilities?.output_limit

  const loadModels = useCallback(async () => {
    try {
      const [modelResult, keyResult] = await Promise.all([
        api.get<ModelOption[]>('/api/models'),
        api.get<ApiKey[]>('/api/api-keys'),
      ])
      setModels(modelResult)
      setApiKeys(keyResult)
      setModel((current) => {
        if (current && modelResult.some((item) => item.id === current)) return current
        return modelResult[0]?.id
      })
      setGatewayKeyId((current) => {
        const selectable = keyResult.filter(
          (key) =>
            key.enabled &&
            (!key.expires_at || new Date(key.expires_at).getTime() > Date.now()),
        )
        if (current && selectable.some((key) => key.id === current)) return current
        return selectable[0]?.id
      })
      setLoadError('')
    } catch (error) {
      setLoadError(formatError(error))
    }
  }, [])

  useEffect(() => {
    void loadModels()
    return () => abortRef.current?.abort()
  }, [loadModels])

  useEffect(() => {
    localStorage.setItem(
      SETTINGS_KEY,
      JSON.stringify({ model, temperature, maxTokens, budgetUsd }),
    )
  }, [budgetUsd, maxTokens, model, temperature])

  useEffect(() => {
    if (gatewayKeyId) localStorage.setItem(GATEWAY_KEY_ID, String(gatewayKeyId))
    else localStorage.removeItem(GATEWAY_KEY_ID)
  }, [gatewayKeyId])

  const regenerate = async () => {
    if (running) return
    const lastUserIndex = messages.map((item) => item.role).lastIndexOf('user')
    if (lastUserIndex < 0) {
      message.info('没有可重新发送的消息')
      return
    }
    const lastUser = messages[lastUserIndex]
    const trimmed = messages.slice(0, lastUserIndex)
    setMessages(trimmed)
    setInput(lastUser.content)
    message.info('已载入上一条问题，确认后可重新发送')
  }

  const updateAssistant = (
    content: string,
    options?: { error?: boolean; streaming?: boolean; toolCalls?: ToolCallSummary[] },
  ) => {
    setMessages((current) => {
      const next = [...current]
      const last = next[next.length - 1]
      if (!last || last.role !== 'assistant') return current
      next[next.length - 1] = {
        ...last,
        content,
        error: options?.error,
        streaming: options?.streaming,
        toolCalls: options?.toolCalls,
      }
      return next
    })
  }

  const send = async () => {
    const content = input.trim()
    if (!content || running) return
    if (!model) {
      message.warning('请先选择模型')
      return
    }
    if (apiKeys.length > 0 && !selectedApiKey) {
      message.warning('请选择网关 API Key')
      return
    }

    const userMessage: ChatMessage = { id: crypto.randomUUID(), role: 'user', content }
    const assistantMessage: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'assistant',
      content: '',
      streaming: true,
    }
    const requestMessages = [
      ...messages.filter((item) => !item.error && item.content.trim()),
      userMessage,
    ]
    setMessages([...requestMessages, assistantMessage])
    setInput('')
    setUsage(undefined)
    setLatency(undefined)
    setRunning(true)

    const controller = new AbortController()
    abortRef.current = controller
    const started = performance.now()
    let answer = ''
    // Tool call deltas arrive fragmented and keyed by index; accumulate them
    // so the UI can show what the model asked to invoke.
    const toolCalls = new Map<number, ToolCallSummary>()

    try {
      const headers = new Headers({ 'Content-Type': 'application/json' })
      const adminToken = getAdminToken()
      if (adminToken) headers.set('x-admin-token', adminToken)
      if (selectedApiKey) headers.set('x-openllm-api-key-id', String(selectedApiKey.id))
      if (budgetUsd && budgetUsd > 0) {
        headers.set('x-openllm-budget-usd', String(budgetUsd))
      }
      const response = await fetch('/api/playground/chat/completions', {
        method: 'POST',
        headers,
        signal: controller.signal,
        body: JSON.stringify({
          model,
          messages: requestMessages.map(({ role, content: messageContent }) => ({
            role,
            content: messageContent,
          })),
          temperature,
          max_tokens: maxTokens || undefined,
          stream: true,
        }),
      })

      if (!response.ok) {
        const body = await response.json().catch(() => undefined)
        throw new Error(body?.error?.message || `${response.status} ${response.statusText}`)
      }
      if (!response.body) throw new Error('响应没有可读取的内容')

      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { value, done } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })
        let boundary = buffer.indexOf('\n\n')
        while (boundary >= 0) {
          const event = buffer.slice(0, boundary)
          buffer = buffer.slice(boundary + 2)
          boundary = buffer.indexOf('\n\n')
          const data = event
            .split('\n')
            .filter((line) => line.startsWith('data:'))
            .map((line) => line.slice(5).trimStart())
            .join('\n')
          if (!data || data === '[DONE]') continue
          try {
            const payload = JSON.parse(data)
            const delta = payload?.choices?.[0]?.delta?.content
            if (typeof delta === 'string') {
              answer += delta
            }
            const deltas = payload?.choices?.[0]?.delta?.tool_calls
            if (Array.isArray(deltas)) {
              for (const item of deltas) {
                const index = typeof item?.index === 'number' ? item.index : 0
                const existing = toolCalls.get(index) ?? {
                  id: '',
                  name: '',
                  arguments: '',
                }
                toolCalls.set(index, {
                  id: item?.id || existing.id,
                  name: item?.function?.name || existing.name,
                  arguments: existing.arguments + (item?.function?.arguments || ''),
                })
              }
            }
            updateAssistant(answer, {
              streaming: true,
              toolCalls: [...toolCalls.values()],
            })
            if (payload?.usage) setUsage(payload.usage)
          } catch {
            // Ignore keep-alive or non-JSON SSE frames.
          }
        }
      }

      const remaining = `${buffer}${decoder.decode()}`.trim()
      if (remaining) {
        const data = remaining
          .split('\n')
          .filter((line) => line.startsWith('data:'))
          .map((line) => line.slice(5).trimStart())
          .join('\n')
        if (data && data !== '[DONE]') {
          try {
            const delta = JSON.parse(data)?.choices?.[0]?.delta?.content
            if (typeof delta === 'string') answer += delta
          } catch {
            // Ignore malformed trailing data.
          }
        }
      }

      const finalToolCalls = [...toolCalls.values()]
      updateAssistant(answer || (finalToolCalls.length ? '' : '（空响应）'), {
        streaming: false,
        toolCalls: finalToolCalls,
      })
      setLatency(Math.round(performance.now() - started))
    } catch (error) {
      if (controller.signal.aborted) {
        // Keep whatever was already streamed instead of replacing it, so the
        // user does not lose partial output when they hit stop.
        updateAssistant(answer ? `${answer}\n\n（已停止生成）` : '已停止生成', {
          error: true,
          streaming: false,
        })
      } else {
        updateAssistant(formatError(error), { error: true, streaming: false })
      }
      setLatency(Math.round(performance.now() - started))
    } finally {
      abortRef.current = undefined
      setRunning(false)
    }
  }

  const stop = () => abortRef.current?.abort()

  const clear = () => {
    abortRef.current?.abort()
    setMessages([])
    setUsage(undefined)
    setLatency(undefined)
  }

  return (
    <>
      <PlaygroundToolbar
        latency={latency}
        usage={usage}
        running={running}
        hasUserMessage={messages.some((item) => item.role === 'user')}
        hasMessages={messages.length > 0}
        onRegenerate={() => void regenerate()}
        onReloadModels={() => void loadModels()}
        onClear={clear}
      />
      {loadError && (
        <Alert
          className="page-alert"
          type="warning"
          showIcon
          message="无法读取模型列表"
          description={loadError}
        />
      )}
      <div className="playground-grid">
        <PlaygroundChat
          messages={messages}
          input={input}
          model={model}
          running={running}
          hasApiKeys={apiKeys.length > 0}
          hasSelectedApiKey={Boolean(selectedApiKey)}
          onInputChange={setInput}
          onSend={() => void send()}
          onStop={stop}
        />
        <PlaygroundSettings
          models={models}
          apiKeys={apiKeys}
          model={model}
          gatewayKeyId={gatewayKeyId}
          temperature={temperature}
          maxTokens={maxTokens}
          budgetUsd={budgetUsd}
          outputLimit={outputLimit}
          capabilities={capabilities}
          selected={selected}
          onModelChange={setModel}
          onGatewayKeyChange={setGatewayKeyId}
          onTemperatureChange={setTemperature}
          onMaxTokensChange={setMaxTokens}
          onBudgetUsdChange={setBudgetUsd}
        />
      </div>
    </>
  )
}
