/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { invoke } from '@tauri-apps/api/core'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { cn } from '@/lib/utils'

// Mirrors src-tauri/src/connection.rs (serde rename_all = "camelCase").
// NOTE: no `password` field on `Profile` — passwords are collected
// transiently at connect time (task B4), never persisted.
export type ConnMode = 'local' | 'remote'
export type AuthMethod = 'agent' | 'keyFile' | 'password'

export interface Profile {
  id: string
  name: string
  mode: ConnMode
  host: string
  user: string
  port: number
  panelPort: number
  auth: AuthMethod
  keyPath: string | null
}

const DEFAULT_PANEL_PORT = 42627
const DEFAULT_SSH_PORT = 22

type Draft = Omit<Profile, 'id'>

const emptyDraft = (): Draft => ({
  name: '',
  mode: 'local',
  host: '127.0.0.1',
  user: '',
  port: DEFAULT_SSH_PORT,
  panelPort: DEFAULT_PANEL_PORT,
  auth: 'agent',
  keyPath: null,
})

// Non-jargony labels: these name what the option DOES for the user rather
// than the SSH mechanism behind it. The `AuthMethod` values themselves
// (`agent` / `keyFile` / `password`) are unchanged — only display text.
const authLabels: Record<AuthMethod, string> = {
  agent: 'Ключ (по умолчанию)',
  keyFile: 'Файл ключа…',
  password: 'Пароль',
}

const modeLabels: Record<ConnMode, string> = {
  local: 'Локально',
  remote: 'По сети',
}

// Fluent text-field feel, shared by every input/select in the form: clear
// label above (in the markup, not here), a 1px border, and an
// accent-colored ring on focus instead of the browser default outline.
const fieldClass = cn(
  'rounded-[4px] border border-input bg-background px-3 py-2 text-sm text-foreground',
  'transition-colors duration-100',
  'placeholder:text-muted-foreground',
  'focus:border-brand focus:outline-none focus:ring-2 focus:ring-brand/25',
)

interface ConnectProps {
  /** Called with the panel base URL once `connect()` resolves. */
  onConnected: (baseUrl: string) => void
}

export default function Connect({ onConnected }: ConnectProps) {
  const [profiles, setProfiles] = useState<Profile[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [connectingId, setConnectingId] = useState<string | null>(null)
  const [formOpen, setFormOpen] = useState(false)
  const [draft, setDraft] = useState<Draft>(emptyDraft())
  // Password-auth profiles need a transient prompt at connect time — the
  // password itself is never persisted (see `Profile` above).
  const [passwordPromptId, setPasswordPromptId] = useState<string | null>(null)
  const [passwordValue, setPasswordValue] = useState('')

  const refresh = async () => {
    try {
      const list = await invoke<Profile[]>('list_profiles')
      setProfiles(list)
      setError(null)
    } catch (e) {
      setError(`Не удалось загрузить профили: ${String(e)}`)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void refresh()
  }, [])

  const saveDraft = async () => {
    if (!draft.name.trim()) {
      setError('Введите название профиля')
      return
    }
    const profile: Profile = { id: crypto.randomUUID(), ...draft }
    try {
      await invoke('save_profile', { profile })
      setFormOpen(false)
      setDraft(emptyDraft())
      await refresh()
    } catch (e) {
      setError(`Не удалось сохранить профиль: ${String(e)}`)
    }
  }

  const deleteProfile = async (id: string) => {
    try {
      await invoke('delete_profile', { id })
      await refresh()
    } catch (e) {
      setError(`Не удалось удалить профиль: ${String(e)}`)
    }
  }

  const doConnect = async (profile: Profile, password?: string) => {
    setConnectingId(profile.id)
    setError(null)
    try {
      const baseUrl = await invoke<string>('connect', { profile, password })
      setPasswordPromptId(null)
      setPasswordValue('')
      onConnected(baseUrl)
    } catch (e) {
      setError(`Не удалось подключиться: ${String(e)}`)
    } finally {
      setConnectingId(null)
    }
  }

  const connect = (profile: Profile) => {
    if (profile.mode === 'remote' && profile.auth === 'password' && passwordPromptId !== profile.id) {
      setError(null)
      setPasswordPromptId(profile.id)
      setPasswordValue('')
      return
    }
    void doConnect(profile, passwordPromptId === profile.id ? passwordValue : undefined)
  }

  return (
    <main className="mx-auto flex min-h-screen max-w-xl flex-col gap-6 px-6 py-10">
      <header>
        <h1 className="font-heading text-2xl text-foreground">Подключение</h1>
        <p className="text-sm text-muted-foreground">Выберите сохраненный профиль или добавьте новый.</p>
      </header>

      {error && (
        <div className="rounded-[var(--radius)] border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive shadow-xs">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex items-center gap-2 text-muted-foreground">
          <Spinner size={18} />
          <span>Загрузка профилей…</span>
        </div>
      ) : (
        <ul className="flex flex-col gap-3">
          {profiles.length === 0 && (
            <li className="rounded-[var(--radius)] border border-dashed border-border px-4 py-6 text-center text-sm text-muted-foreground">
              Сохраненных профилей нет.
            </li>
          )}
          {profiles.map((p) => (
            <li
              key={p.id}
              className={cn(
                'flex flex-col gap-3 rounded-[var(--radius)] border border-border bg-card px-5 py-4 shadow-xs',
                'transition-[box-shadow,border-color] duration-150 hover:border-brand/30 hover:shadow-sm',
              )}
            >
              <div className="flex items-center justify-between gap-3">
                <div className="flex flex-col">
                  <span className="font-medium text-card-foreground">{p.name}</span>
                  <span className="text-xs text-muted-foreground">
                    {modeLabels[p.mode]}
                    {p.mode === 'remote' ? ` · ${p.user}@${p.host}:${p.port}` : ` · порт панели ${p.panelPort}`}
                  </span>
                </div>
                {/* Fluent: secondary/dismissive action on the left, primary
                    (positive) action rightmost. */}
                <div className="flex items-center gap-2">
                  <Button
                    variant="secondary"
                    onClick={() => void deleteProfile(p.id)}
                    className="hover:border-destructive/50 hover:bg-destructive/5 hover:text-destructive"
                  >
                    Удалить
                  </Button>
                  <Button variant="primary" onClick={() => connect(p)} disabled={connectingId !== null}>
                    {connectingId === p.id ? <Spinner size={14} /> : null}
                    Подключить
                  </Button>
                </div>
              </div>

              {passwordPromptId === p.id && (
                <form
                  className="flex items-center gap-2"
                  onSubmit={(e) => {
                    e.preventDefault()
                    void doConnect(p, passwordValue)
                  }}
                >
                  <input
                    type="password"
                    autoFocus
                    value={passwordValue}
                    onChange={(e) => {
                      const v = e.currentTarget.value
                      setPasswordValue(v)
                    }}
                    placeholder="Пароль SSH"
                    className={cn(fieldClass, 'flex-1')}
                  />
                  <Button
                    variant="secondary"
                    onClick={() => {
                      setPasswordPromptId(null)
                      setPasswordValue('')
                    }}
                  >
                    Отмена
                  </Button>
                  <Button type="submit" variant="primary" disabled={connectingId !== null}>
                    Войти
                  </Button>
                </form>
              )}
            </li>
          ))}
        </ul>
      )}

      {formOpen ? (
        <form
          className="flex flex-col gap-4 rounded-[var(--radius)] border border-border bg-card p-5 shadow-xs"
          onSubmit={(e) => {
            e.preventDefault()
            void saveDraft()
          }}
        >
          <div className="flex gap-2">
            {(['local', 'remote'] as const).map((m) => (
              <button
                key={m}
                type="button"
                onClick={() =>
                  setDraft((d) => ({
                    ...d,
                    mode: m,
                    host: m === 'local' ? '127.0.0.1' : d.host === '127.0.0.1' ? '' : d.host,
                  }))
                }
                className={cn(
                  'flex-1 rounded-[4px] border px-3 py-2 text-sm transition-colors duration-100',
                  'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand',
                  draft.mode === m
                    ? 'border-brand bg-brand text-brand-foreground'
                    : 'border-border bg-transparent text-muted-foreground hover:border-brand/40 hover:text-foreground',
                )}
              >
                {modeLabels[m]}
              </button>
            ))}
          </div>

          <label className="flex flex-col gap-1.5 text-sm text-foreground">
            Название
            <input
              className={fieldClass}
              value={draft.name}
              onChange={(e) => {
                const v = e.currentTarget.value
                setDraft((d) => ({ ...d, name: v }))
              }}
              placeholder="Мой сервер"
              required
            />
          </label>

          {draft.mode === 'remote' && (
            <>
              <div className="flex gap-3">
                <label className="flex flex-1 flex-col gap-1.5 text-sm text-foreground">
                  Хост
                  <input
                    className={fieldClass}
                    value={draft.host}
                    onChange={(e) => {
                      const v = e.currentTarget.value
                      setDraft((d) => ({ ...d, host: v }))
                    }}
                    placeholder="example.com"
                    required
                  />
                </label>
                <label className="flex w-24 flex-col gap-1.5 text-sm text-foreground">
                  SSH-порт
                  <input
                    type="number"
                    className={fieldClass}
                    value={draft.port}
                    onChange={(e) => {
                      const v = Number(e.currentTarget.value)
                      setDraft((d) => ({ ...d, port: v }))
                    }}
                  />
                </label>
              </div>

              <label className="flex flex-col gap-1.5 text-sm text-foreground">
                Пользователь
                <input
                  className={fieldClass}
                  value={draft.user}
                  onChange={(e) => {
                    const v = e.currentTarget.value
                    setDraft((d) => ({ ...d, user: v }))
                  }}
                  placeholder="admin"
                  required
                />
              </label>

              <label className="flex flex-col gap-1.5 text-sm text-foreground">
                Способ входа
                <select
                  className={fieldClass}
                  value={draft.auth}
                  onChange={(e) => {
                    const v = e.currentTarget.value as AuthMethod
                    setDraft((d) => ({ ...d, auth: v }))
                  }}
                >
                  {(Object.keys(authLabels) as AuthMethod[]).map((a) => (
                    <option key={a} value={a}>
                      {authLabels[a]}
                    </option>
                  ))}
                </select>
              </label>
              <p className="-mt-2.5 text-xs text-muted-foreground">
                Ключ по умолчанию берется из ssh-agent/~/.ssh. Пароль — только если ключа нет.
              </p>

              {draft.auth === 'keyFile' && (
                <label className="flex flex-col gap-1.5 text-sm text-foreground">
                  Путь к файлу ключа
                  <input
                    className={fieldClass}
                    value={draft.keyPath ?? ''}
                    onChange={(e) => {
                      const v = e.currentTarget.value
                      setDraft((d) => ({ ...d, keyPath: v || null }))
                    }}
                    placeholder="~/.ssh/id_ed25519"
                  />
                </label>
              )}
            </>
          )}

          <label className="flex flex-col gap-1.5 text-sm text-foreground">
            Порт панели
            <input
              type="number"
              className={fieldClass}
              value={draft.panelPort}
              onChange={(e) => {
                const v = Number(e.currentTarget.value)
                setDraft((d) => ({ ...d, panelPort: v }))
              }}
            />
          </label>

          {/* Fluent: form action row lives at the bottom-right, primary
              action ("Сохранить") rightmost, secondary ("Отмена") to its left. */}
          <div className="flex justify-end gap-2 pt-2">
            <Button
              variant="secondary"
              onClick={() => {
                setFormOpen(false)
                setDraft(emptyDraft())
              }}
            >
              Отмена
            </Button>
            <Button type="submit" variant="primary">
              Сохранить
            </Button>
          </div>
        </form>
      ) : (
        <Button
          variant="secondary"
          onClick={() => setFormOpen(true)}
          className="self-start border-dashed bg-transparent text-muted-foreground hover:bg-transparent hover:text-foreground"
        >
          + Добавить профиль
        </Button>
      )}
    </main>
  )
}
