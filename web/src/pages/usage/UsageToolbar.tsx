import {
  DownloadOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Alert, Button, Space, Switch, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import PageHeader from '../../components/PageHeader'

type UsageToolbarProps = {
  lastUpdated?: Date
  refreshing: boolean
  paused: boolean
  realtimeConnected: boolean
  autoScroll: boolean
  newRequests: number
  exporting: boolean
  onAutoScrollChange: (value: boolean) => void
  onTogglePaused: () => void
  onRefresh: () => void
  onExport: () => void
  onCleanup: () => void
  onShowLatest: () => void
  onDismissNew: () => void
}

export default function UsageToolbar({
  lastUpdated,
  refreshing,
  paused,
  realtimeConnected,
  autoScroll,
  newRequests,
  exporting,
  onAutoScrollChange,
  onTogglePaused,
  onRefresh,
  onExport,
  onCleanup,
  onShowLatest,
  onDismissNew,
}: UsageToolbarProps) {
  return (
    <>
      <PageHeader
        title="请求日志"
        description="查询每一次模型调用、令牌用量、总用时与错误"
        extra={
          <>
            {lastUpdated && (
              <Typography.Text type="secondary">
                更新于 {dayjs(lastUpdated).format('HH:mm:ss')}
              </Typography.Text>
            )}
            <Tag color={realtimeConnected ? 'success' : 'default'}>
              {realtimeConnected ? '实时' : '轮询'}
            </Tag>
            <Tooltip title="第一页时自动切换为最新记录">
              <Space size={6}>
                <Switch
                  size="small"
                  checked={autoScroll}
                  onChange={onAutoScrollChange}
                />
                <Typography.Text type="secondary">自动置顶</Typography.Text>
              </Space>
            </Tooltip>
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
            <Button icon={<DownloadOutlined />} loading={exporting} onClick={onExport}>
              导出 CSV
            </Button>
            <Button onClick={onCleanup}>清理历史</Button>
          </>
        }
      />
      {newRequests > 0 && (
        <Alert
          className="new-records-alert"
          type="info"
          showIcon
          message={`有 ${newRequests} 条新请求`}
          action={
            <Button size="small" type="primary" onClick={onShowLatest}>
              查看最新
            </Button>
          }
          closable
          onClose={onDismissNew}
        />
      )}
    </>
  )
}
