import { createContext, useContext, useEffect, useId, useState } from 'react'
import type { ReactNode } from 'react'
import { App } from 'antd'
import { useBlocker, useBeforeUnload } from 'react-router-dom'

const SettingsEditsContext = createContext<{
  pending: boolean
  register: (id: string, pending: boolean) => void
} | null>(null)

export function SettingsEditsProvider({ children }: { children: ReactNode }) {
  const [sections, setSections] = useState<Record<string, boolean>>({})
  const [register] = useState(() => (id: string, pending: boolean) => {
    setSections((current) =>
      current[id] === pending ? current : { ...current, [id]: pending }
    )
  })
  return (
    <SettingsEditsContext.Provider
      value={{ pending: Object.values(sections).some(Boolean), register }}
    >
      {children}
    </SettingsEditsContext.Provider>
  )
}

export function useSettingsEdits(pending: boolean) {
  const context = useContext(SettingsEditsContext)
  const register = context?.register
  const id = useId()
  useEffect(() => {
    register?.(id, pending)
    return () => register?.(id, false)
  }, [id, pending, register])
}

export function SettingsNavigationGuard() {
  const context = useContext(SettingsEditsContext)
  const { modal } = App.useApp()
  const blocker = useBlocker(context?.pending ?? false)

  useBeforeUnload((event) => {
    if (!context?.pending) return
    event.preventDefault()
    event.returnValue = ''
  })

  useEffect(() => {
    if (blocker.state !== 'blocked') return
    const dialog = modal.confirm({
      title: '离开当前设置？',
      content: '当前有未保存的修改或尚未完成的保存。离开将丢弃未保存的修改。',
      okText: '放弃修改并离开',
      cancelText: '继续编辑',
      onOk: () => blocker.proceed(),
      onCancel: () => blocker.reset(),
    })
    return () => dialog.destroy()
  }, [blocker, modal])

  return null
}
