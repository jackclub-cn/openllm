import type { ReactNode } from 'react'
import { ReloadOutlined, SaveOutlined } from '@ant-design/icons'
import { Alert, Button, Form, Skeleton, Space, Tag, Typography } from 'antd'
import { useSettingsEdits } from './SettingsEdits'
import type { SettingsSectionState } from './useSettingsSection'

type Props<T> = {
  title: string
  icon: ReactNode
  state: SettingsSectionState<T>
  children: ReactNode
}

export default function SettingsSection<T>({ title, icon, state, children }: Props<T>) {
  useSettingsEdits(state.dirty || state.saving)
  return (
    <section className="settings-section" aria-label={title}>
      <div className="settings-section-heading">
        <Typography.Title level={4}><Space>{icon}{title}</Space></Typography.Title>
        {state.dirty && <Tag color="warning">未保存</Tag>}
      </div>
      {state.error ? (
        <Alert
          type="error"
          showIcon
          title={`${title}加载失败`}
          description={state.error}
          action={
            <Button icon={<ReloadOutlined />} onClick={() => void state.reload()}>
              重试
            </Button>
          }
        />
      ) : state.loading ? (
        <Skeleton active paragraph={{ rows: 4 }} />
      ) : (
        <Form layout="vertical" disabled={state.saving || state.value === undefined}>
          {children}
          <div className="settings-section-actions">
            <Button
              type="primary"
              icon={<SaveOutlined />}
              loading={state.saving}
              disabled={!state.dirty}
              onClick={() => void state.save()}
            >
              保存{title}
            </Button>
            <Button disabled={!state.dirty} onClick={state.reset}>撤销修改</Button>
          </div>
        </Form>
      )}
    </section>
  )
}
