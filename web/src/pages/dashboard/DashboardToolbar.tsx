import {
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Button, DatePicker, Tag, Tooltip, Typography } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import PageHeader from '../../components/PageHeader'

type DashboardToolbarProps = {
  dates: [Dayjs, Dayjs]
  lastUpdated?: Date
  refreshing: boolean
  paused: boolean
  realtimeConnected: boolean
  onDatesChange: (dates: [Dayjs, Dayjs]) => void
  onTogglePaused: () => void
  onRefresh: () => void
}

export default function DashboardToolbar({
  dates,
  lastUpdated,
  refreshing,
  paused,
  realtimeConnected,
  onDatesChange,
  onTogglePaused,
  onRefresh,
}: DashboardToolbarProps) {
  const rangePresets = [
    { label: '今天', value: [dayjs().startOf('day'), dayjs().endOf('day')] as [Dayjs, Dayjs] },
    {
      label: '近 7 天',
      value: [dayjs().subtract(6, 'day').startOf('day'), dayjs().endOf('day')] as [
        Dayjs,
        Dayjs,
      ],
    },
    {
      label: '近 14 天',
      value: [dayjs().subtract(13, 'day').startOf('day'), dayjs().endOf('day')] as [
        Dayjs,
        Dayjs,
      ],
    },
    {
      label: '近 30 天',
      value: [dayjs().subtract(29, 'day').startOf('day'), dayjs().endOf('day')] as [
        Dayjs,
        Dayjs,
      ],
    },
  ]

  return (
    <PageHeader
      title="运行概览"
      description="网关请求、模型令牌与提供商健康状态"
      extra={
        <>
          <DatePicker.RangePicker
            allowClear={false}
            value={dates}
            presets={rangePresets}
            disabledDate={(current) => Boolean(current && current > dayjs().endOf('day'))}
            onChange={(value) => {
              if (value?.[0] && value[1]) onDatesChange(value as [Dayjs, Dayjs])
            }}
          />
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
