import {
  ClearOutlined,
  HistoryOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Button, Tag } from 'antd'
import PageHeader from '../../components/PageHeader'
import { formatCompact } from '../../format'
import type { PlaygroundUsage } from './types'

type PlaygroundToolbarProps = {
  latency?: number
  usage?: PlaygroundUsage
  running: boolean
  hasUserMessage: boolean
  hasMessages: boolean
  onRegenerate: () => void
  onReloadModels: () => void
  onClear: () => void
}

export default function PlaygroundToolbar({
  latency,
  usage,
  running,
  hasUserMessage,
  hasMessages,
  onRegenerate,
  onReloadModels,
  onClear,
}: PlaygroundToolbarProps) {
  return (
    <PageHeader
      title="模型调试"
      description="直接验证路由、鉴权和流式响应"
      extra={
        <>
          {latency !== undefined && <Tag>{latency} ms</Tag>}
          {usage && <Tag color="blue">{formatCompact(usage.total_tokens)} tokens</Tag>}
          <Button
            icon={<HistoryOutlined />}
            onClick={onRegenerate}
            disabled={running || !hasUserMessage}
          >
            重发上一条
          </Button>
          <Button icon={<ReloadOutlined />} onClick={onReloadModels}>
            刷新模型
          </Button>
          <Button icon={<ClearOutlined />} onClick={onClear} disabled={!hasMessages}>
            清空
          </Button>
        </>
      }
    />
  )
}
