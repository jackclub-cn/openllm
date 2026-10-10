import { useEffect, useState } from 'react'
import { HistoryOutlined } from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Descriptions,
  Empty,
  Input,
  Modal,
  Select,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type RouteDiagnose,
  type RouteDiagnoseRuntimeTarget,
} from '../../api'
import {
  diagnosticEndpoints,
  diagnosisReasonLabels,
  matchTypeLabels,
  strategyLabels,
} from './types'

export type RouteDiagnosisRequest = {
  model?: string
  endpoint?: string
  sessionId?: string
}

const decisionReasonLabels: Record<string, string> = {
  lowest_known_cost: '最低已知成本',
  known_cost: '已知成本',
  unknown_cost: '价格未知',
  lowest_recent_latency: '最低近期延迟',
  recent_latency: '已有延迟样本',
  no_recent_latency: '无近期延迟样本',
  least_recent_traffic: '近期负载最低',
  recent_traffic: '已有近期负载',
  no_recent_traffic: '无近期负载',
  priority_order: '优先级顺序',
  session_affinity: '会话粘性',
  provider_health_failed: '提供商健康异常',
}

function runtimeMetric(record: RouteDiagnoseRuntimeTarget) {
  const metrics = []
  if (record.input_cost_per_million != null || record.output_cost_per_million != null) {
    const input =
      record.input_cost_per_million != null
        ? `$${record.input_cost_per_million.toFixed(2)}`
        : '-'
    const output =
      record.output_cost_per_million != null
        ? `$${record.output_cost_per_million.toFixed(2)}`
        : '-'
    metrics.push(`输入 ${input}/M · 输出 ${output}/M`)
  }
  if (record.avg_latency_ms != null && Number.isFinite(record.avg_latency_ms)) {
    metrics.push(`延迟 ${Math.round(record.avg_latency_ms)} ms`)
  }
  if (record.recent_requests != null) {
    metrics.push(`请求 ${record.recent_requests}`)
  }
  return metrics.join(' · ')
}

type RouteDiagnosisModalProps = {
  request?: RouteDiagnosisRequest
  onClose: () => void
}

export default function RouteDiagnosisModal({
  request,
  onClose,
}: RouteDiagnosisModalProps) {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [model, setModel] = useState('')
  const [endpoint, setEndpoint] = useState('/v1/chat/completions')
  const [sessionId, setSessionId] = useState('')
  const [diagnosing, setDiagnosing] = useState(false)
  const [diagnosis, setDiagnosis] = useState<RouteDiagnose>()

  const runDiagnosis = async (
    modelValue = model,
    endpointValue = endpoint,
    sessionValue = sessionId,
  ) => {
    const nextModel = modelValue.trim()
    const nextSessionId = sessionValue.trim()
    if (!nextModel) {
      message.warning('请输入要诊断的模型')
      return
    }
    setDiagnosing(true)
    try {
      setDiagnosis(
        await api.post<RouteDiagnose>('/api/routes/diagnose', {
          model: nextModel,
          endpoint: endpointValue,
          session_id: nextSessionId || undefined,
        }),
      )
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setDiagnosing(false)
    }
  }

  useEffect(() => {
    if (!request) return
    const nextModel = request.model?.trim() || ''
    const nextEndpoint = request.endpoint || '/v1/chat/completions'
    const nextSessionId = request.sessionId?.trim() || ''
    setModel(nextModel)
    setEndpoint(nextEndpoint)
    setSessionId(nextSessionId)
    setDiagnosis(undefined)
    if (nextModel) void runDiagnosis(nextModel, nextEndpoint, nextSessionId)
  }, [request])

  return (
    <Modal
      title="路由诊断"
      open={Boolean(request)}
      onCancel={onClose}
      footer={null}
      width={920}
      destroyOnHidden
    >
      <Space direction="vertical" size={16} style={{ width: '100%' }}>
        <Space.Compact block>
          <Input
            autoFocus
            value={model}
            onChange={(event) => setModel(event.target.value)}
            onPressEnter={() => void runDiagnosis()}
            placeholder="输入客户端实际使用的模型名称"
          />
          <Select
            value={endpoint}
            options={diagnosticEndpoints}
            onChange={setEndpoint}
            style={{ width: 220 }}
          />
          <Button type="primary" loading={diagnosing} onClick={() => void runDiagnosis()}>
            诊断
          </Button>
        </Space.Compact>
        <Input
          allowClear
          value={sessionId}
          onChange={(event) => setSessionId(event.target.value)}
          onPressEnter={() => void runDiagnosis()}
          placeholder="会话 ID（可选，用于查看粘性路由顺序）"
        />

        {diagnosis && (
          <>
            <Alert
              showIcon
              type={diagnosis.resolved ? 'success' : 'error'}
              message={diagnosis.resolved ? '路由可用' : '路由不可用'}
              description={diagnosis.message}
            />
            <Descriptions size="small" bordered column={2}>
              <Descriptions.Item label="匹配方式">
                {matchTypeLabels[diagnosis.match_type]}
              </Descriptions.Item>
              <Descriptions.Item label="路由">{diagnosis.route_name || '-'}</Descriptions.Item>
              <Descriptions.Item label="策略">
                {diagnosis.strategy ? strategyLabels[diagnosis.strategy] : '-'}
              </Descriptions.Item>
              <Descriptions.Item label="能力桶">
                {diagnosis.barrel
                  ? [
                      diagnosis.barrel.context_limit
                        ? `上下文 ${diagnosis.barrel.context_limit}`
                        : '',
                      diagnosis.barrel.output_limit
                        ? `输出 ${diagnosis.barrel.output_limit}`
                        : '',
                    ]
                      .filter(Boolean)
                      .join(' / ') || '-'
                  : '-'}
                {diagnosis.barrel_incomplete ? '（信息不完整）' : ''}
              </Descriptions.Item>
            </Descriptions>
            {diagnosis.runtime_targets && diagnosis.runtime_targets.length > 0 && (
              <>
                <Space>
                  <Typography.Text strong>会话实际顺序</Typography.Text>
                  {diagnosis.session_id && (
                    <Button
                      type="link"
                      size="small"
                      icon={<HistoryOutlined />}
                      onClick={() => {
                        const params = new URLSearchParams({
                          session_id: diagnosis.session_id!,
                        })
                        navigate(`/usage?${params}`)
                      }}
                    >
                      查看会话日志
                    </Button>
                  )}
                </Space>
                <Table
                  rowKey={(record) =>
                    `${record.order}-${record.provider_id}-${record.upstream_model}-${record.provider_api_key_id ?? 0}`
                  }
                  size="small"
                  pagination={false}
                  dataSource={diagnosis.runtime_targets}
                  scroll={{ x: 1080 }}
                  columns={[
                    {
                      title: '顺序',
                      dataIndex: 'order',
                      width: 70,
                    },
                    {
                      title: '提供商',
                      dataIndex: 'provider_name',
                      width: 170,
                      render: (value: string, record) => (
                        <Space size={4}>
                          <Typography.Text strong>{value}</Typography.Text>
                          {record.provider_health === false && <Tag color="error">异常</Tag>}
                        </Space>
                      ),
                    },
                    {
                      title: '上游模型',
                      dataIndex: 'upstream_model',
                      width: 210,
                      render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                    },
                    {
                      title: '上游 Key',
                      dataIndex: 'provider_api_key_name',
                      width: 140,
                      render: (value?: string | null) =>
                        value ? <Typography.Text>{value}</Typography.Text> : '-',
                    },
                    {
                      title: '近期指标',
                      width: 250,
                      render: (_, record) =>
                        runtimeMetric(record) || <Typography.Text type="secondary">-</Typography.Text>,
                    },
                    {
                      title: '决策依据',
                      dataIndex: 'decision_reason',
                      width: 150,
                      render: (value: string) => (
                        <Tag color={value.includes('lowest') || value === 'least_recent_traffic' ? 'success' : 'default'}>
                          {decisionReasonLabels[value] || value}
                        </Tag>
                      ),
                    },
                  ]}
                />
              </>
            )}
            {diagnosis.targets.length ? (
              <Table
                rowKey={(record) => `${record.provider_id}-${record.upstream_model}`}
                size="small"
                pagination={false}
                dataSource={diagnosis.targets}
                scroll={{ x: 820 }}
                columns={[
                  {
                    title: '状态',
                    dataIndex: 'eligible',
                    width: 80,
                    render: (eligible: boolean) => (
                      <Tag color={eligible ? 'success' : 'error'}>
                        {eligible ? '可用' : '跳过'}
                      </Tag>
                    ),
                  },
                  {
                    title: '提供商',
                    dataIndex: 'provider_name',
                    width: 150,
                    render: (value: string, record) => (
                      <Space size={4}>
                        <Typography.Text strong>{value}</Typography.Text>
                        {record.provider_health === false && <Tag color="error">异常</Tag>}
                      </Space>
                    ),
                  },
                  {
                    title: '上游模型',
                    dataIndex: 'upstream_model',
                    width: 180,
                    render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                  },
                  {
                    title: '支持接口',
                    dataIndex: 'supported_endpoints',
                    width: 220,
                    render: (endpoints: string[]) =>
                      endpoints.length ? (
                        endpoints.map((item) => (
                          <Tag key={item}>{item.replace(/^\/v1/, '')}</Tag>
                        ))
                      ) : (
                        <Typography.Text type="secondary">未声明</Typography.Text>
                      ),
                  },
                  {
                    title: '原因',
                    dataIndex: 'reason',
                    render: (value: string) => (
                      <Typography.Text type={value === 'eligible' ? 'success' : 'secondary'}>
                        {diagnosisReasonLabels[value] || value}
                      </Typography.Text>
                    ),
                  },
                ]}
              />
            ) : (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有匹配到候选目标" />
            )}
          </>
        )}
      </Space>
    </Modal>
  )
}
