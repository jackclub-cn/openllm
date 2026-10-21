import { useRef, useState } from 'react'
import { CloudDownloadOutlined, CloudUploadOutlined } from '@ant-design/icons'
import { Alert, App, Button, Card, Checkbox, Space, Table, Tag, Typography } from 'antd'
import {
  api,
  formatError,
  type ConfigDocument,
  type ImportAction,
  type ImportItem,
  type ImportPlan,
} from '../../api'

const actionMeta: Record<ImportAction, { color: string; label: string }> = {
  create: { color: 'green', label: '新增' },
  update: { color: 'blue', label: '更新' },
  unchanged: { color: 'default', label: '未变化' },
  skip: { color: 'orange', label: '跳过' },
  error: { color: 'red', label: '错误' },
}

export default function ConfigTransferCard({ onImported }: { onImported: () => Promise<void> }) {
  const { message } = App.useApp()
  const [busy, setBusy] = useState<string>()
  const [includeSecrets, setIncludeSecrets] = useState(false)
  const [configDocument, setConfigDocument] = useState<ConfigDocument>()
  const [documentName, setDocumentName] = useState('')
  const [apply, setApply] = useState(false)
  const [plan, setPlan] = useState<ImportPlan>()
  const fileInput = useRef<HTMLInputElement>(null)

  const exportConfig = async () => {
    setBusy('export')
    try {
      const query = includeSecrets ? '?include_secrets=true' : ''
      const blob = await api.download(`/api/config/export${query}`)
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = `openllm-config-${new Date().toISOString().replace(/[:.]/g, '-')}.json`
      anchor.click()
      URL.revokeObjectURL(url)
      message.success(includeSecrets ? '已导出（包含明文凭据，请妥善保管）' : '已导出（凭据已掩码）')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const pickDocument = async (file?: File) => {
    if (!file) return
    try {
      const parsed = JSON.parse(await file.text()) as ConfigDocument
      if (typeof parsed.openllm_config_version !== 'number') {
        message.error('该文件不是 OpenLLM 配置文档（缺少 openllm_config_version）')
        return
      }
      setConfigDocument(parsed)
      setDocumentName(file.name)
      setPlan(undefined)
      message.success(
        `已载入 ${file.name}：${parsed.providers?.length ?? 0} 个提供商 / ${parsed.routes?.length ?? 0} 条路由 / ${parsed.secrets_included ? '包含明文凭据' : '凭据已掩码'}`,
      )
    } catch (error) {
      message.error(`读取失败：${formatError(error)}`)
    }
  }

  const importDocument = async (write: boolean) => {
    if (!configDocument) {
      message.warning('请先选择配置文件')
      return
    }
    setBusy(write ? 'import' : 'preview')
    try {
      const result = await api.post<ImportPlan>('/api/config/import', {
        document: configDocument,
        apply: write,
      })
      setPlan(result)
      if (result.problems.length > 0) {
        message.warning(write ? '文档有问题，未写入任何配置' : '文档有问题，请先修正')
      } else if (write) {
        message.success(`已写入：新增 ${result.counts.create} / 更新 ${result.counts.update}`)
      } else {
        message.success('校验通过，未写入任何配置')
      }
      if (write) await onImported()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  return (
    <Card
      title="配置导入 / 导出"
      style={{ marginBottom: 16 }}
      extra={
        <Space>
          <Checkbox
            checked={includeSecrets}
            onChange={(event) => setIncludeSecrets(event.target.checked)}
          >
            包含明文上游凭据
          </Checkbox>
          <Button
            icon={<CloudDownloadOutlined />}
            loading={busy === 'export'}
            onClick={() => void exportConfig()}
          >
            导出配置
          </Button>
        </Space>
      }
    >
      <Typography.Paragraph type="secondary" style={{ marginBottom: 8 }}>
        导出的文档包含设置项、提供商（含模型启用状态）、路由、配额池与 Webhook，引用一律使用名称，
        因此可以导入到另一套网关。默认情况下上游密钥、Webhook 签名与凭据类请求头都会掩码为
        <Typography.Text code>***</Typography.Text>，掩码值永远不会被导入覆盖。
        网关 API Key 只保存哈希，只能按名称提示，无法导出或导入。
      </Typography.Paragraph>
      {includeSecrets && (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 8 }}
          message="勾选后导出的文件包含明文凭据"
          description="导出与导入都会写入审计日志，但文件本身就是密钥材料：请限制访问权限，并避免提交到代码仓库。"
        />
      )}
      <Space wrap>
        <input
          ref={fileInput}
          type="file"
          accept=".json,application/json"
          style={{ display: 'none' }}
          onChange={(event) => {
            void pickDocument(event.target.files?.[0])
            event.target.value = ''
          }}
        />
        <Button icon={<CloudUploadOutlined />} onClick={() => fileInput.current?.click()}>
          选择配置文件
        </Button>
        <Typography.Text type={configDocument ? undefined : 'secondary'}>
          {configDocument ? `${documentName}（v${configDocument.openllm_config_version}）` : '尚未选择文件'}
        </Typography.Text>
        <Button onClick={() => void importDocument(false)} loading={busy === 'preview'}>
          仅校验
        </Button>
        <Checkbox checked={apply} onChange={(event) => setApply(event.target.checked)}>
          写入配置
        </Checkbox>
        <Button
          type="primary"
          disabled={!apply}
          loading={busy === 'import'}
          onClick={() => void importDocument(true)}
        >
          导入
        </Button>
      </Space>
      {plan && (
        <div style={{ marginTop: 16 }}>
          {plan.problems.length > 0 && (
            <Alert
              type="error"
              showIcon
              style={{ marginBottom: 12 }}
              message={plan.applied ? '已写入可用部分' : '文档存在问题，未写入任何配置'}
              description={
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  {plan.problems.map((problem) => <li key={problem}>{problem}</li>)}
                </ul>
              }
            />
          )}
          {plan.notes.length > 0 && (
            <Alert
              type="info"
              showIcon
              style={{ marginBottom: 12 }}
              message="导入说明"
              description={
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  {plan.notes.map((note) => <li key={note}>{note}</li>)}
                </ul>
              }
            />
          )}
          <Space wrap style={{ marginBottom: 12 }}>
            <Tag color="green">新增 {plan.counts.create}</Tag>
            <Tag color="blue">更新 {plan.counts.update}</Tag>
            <Tag>未变化 {plan.counts.unchanged}</Tag>
            <Tag color="orange">跳过 {plan.counts.skip}</Tag>
            <Tag color={plan.counts.error > 0 ? 'red' : 'default'}>错误 {plan.counts.error}</Tag>
            <Tag>{plan.applied ? '已写入' : '仅校验'}</Tag>
            <Tag>{plan.secrets_included ? '含明文凭据' : '凭据已掩码'}</Tag>
          </Space>
          <Table<ImportItem>
            size="small"
            rowKey={(row) => `${row.kind}:${row.name}`}
            dataSource={plan.items}
            pagination={{ pageSize: 10, hideOnSinglePage: true }}
            columns={[
              { title: '类型', dataIndex: 'kind', width: 120 },
              { title: '名称', dataIndex: 'name', width: 200 },
              {
                title: '动作',
                dataIndex: 'action',
                width: 100,
                render: (action: ImportAction) => (
                  <Tag color={actionMeta[action].color}>{actionMeta[action].label}</Tag>
                ),
              },
              { title: '说明', dataIndex: 'detail' },
            ]}
          />
        </div>
      )}
    </Card>
  )
}
