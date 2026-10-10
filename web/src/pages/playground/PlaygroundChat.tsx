import { useEffect, useRef } from 'react'
import {
  CopyOutlined,
  RobotOutlined,
  SendOutlined,
  StopOutlined,
  UserOutlined,
} from '@ant-design/icons'
import { App, Button, Card, Empty, Input, Tag, Tooltip, Typography } from 'antd'
import type { ChatMessage } from './types'

type PlaygroundChatProps = {
  messages: ChatMessage[]
  input: string
  model?: string
  running: boolean
  hasApiKeys: boolean
  hasSelectedApiKey: boolean
  onInputChange: (value: string) => void
  onSend: () => void
  onStop: () => void
}

export default function PlaygroundChat({
  messages,
  input,
  model,
  running,
  hasApiKeys,
  hasSelectedApiKey,
  onInputChange,
  onSend,
  onStop,
}: PlaygroundChatProps) {
  const { message } = App.useApp()
  const scrollRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const element = scrollRef.current
    if (element) element.scrollTop = element.scrollHeight
  }, [messages])

  return (
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
                {item.content ||
                  (item.streaming ? <span className="stream-cursor" /> : '')}
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
          onChange={(event) => onInputChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
              event.preventDefault()
              onSend()
            }
          }}
          placeholder="输入消息"
          autoSize={{ minRows: 2, maxRows: 8 }}
          disabled={!model}
        />
        <div className="composer-actions">
          <Typography.Text type="secondary">{model || '尚未选择模型'}</Typography.Text>
          {running ? (
            <Button danger icon={<StopOutlined />} onClick={onStop}>
              停止
            </Button>
          ) : (
            <Button
              type="primary"
              icon={<SendOutlined />}
              disabled={!input.trim() || !model || (hasApiKeys && !hasSelectedApiKey)}
              onClick={onSend}
            >
              发送
            </Button>
          )}
        </div>
      </div>
    </Card>
  )
}
