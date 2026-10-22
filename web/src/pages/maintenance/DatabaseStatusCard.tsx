import {
  CompressOutlined,
  DatabaseOutlined,
  DownloadOutlined,
} from '@ant-design/icons'
import {
  Button,
  Card,
  Descriptions,
  Popconfirm,
  Space,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import type { DatabaseStats } from '../../api'
import { formatBytes, formatCompact, formatExact } from '../../format'

type Props = {
  stats?: DatabaseStats
  vacuuming: boolean
  downloadLoading: boolean
  onVacuum: () => void
  onDownload: () => void
}

export default function DatabaseStatusCard({
  stats,
  vacuuming,
  downloadLoading,
  onVacuum,
  onDownload,
}: Props) {
  return (
    <Card
      title={
        <Space>
          <DatabaseOutlined />
          数据库状态与维护
        </Space>
      }
      extra={
        <Space>
          <Button
            icon={<DownloadOutlined />}
            loading={downloadLoading}
            onClick={onDownload}
          >
            下载数据库
          </Button>
          <Popconfirm
            title="整理数据库并回收空间？"
            description="操作期间会短暂锁定数据库写入；数据量较大时可能耗时，建议先下载备份。"
            okText="开始整理"
            cancelText="取消"
            onConfirm={onVacuum}
          >
            <Button
              icon={<CompressOutlined />}
              loading={vacuuming}
              disabled={!stats?.free_bytes}
            >
              整理数据库
            </Button>
          </Popconfirm>
        </Space>
      }
    >
      {stats ? (
        <>
          <Descriptions column={2} size="small">
            <Descriptions.Item label="数据库文件" span={2}>
              {stats.path ? (
                <Typography.Text copyable={{ text: stats.path }}>
                  {stats.path}
                </Typography.Text>
              ) : (
                '内存数据库'
              )}
            </Descriptions.Item>
            <Descriptions.Item label="占用空间">
              <Tooltip title={`${formatExact(stats.size_bytes)} bytes`}>
                {formatBytes(stats.size_bytes)}
              </Tooltip>
            </Descriptions.Item>
            <Descriptions.Item label="可回收空间">
              <Tooltip title={`${formatExact(stats.free_bytes)} bytes`}>
                {formatBytes(stats.free_bytes)}
              </Tooltip>
            </Descriptions.Item>
          </Descriptions>
          <Space wrap style={{ marginTop: 16 }}>
            <Tag>提供商 {formatCompact(stats.providers)}</Tag>
            <Tag>模型 {formatCompact(stats.provider_models)}</Tag>
            <Tag>上游密钥 {formatCompact(stats.provider_api_keys)}</Tag>
            <Tag>路由 {formatCompact(stats.routes)}</Tag>
            <Tag>访问密钥 {formatCompact(stats.access_keys)}</Tag>
            <Tag>审计日志 {formatCompact(stats.audit_logs)}</Tag>
            <Tag>请求日志 {formatCompact(stats.usage_logs)}</Tag>
            <Tag color={stats.in_flight_requests ? 'processing' : 'default'}>
              请求中 {formatCompact(stats.in_flight_requests)}
            </Tag>
          </Space>
        </>
      ) : (
        <Typography.Text type="secondary">正在读取数据库状态…</Typography.Text>
      )}
    </Card>
  )
}
