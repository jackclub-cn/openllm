import { FileTextOutlined, ThunderboltOutlined } from '@ant-design/icons'
import { Alert, Card, Empty, Skeleton, Space, Tag } from 'antd'
import { Link } from 'react-router-dom'
import type { RequestActivity } from '../../api'
import ActivityDetailPanel from './ActivityDetailPanel'
import { selectCurrentActivity } from './requestActivity'

type DashboardRequestActivityProps = {
  items: RequestActivity[]
  connected: boolean
  error?: string
  activeCount: number
  loading: boolean
  enabled: boolean
  paused: boolean
}

export default function DashboardRequestActivity({
  items,
  connected,
  error,
  activeCount,
  loading,
  enabled,
  paused,
}: DashboardRequestActivityProps) {
  const activity = selectCurrentActivity(items)

  return (
    <Card
      className="section-row activity-live-card"
      bordered={false}
      title={
        <Space size={8} wrap>
          <ThunderboltOutlined />
          实时状态
          <Tag color={connected && !paused ? 'success' : 'default'}>
            {paused ? '已暂停' : connected ? '实时' : '轮询'}
          </Tag>
        </Space>
      }
      extra={enabled && (
        <Space size={8} wrap>
          <Tag color={activeCount > 0 ? 'processing' : 'default'}>{activeCount} 进行中</Tag>
          {activity && (
            <>
              <Tag>{activity.outcome === 'running' ? '进行中请求' : '最近一条'}</Tag>
              <Link to={`/usage?request_id=${encodeURIComponent(activity.request_id)}`}>
                <FileTextOutlined /> 请求日志
              </Link>
            </>
          )}
        </Space>
      )}
    >
      {error && (
        <Alert
          type="warning"
          showIcon
          message="无法刷新实时请求"
          description={error}
          style={{ marginBottom: 12 }}
        />
      )}
      {!enabled ? (
        <Empty description="实时请求记录已关闭" image={Empty.PRESENTED_IMAGE_SIMPLE} />
      ) : activity ? (
        <ActivityDetailPanel key={activity.request_id} activity={activity} paused={paused} />
      ) : loading ? (
        <Skeleton active paragraph={{ rows: 4 }} />
      ) : (
        <Empty description="暂无最近请求" image={Empty.PRESENTED_IMAGE_SIMPLE} />
      )}
    </Card>
  )
}
