import { ClockCircleOutlined, FileSearchOutlined } from '@ant-design/icons'
import { Form, InputNumber, Switch } from 'antd'
import type { InspectorSettings, RuntimeSettings } from '../../api'
import SettingsSection from './SettingsSection'
import useSettingsSection from './useSettingsSection'

export default function LoggingPrivacyTab() {
  const retention = useSettingsSection<Pick<RuntimeSettings, 'usage_retention_days'>>(
    '/api/settings/runtime',
    '日志保留策略已保存',
    (current) => ({
      usage_retention_days:
        current.usage_retention_days && current.usage_retention_days > 0
          ? current.usage_retention_days
          : null,
    })
  )
  const inspector = useSettingsSection<InspectorSettings>(
    '/api/settings/inspector',
    '请求内容记录策略已保存'
  )

  return (
    <>
      <SettingsSection
        title="日志保留"
        icon={<ClockCircleOutlined />}
        state={retention}
      >
        {retention.value && (
          <Form.Item
            label="保留天数"
            extra="留空或设为 0 表示永久保留。启用后每小时自动清理过期日志，最多保留 3650 天。"
          >
            <InputNumber
              min={0}
              max={3650}
              precision={0}
              value={retention.value.usage_retention_days ?? null}
              onChange={(value) =>
                retention.setValue((current) =>
                  current ? { ...current, usage_retention_days: value ?? null } : current
                )
              }
              placeholder="永久保留"
              addonAfter="天"
              style={{ width: 220 }}
            />
          </Form.Item>
        )}
      </SettingsSection>

      <SettingsSection
        title="请求内容记录"
        icon={<FileSearchOutlined />}
        state={inspector}
      >
        {inspector.value && (
          <>
            <Form.Item
              label="捕获请求内容"
              extra="默认关闭。开启后可在请求详情中查看脱敏并截断后的请求体，便于排查上游问题。"
            >
              <Switch
                checked={inspector.value.capture_request_previews}
                onChange={(value) =>
                  inspector.setValue((current) =>
                    current ? { ...current, capture_request_previews: value } : current
                  )
                }
              />
            </Form.Item>
            <Form.Item label="最大保留字符数" extra="范围 256 到 65536，过长内容会截断。">
              <InputNumber
                min={256}
                max={65536}
                precision={0}
                value={inspector.value.request_preview_max_chars}
                onChange={(value) =>
                  inspector.setValue((current) =>
                    current
                      ? { ...current, request_preview_max_chars: value ?? 4000 }
                      : current
                  )
                }
                addonAfter="字符"
                style={{ width: 220 }}
                disabled={inspector.saving || !inspector.value.capture_request_previews}
              />
            </Form.Item>
          </>
        )}
      </SettingsSection>
    </>
  )
}
