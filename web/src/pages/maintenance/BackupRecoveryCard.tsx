import {
  SafetyOutlined,
} from '@ant-design/icons'
import {
  Alert,
  Button,
  Card,
  Descriptions,
  Empty,
  Table,
  Tag,
  Typography,
} from 'antd'
import type { BackupInfo, DatabaseFileReport } from '../../api'
import { formatTimestamp, humanSize } from './format'

type Props = {
  backups: BackupInfo[]
  loading: boolean
  busy?: string
  dataDirectory: string
  report?: DatabaseFileReport
  reportName: string
  onVerify: (name: string) => void
}

export default function BackupRecoveryCard({
  backups,
  loading,
  busy,
  dataDirectory,
  report,
  reportName,
  onVerify,
}: Props) {
  return (
    <Card title="备份与恢复" style={{ marginBottom: 16 }}>
      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 12 }}
        message="恢复必须在网关停止时进行"
        description={
          <>
            单实例网关的数据库只能被一个进程写入。恢复步骤：停止网关（等待排空完成）→
            执行{' '}
            <Typography.Text code>
              openllm --data-dir {dataDirectory} --list-backups
            </Typography.Text>{' '}
            查看备份 → 执行{' '}
            <Typography.Text code>
              openllm --data-dir {dataDirectory} --restore-backup &lt;文件名&gt;
            </Typography.Text>{' '}
            → 启动网关并确认 <Typography.Text code>GET /api/ready</Typography.Text>{' '}
            返回 200。备份中的实例租约会被自动清除，因此可以直接恢复到全新数据目录。
          </>
        }
      />
      <Table<BackupInfo>
        size="small"
        rowKey="name"
        loading={loading}
        dataSource={backups}
        locale={{
          emptyText: (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="还没有备份：点击右上角“立即备份”，或让启动迁移前自动生成的备份累积"
            />
          ),
        }}
        pagination={{ pageSize: 10, hideOnSinglePage: true }}
        columns={[
          {
            title: '文件',
            dataIndex: 'name',
            render: (name: string) => <Typography.Text code>{name}</Typography.Text>,
          },
          {
            title: '类型',
            dataIndex: 'kind',
            width: 120,
            render: (kind: string) => (
              <Tag color={kind === 'manual' ? 'blue' : 'default'}>
                {kind === 'manual' ? '手动' : '迁移前'}
              </Tag>
            ),
          },
          {
            title: '大小',
            dataIndex: 'size_bytes',
            width: 110,
            render: (bytes: number) => humanSize(bytes),
          },
          {
            title: '创建时间',
            dataIndex: 'created_at',
            width: 200,
            render: (value: string) => formatTimestamp(value),
          },
          {
            title: '操作',
            width: 100,
            render: (_, row) => (
              <Button
                size="small"
                icon={<SafetyOutlined />}
                loading={busy === `verify:${row.name}`}
                onClick={() => onVerify(row.name)}
              >
                校验
              </Button>
            ),
          },
        ]}
      />
      {report && (
        <div style={{ marginTop: 16 }}>
          <Alert
            type={report.ok ? 'success' : 'error'}
            showIcon
            message={
              report.ok
                ? `${reportName} 可以用于恢复`
                : `${reportName} 存在问题，不要用它恢复`
            }
            description={
              <>
                <Descriptions size="small" column={3} style={{ marginTop: 8 }}>
                  <Descriptions.Item label="完整性">{report.integrity}</Descriptions.Item>
                  <Descriptions.Item label="大小">
                    {humanSize(report.size_bytes)}
                  </Descriptions.Item>
                  <Descriptions.Item label="表">{report.tables.length} 个</Descriptions.Item>
                  <Descriptions.Item label="已应用迁移">
                    {report.applied_migrations}
                  </Descriptions.Item>
                  <Descriptions.Item label="提供商">{report.providers}</Descriptions.Item>
                  <Descriptions.Item label="路由">{report.routes}</Descriptions.Item>
                  <Descriptions.Item label="API Key">{report.api_keys}</Descriptions.Item>
                  <Descriptions.Item label="Webhook">{report.webhooks}</Descriptions.Item>
                  <Descriptions.Item label="配额池">{report.provider_pools}</Descriptions.Item>
                </Descriptions>
                {report.missing_tables.length > 0 && (
                  <Typography.Paragraph type="danger" style={{ marginBottom: 0 }}>
                    缺少数据表：{report.missing_tables.join('、')}
                  </Typography.Paragraph>
                )}
                {report.problems.map((problem) => (
                  <Typography.Paragraph key={problem} type="danger" style={{ marginBottom: 0 }}>
                    {problem}
                  </Typography.Paragraph>
                ))}
              </>
            }
          />
        </div>
      )}
    </Card>
  )
}
