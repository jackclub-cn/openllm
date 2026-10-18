import {
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Button, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import type { ReactNode } from 'react'
import PageHeader from './PageHeader'

type UsageRangeToolbarProps = {
  title: string
  description?: string
  lastUpdated?: Date
  refreshing: boolean
  paused: boolean
  realtimeConnected: boolean
  onTogglePaused: () => void
  onRefresh: () => void
  extraActions?: ReactNode
}

export default function UsageRangeToolbar({
  title,
  description,
  lastUpdated,
  refreshing,
  paused,
  realtimeConnected,
  onTogglePaused,
  onRefresh,
  extraActions,
}: UsageRangeToolbarProps) {
  return (
    <PageHeader
      title={title}
      description={description ?? ''}
      extra={
        <>
          {extraActions}
          {lastUpdated && (
            <Typography.Text type="secondary">
              更新于 {dayjs(lastUpdated).format('HH:mm:ss')}
            </Typography.Text>
          )}
          <Tag color={realtimeConnected ? 'success' : 'default'}>
            {realtimeConnected ? '实时' : '轮询'}
          </Tag>
          <Tooltip title={paused ? '恢复自动刷新' : '暂停自动刷新'}>
            <Button
              icon={paused ? <PlayCircleOutlined /> : <PauseCircleOutlined />}
              onClick={onTogglePaused}
            >
              {paused ? '已暂停' : '自动刷新'}
            </Button>
          </Tooltip>
          <Button
            icon={<ReloadOutlined spin={refreshing} />}
            loading={refreshing}
            onClick={onRefresh}
          >
            刷新
          </Button>
        </>
      }
    />
  )
}
