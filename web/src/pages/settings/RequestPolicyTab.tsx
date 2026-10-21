import {
  ReloadOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons'
import { Form, Input, InputNumber, Switch } from 'antd'
import type { GuardrailSettings, ResilienceSettings } from '../../api'
import SettingsSection from './SettingsSection'
import useSettingsSection from './useSettingsSection'

export default function RequestPolicyTab() {
  const guardrails = useSettingsSection<GuardrailSettings>(
    '/api/settings/guardrails',
    '内容防护策略已保存',
    (current) => ({
      ...current,
      blocked_terms: current.blocked_terms
        .map((term) => term.trim())
        .filter(Boolean),
      max_prompt_tokens:
        current.max_prompt_tokens && current.max_prompt_tokens > 0
          ? current.max_prompt_tokens
          : null,
    })
  )
  const resilience = useSettingsSection<ResilienceSettings>(
    '/api/settings/resilience',
    '故障恢复策略已保存'
  )
  const updateGuardrails = (patch: Partial<GuardrailSettings>) =>
    guardrails.setValue((current) => (current ? { ...current, ...patch } : current))
  const updateResilience = (patch: Partial<ResilienceSettings>) =>
    resilience.setValue((current) => (current ? { ...current, ...patch } : current))

  return (
    <>
      <SettingsSection
        title="内容防护"
        icon={<SafetyCertificateOutlined />}
        state={guardrails}
      >
        {guardrails.value && (
          <>
            <Form.Item
              label="阻止词"
              extra="每行一个，大小写不敏感；仅扫描提示词文本，不扫描图片和 Base64 内容。"
            >
              <Input.TextArea
                rows={5}
                value={guardrails.value.blocked_terms.join('\n')}
                onChange={(event) =>
                  updateGuardrails({ blocked_terms: event.target.value.split(/\r?\n/) })
                }
                placeholder={'例如：\ninternal-secret\n禁止分享的客户名'}
              />
            </Form.Item>
            <Form.Item
              label="最大提示词 Token"
              extra="按网关估算值在路由前拦截；留空或设为 0 表示不限制。"
            >
              <InputNumber
                min={0}
                max={10000000}
                precision={0}
                value={guardrails.value.max_prompt_tokens ?? null}
                onChange={(value) => updateGuardrails({ max_prompt_tokens: value ?? null })}
                placeholder="不限制"
                addonAfter="Token"
                style={{ width: 220 }}
              />
            </Form.Item>
          </>
        )}
      </SettingsSection>

      <SettingsSection
        title="故障恢复"
        icon={<ReloadOutlined />}
        state={resilience}
      >
        {resilience.value && (
          <>
            <Form.Item
              label="流式首包恢复"
              extra="仅在上游尚未送出内容时保留最多 750ms 或 64KiB 的首包窗口；已有内容送达后立即放行。"
            >
              <Switch
                checked={resilience.value.stream_recovery_enabled}
                onChange={(value) => updateResilience({ stream_recovery_enabled: value })}
              />
            </Form.Item>
            <Form.Item
              label="首包恢复重试次数"
              extra="仅在上方开关开启时生效，范围 0 到 4，与同目标重试次数相互独立。"
            >
              <InputNumber
                min={0}
                max={4}
                precision={0}
                disabled={resilience.saving || !resilience.value.stream_recovery_enabled}
                value={resilience.value.stream_recovery_max_retries}
                onChange={(value) =>
                  updateResilience({ stream_recovery_max_retries: value ?? 0 })
                }
                addonAfter="次"
                style={{ width: 220 }}
              />
            </Form.Item>
            <Form.Item
              label="同目标重试次数"
              extra="连接错误、超时和可重试 5xx 会先重试当前目标，再切换到下一个目标；0 表示关闭。"
            >
              <InputNumber
                min={0}
                max={5}
                precision={0}
                value={resilience.value.max_retries}
                onChange={(value) => updateResilience({ max_retries: value ?? 0 })}
                addonAfter="次"
                style={{ width: 220 }}
              />
            </Form.Item>
            <Form.Item
              label="首次退避"
              extra="范围 0 到 60000 毫秒，之后每次重试翻倍。"
            >
              <InputNumber
                min={0}
                max={60000}
                precision={0}
                value={resilience.value.retry_backoff_ms}
                onChange={(value) => updateResilience({ retry_backoff_ms: value ?? 0 })}
                addonAfter="ms"
                style={{ width: 220 }}
              />
            </Form.Item>
            <Form.Item
              label="退避上限"
              extra="范围 0 到 60000 毫秒，且不得小于首次退避。"
            >
              <InputNumber
                min={0}
                max={60000}
                precision={0}
                value={resilience.value.retry_max_backoff_ms}
                onChange={(value) =>
                  updateResilience({ retry_max_backoff_ms: value ?? 0 })
                }
                addonAfter="ms"
                style={{ width: 220 }}
              />
            </Form.Item>
          </>
        )}
      </SettingsSection>
    </>
  )
}
