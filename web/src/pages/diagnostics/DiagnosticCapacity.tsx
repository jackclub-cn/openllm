import { Card, Col, Progress, Row, Statistic, Typography } from 'antd'
import { type DiagnosticSnapshot } from '../../api'

type Props = { snapshot?: DiagnosticSnapshot }
export default function DiagnosticCapacity({ snapshot }: Props) {
  const admission = snapshot?.status.admission
  const memory = snapshot?.status.memory
  const database = snapshot?.status.database
  return (
    <Card title="容量与并发">
      <Row gutter={[16, 16]}>
        <Col xs={12} md={6}>
          <Statistic
            title="在途请求 / 并发上限"
            value={admission?.in_flight ?? 0}
            suffix={`/ ${admission?.max_concurrent_requests || '不限'}`}
          />
        </Col>
        <Col xs={12} md={6}>
          <Statistic title="准入卸载请求" value={admission?.requests_shed_total ?? 0} />
        </Col>
        <Col xs={12} md={6}>
          <Statistic
            title="请求体占用 / 上限 (MiB)"
            value={(admission?.in_flight_request_mib ?? 0).toFixed(2)}
            suffix={`/ ${admission?.max_inflight_request_mib || '不限'}`}
          />
        </Col>
        <Col xs={12} md={6}>
          <Statistic
            title="运行时阻塞"
            value={snapshot?.status.runtime_lag_millis ?? 0}
            suffix="ms"
            valueStyle={
              (snapshot?.status.runtime_lag_millis ?? 0) > 5000
                ? { color: '#d4380d' }
                : undefined
            }
          />
        </Col>
        <Col xs={24} md={12}>
          <Typography.Text type="secondary">
            内存{' '}
            {memory ? `${memory.used_mib ?? 0} / ${memory.limit_mib || '不限'} MiB` : '—'}
            {memory && !memory.limit_mib
              ? `（采样来源：${memory.source}${
                  memory.source === 'off' ? '，未启用内存保护' : ''
                }）`
              : ''}
          </Typography.Text>
          <Progress
            percent={
              memory?.used_ratio ? Math.min(100, Math.round(memory.used_ratio * 100)) : 0
            }
            status={memory?.pressure ? 'exception' : 'normal'}
            showInfo={Boolean(memory?.used_ratio)}
          />
        </Col>
        <Col xs={24} md={12}>
          <Typography.Text type="secondary">
            数据库连接{' '}
            {database
              ? `${database.connections} 已用 / ${database.idle} 空闲 / ${database.max_connections} 上限`
              : '—'}
          </Typography.Text>
          <div>
            <Typography.Text type="secondary">
              内存卸载 {memory?.shed_total ?? 0} 次 · 请求体卸载{' '}
              {admission?.request_bytes_shed_total ?? 0} 次
            </Typography.Text>
          </div>
        </Col>
      </Row>
    </Card>
  )
}
