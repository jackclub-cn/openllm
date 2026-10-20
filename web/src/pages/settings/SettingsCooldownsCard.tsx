import { ClearOutlined, ClockCircleOutlined, ReloadOutlined } from '@ant-design/icons'
import { Button, Card, Popconfirm, Space, Table, Tag, Typography } from 'antd'
import type { SettingsState, CooldownRow } from './useSettings'

type Props = Pick<
  SettingsState,
  | 'loadingCooldowns'
  | 'clearingCooldowns'
  | 'loadCooldowns'
  | 'clearCooldowns'
  | 'cooldownRows'
>
export default function SettingsCooldownsCard({ state }: { state: Props }) {
  const {
    loadingCooldowns,
    clearingCooldowns,
    loadCooldowns,
    clearCooldowns,
    cooldownRows,
  } = state
  return (
    <Card
      className="settings-retention"
      title={
        <Space>
          <ClockCircleOutlined />
          冷却管理
        </Space>
      }
      bordered={false}
      extra={
        <Space>
          <Button
            icon={<ReloadOutlined />}
            loading={loadingCooldowns}
            onClick={loadCooldowns}
          >
            刷新
          </Button>
          <Popconfirm
            title="清除全部冷却？"
            onConfirm={() => void clearCooldowns({ all: true })}
          >
            <Button
              danger
              icon={<ClearOutlined />}
              disabled={!cooldownRows.length}
              loading={clearingCooldowns}
            >
              全部清除
            </Button>
          </Popconfirm>
        </Space>
      }
    >
      <Typography.Paragraph type="secondary">
        冷却与熔断只存在于当前进程内，处于冷却的提供商、模型或上游密钥会暂时退出路由；连续失败达到熔断阈值的提供商会直接短路，冷却结束后只放行一个探测请求验证恢复。上游已经恢复时可以在这里提前解除。
      </Typography.Paragraph>
      <Table
        rowKey="key"
        size="small"
        loading={loadingCooldowns}
        dataSource={cooldownRows}
        pagination={false}
        locale={{ emptyText: '当前没有冷却或熔断中的目标' }}
        columns={[
          {
            title: '范围',
            dataIndex: 'scope',
            width: 100,
            render: (value: string) => <Tag>{value}</Tag>,
          },
          {
            title: '目标',
            dataIndex: 'target',
          },
          {
            title: '备注',
            dataIndex: 'detail',
            width: 160,
            render: (value: string) =>
              value || <Typography.Text type="secondary">-</Typography.Text>,
          },
          {
            title: '剩余',
            dataIndex: 'remaining',
            width: 100,
            render: (value: number, record: CooldownRow) =>
              record.circuit === 'half_open' ? '待探测' : `${value}s`,
          },
          {
            title: '操作',
            width: 100,
            render: (_, record) => (
              <Button
                size="small"
                loading={clearingCooldowns}
                onClick={() => void clearCooldowns(record.clear)}
              >
                清除
              </Button>
            ),
          },
        ]}
      />
    </Card>
  )
}
