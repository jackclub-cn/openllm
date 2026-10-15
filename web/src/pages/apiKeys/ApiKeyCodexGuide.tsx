import { CopyOutlined, DownloadOutlined } from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Modal,
  Space,
  Tabs,
  Tag,
  Tooltip,
  Typography,
} from 'antd'

type ApiKeyCodexGuideProps = {
  open: boolean
  onCancel: () => void
}

const SCRIPT_REVISION = '1'

export default function ApiKeyCodexGuide({
  open,
  onCancel,
}: ApiKeyCodexGuideProps) {
  const origin = window.location.origin
  const scriptBase = `${origin}/codex`
  const powershellUrl = `${scriptBase}/organize-relay-catalog.ps1?v=${SCRIPT_REVISION}`
  const shellUrl = `${scriptBase}/organize-relay-catalog.sh?v=${SCRIPT_REVISION}`
  const baseUrl = `${origin}/v1`

  const config = `model = "coding"
model_provider = "openllm"
model_catalog_json = "model-catalogs/relay-mtwm81nx.json"

[model_providers.openllm]
name = "OpenLLM"
base_url = "${baseUrl}"
wire_api = "responses"
env_key = "OPENLLM_API_KEY"`

  const auth = `{
  "OPENLLM_API_KEY": "<你的密钥>"
}`

  const powershellCommand = `$k="<你的密钥>"; $a="$env:USERPROFILE\\.codex\\auth.json"; New-Item -ItemType Directory -Force (Split-Path $a) | Out-Null; $j=if (Test-Path -LiteralPath $a) { Get-Content -Raw -LiteralPath $a | ConvertFrom-Json } else { [pscustomobject]@{} }; if ($null -eq $j) { $j=[pscustomobject]@{} }; $j | Add-Member -Force -NotePropertyName OPENLLM_API_KEY -NotePropertyValue $k; $j | ConvertTo-Json -Depth 100 | Set-Content -LiteralPath $a -Encoding utf8; $d="$env:USERPROFILE\\.codex\\model-catalogs"; New-Item -ItemType Directory -Force $d | Out-Null; irm "${powershellUrl}" -OutFile "$d\\organize-relay-catalog.ps1"; pwsh -File "$d\\organize-relay-catalog.ps1"`

  const shellCommand = `a="$HOME/.codex/auth.json"; k="<你的密钥>"; mkdir -p "$HOME/.codex"; python3 -c 'import json,os,sys; p=sys.argv[1]; d=json.load(open(p,encoding="utf-8")) if os.path.exists(p) and os.path.getsize(p) else {}; d["OPENLLM_API_KEY"]=sys.argv[2]; open(p,"w",encoding="utf-8").write(json.dumps(d,ensure_ascii=False,indent=2)+"\\n"); os.chmod(p,0o600)' "$a" "$k"; d="$HOME/.codex/model-catalogs"; mkdir -p "$d" && curl -fsSL "${shellUrl}" -o "$d/organize-relay-catalog.sh" && chmod +x "$d/organize-relay-catalog.sh" && "$d/organize-relay-catalog.sh"`

  return (
    <Modal
      title="在 Codex 中使用 OpenLLM"
      open={open}
      onCancel={onCancel}
      footer={
        <Button type="primary" onClick={onCancel}>
          知道了
        </Button>
      }
      width={760}
      destroyOnHidden
    >
      <Space direction="vertical" size={24} style={{ width: '100%' }}>
        <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
          Codex 通过 <Typography.Text code>config.toml</Typography.Text> 和{' '}
          <Typography.Text code>auth.json</Typography.Text>{' '}
          读取网关地址、密钥和模型目录。下面命令会自动下载导入脚本，读取本机 Codex
          配置，并生成
          <Typography.Text code>~/.codex/model-catalogs/relay-mtwm81nx.json</Typography.Text>。
        </Typography.Paragraph>

        <section>
          <Space align="center" style={{ marginBottom: 10 }}>
            <Tag color="blue">1</Tag>
            <Typography.Title level={5} style={{ margin: 0 }}>
              创建访问密钥
            </Typography.Title>
          </Space>
          <Typography.Paragraph style={{ marginBottom: 0 }}>
            先在当前页面创建一个密钥并立即保存。下面命令中的{' '}
            <Typography.Text code>&lt;你的密钥&gt;</Typography.Text>{' '}
            需要替换为这个值。
          </Typography.Paragraph>
        </section>

        <section>
          <Space align="center" style={{ marginBottom: 10 }}>
            <Tag color="blue">2</Tag>
            <Typography.Title level={5} style={{ margin: 0 }}>
              配置 Codex
            </Typography.Title>
          </Space>
          <Typography.Paragraph type="secondary">
            合并到 <Typography.Text code>~/.codex/config.toml</Typography.Text>。
            顶层字段要放在第一个 <Typography.Text code>[...]</Typography.Text>{' '}
            配置表之前；如果已有同名的 model provider，请只补齐缺失项。密钥写入{' '}
            <Typography.Text code>~/.codex/auth.json</Typography.Text>。
          </Typography.Paragraph>
          <Space direction="vertical" size={12} style={{ width: '100%' }}>
            <CommandBlock label="config.toml" command={config} />
            <CommandBlock label="auth.json" command={auth} />
          </Space>
          <Alert
            type="warning"
            showIcon
            style={{ marginTop: 12 }}
            message="auth.json 可能已有 Codex 的其它认证字段。请只补充 OPENLLM_API_KEY，不要整文件覆盖；下面的一键命令会自动合并。"
          />
        </section>

        <section>
          <Space align="center" style={{ marginBottom: 10 }}>
            <Tag color="blue">3</Tag>
            <Typography.Title level={5} style={{ margin: 0 }}>
              一条命令导入模型
            </Typography.Title>
          </Space>
          <Tabs
            items={[
              {
                key: 'powershell',
                label: 'Windows PowerShell',
                children: (
                  <CommandBlock
                    label="PowerShell"
                    command={powershellCommand}
                    hint="需要 PowerShell 7（pwsh）。"
                  />
                ),
              },
              {
                key: 'shell',
                label: 'macOS / Linux',
                children: (
                  <CommandBlock
                    label="bash / zsh"
                    command={shellCommand}
                    hint="需要 Python 3.11+，旧版本可先安装 tomli。"
                  />
                ),
              },
            ]}
          />
          <Typography.Paragraph type="secondary" style={{ marginTop: 8 }}>
            导入完成后重启 Codex。以后 OpenLLM 模型清单有变化时，重新运行同一条命令即可。
          </Typography.Paragraph>
        </section>

        <section>
          <Typography.Title level={5}>脚本下载</Typography.Title>
          <Space wrap>
            <Button
              icon={<DownloadOutlined />}
              href={powershellUrl}
              download="organize-relay-catalog.ps1"
              target="_blank"
            >
              PowerShell 脚本
            </Button>
            <Button
              icon={<DownloadOutlined />}
              href={shellUrl}
              download="organize-relay-catalog.sh"
              target="_blank"
            >
              shell 脚本
            </Button>
          </Space>
        </section>
      </Space>
    </Modal>
  )
}

type CommandBlockProps = {
  label: string
  command: string
  hint?: string
}

function CommandBlock({ label, command, hint }: CommandBlockProps) {
  const { message } = App.useApp()

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command)
      message.success('已复制')
    } catch {
      message.error('复制失败，请手动选择内容')
    }
  }

  return (
    <div className="codex-command">
      <div className="codex-command-head">
        <div>
          <Typography.Text strong>{label}</Typography.Text>
          {hint && <Typography.Text type="secondary">{hint}</Typography.Text>}
        </div>
        <Tooltip title="复制">
          <Button
            type="text"
            size="small"
            icon={<CopyOutlined />}
            aria-label={`复制${label}`}
            onClick={() => void copy()}
          />
        </Tooltip>
      </div>
      <pre>{command}</pre>
    </div>
  )
}
