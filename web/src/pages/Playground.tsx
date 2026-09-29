import { useCallback, useEffect, useRef, useState } from 'react'
import {
  ClearOutlined,
  CopyOutlined,
  HistoryOutlined,
  ReloadOutlined,
  RobotOutlined,
  SendOutlined,
  StopOutlined,
  UserOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Empty,
  Input,
  InputNumber,
  Select,
  Slider,
  Space,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import { api, formatError, type ModelInfo } from '../api'
import PageHeader from '../components/PageHeader'
import { formatCompact } from '../format'

const GATEWAY_KEY = 'openllm-gateway-key'
const SETTINGS_KEY = 'openllm-playground-settings'

type PersistedSettings = {
  model?: string
  temperature: number
  maxTokens: number | null
}

function readSettings(): PersistedSettings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<PersistedSettings>
      return {
        model: parsed.model,
        temperature: typeof parsed.temperature === 'number' ? parsed.temperature : 0.7,
        maxTokens: parsed.maxTokens ?? 4096,
      }
    }
  } catch {
    // Fall through to defaults on malformed storage.
  }
  return { temperature: 0.7, maxTokens: 4096 }
}

type ModelOption = ModelInfo

type ChatMessage = {
  id: string
  role: 'user' | 'assistant'
  content: string
  error?: boolean
  streaming?: boolean
  toolCalls?: ToolCallSummary[]
}

type ToolCallSummary = {
  id: string
  name: string
  arguments: string
}

type Usage = {
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
}

export default function Playground() {
  const { message } = App.useApp()
  const initial = useRef(readSettings())
  const [models, setModels] = useState<ModelOption[]>([])
  const [model, setModel] = useState<string | undefined>(initial.current.model)
  const [gatewayKey, setGatewayKey] = useState(() => localStorage.getItem(GATEWAY_KEY) || '')
  const [input, setInput] = useState('')
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [running, setRunning] = useState(false)
  const [temperature, setTemperature] = useState(initial.current.temperature)
  const [maxTokens, setMaxTokens] = useState<number | null>(initial.current.maxTokens)
  const [usage, setUsage] = useState<Usage>()
  const [latency, setLatency] = useState<number>()
  const [loadError, setLoadError] = useState('')
  const abortRef = useRef<AbortController | undefined>(undefined)
  const scrollRef = useRef<HTMLDivElement>(null)
  const selected = models.find((item) => item.id === model)
  const capabilities = selected?.capabilities
  // Never let the form submit above what the model (or route barrel) accepts;
  // the gateway clamps too, but surfacing the real ceiling avoids a surprise.
  const outputLimit = capabilities?.output_limit

  const loadModels = useCallback(async () => {
    try {
      const result = await api.get<ModelOption[]>('/api/models')
      setModels(result)
      setModel((current) => {
        if (current && result.some((item) => item.id === current)) return current
        return result[0]?.id
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
      JSON.stringify({ model, temperature, maxTokens } satisfies PersistedSettings),
    )
  }, [maxTokens, model, temperature])

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

  useEffect(() => {
    const element = scrollRef.current
    if (element) element.scrollTop = element.scrollHeight
  }, [messages])

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
    if (gatewayKey) localStorage.setItem(GATEWAY_KEY, gatewayKey)
    else localStorage.removeItem(GATEWAY_KEY)

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
      if (gatewayKey) headers.set('Authorization', `Bearer ${gatewayKey}`)
      const response = await fetch('/v1/chat/completions', {
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
                const existing = toolCalls.get(index) ?? { id: '', name: '', arguments: '' }
                toolCalls.set(index, {
                  id: item?.id || existing.id,
                  name: item?.function?.name || existing.name,
                  arguments: existing.arguments + (item?.function?.arguments || ''),
                })
              }
            }
            updateAssistant(answer, { streaming: true, toolCalls: [...toolCalls.values()] })
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
      updateAssistant(
        answer || (finalToolCalls.length ? '' : '（空响应）'),
        { streaming: false, toolCalls: finalToolCalls },
      )
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
      <PageHeader
        title="模型调试"
        description="直接验证路由、鉴权和流式响应"
        extra={
          <>
            {latency !== undefined && <Tag>{latency} ms</Tag>}
            {usage && <Tag color="blue">{formatCompact(usage.total_tokens)} tokens</Tag>}
            <Button
              icon={<HistoryOutlined />}
              onClick={() => void regenerate()}
              disabled={running || !messages.some((item) => item.role === 'user')}
            >
              重发上一条
            </Button>
            <Button icon={<ReloadOutlined />} onClick={() => void loadModels()}>刷新模型</Button>
            <Button icon={<ClearOutlined />} onClick={clear} disabled={!messages.length}>清空</Button>
          </>
        }
      />
      {loadError && <Alert className="page-alert" type="warning" showIcon message="无法读取模型列表" description={loadError} />}
      <div className="playground-grid">
        <Card className="playground-card" bordered={false}>
          <div className="chat-scroll" ref={scrollRef}>
            {messages.length === 0 ? (
              <div className="chat-empty">
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="输入消息开始测试" />
              </div>
            ) : (
              messages.map((item) => (
                <div className={`chat-message chat-message-${item.role}`} key={item.id}>
                  <div className="chat-avatar">
                    {item.role === 'assistant' ? <RobotOutlined /> : <UserOutlined />}
                  </div>
                  <div className={`chat-bubble ${item.error ? 'chat-bubble-error' : ''}`}>
                    {item.content || (item.streaming ? <span className="stream-cursor" /> : '')}
                    {item.streaming && item.content && <span className="stream-cursor" />}
                    {item.toolCalls?.map((call, index) => (
                      <div className="tool-call" key={`${call.id || index}-${index}`}>
                        <div className="tool-call-head">
                          <Tag color="geekblue">工具调用 {index + 1}</Tag>
                          <Typography.Text strong>{call.name || '(未命名)'}</Typography.Text>
                        </div>
                        {call.arguments && (
                          <pre className="tool-call-args">{call.arguments}</pre>
                        )}
                      </div>
                    ))}
                  </div>
                  {item.role === 'assistant' && item.content && !item.streaming && (
                    <Tooltip title="复制">
                      <Button
                        className="chat-copy"
                        type="text"
                        size="small"
                        icon={<CopyOutlined />}
                        onClick={() => {
                          void navigator.clipboard.writeText(item.content)
                          message.success('已复制')
                        }}
                      />
                    </Tooltip>
                  )}
                </div>
              ))
            )}
          </div>
          <div className="chat-composer">
            <Input.TextArea
              value={input}
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
                  event.preventDefault()
                  void send()
                }
              }}
              placeholder="输入消息"
              autoSize={{ minRows: 2, maxRows: 8 }}
              disabled={!model}
            />
            <div className="composer-actions">
              <Typography.Text type="secondary">
                {model || '尚未选择模型'}
              </Typography.Text>
              {running ? (
                <Button danger icon={<StopOutlined />} onClick={stop}>停止</Button>
              ) : (
                <Button type="primary" icon={<SendOutlined />} disabled={!input.trim() || !model} onClick={() => void send()}>
                  发送
                </Button>
              )}
            </div>
          </div>
        </Card>

        <Card title="请求参数" bordered={false} className="playground-settings">
          <Space direction="vertical" size={20} style={{ width: '100%' }}>
            <div>
              <Typography.Text strong>模型</Typography.Text>
              <Select
                className="settings-control"
                showSearch
                value={model}
                onChange={setModel}
                placeholder="选择模型"
                options={models.map((item) => ({ value: item.id, label: item.id }))}
              />
            </div>
            <div>
              <Typography.Text strong>网关 API Key</Typography.Text>
              <Input.Password
                className="settings-control"
                value={gatewayKey}
                onChange={(event) => setGatewayKey(event.target.value)}
                placeholder="未启用鉴权时可留空"
                autoComplete="off"
              />
            </div>
            <div>
              <Typography.Text strong>Temperature</Typography.Text>
              <Slider min={0} max={2} step={0.1} value={temperature} onChange={setTemperature} />
              <Typography.Text type="secondary">{temperature.toFixed(1)}</Typography.Text>
            </div>
            <div>
              <Typography.Text strong>最大输出 tokens</Typography.Text>
              <InputNumber
                className="settings-control"
                min={1}
                max={outputLimit ?? 131072}
                value={maxTokens && outputLimit ? Math.min(maxTokens, outputLimit) : maxTokens}
                onChange={setMaxTokens}
              />
            </div>
            {capabilities && (
              <div>
                <Typography.Text strong>模型能力</Typography.Text>
                <div className="capability-tags">
                  {capabilities.context_limit != null && (
                    <Tag>上下文 {capabilities.context_limit.toLocaleString()}</Tag>
                  )}
                  {capabilities.output_limit != null && (
                    <Tag>输出上限 {capabilities.output_limit.toLocaleString()}</Tag>
                  )}
                  {capabilities.reasoning && <Tag color="geekblue">推理</Tag>}
                  {capabilities.tool_call && <Tag color="green">工具调用</Tag>}
                  {capabilities.attachment && <Tag color="orange">附件</Tag>}
                  {capabilities.structured_output && <Tag>结构化输出</Tag>}
                  {capabilities.input_modalities && (
                    <Tag>输入 {capabilities.input_modalities.join('/')}</Tag>
                  )}
                </div>
                {selected?.target_count != null && (
                  <Typography.Text type="secondary">
                    路由 {selected.target_count} 个目标，能力为共同下限
                    {selected.limits_verified === false ? '（部分目标缺少元数据）' : ''}
                  </Typography.Text>
                )}
              </div>
            )}
          </Space>
        </Card>
      </div>
    </>
  )
}
