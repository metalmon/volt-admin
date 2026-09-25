/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { invoke } from '@tauri-apps/api/core'
import { useEffect, useState } from 'react'
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

const authLabels: Record<AuthMethod, string> = {
  agent: 'SSH-агент',
  keyFile: 'Файл ключа',
  password: 'Пароль',
}

const modeLabels: Record<ConnMode, string> = {
  local: 'Локально',
  remote: 'По сети',
}

export default function Connect() {
  const [profiles, setProfiles] = useState<Profile[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [connectingId, setConnectingId] = useState<string | null>(null)
  const [formOpen, setFormOpen] = useState(false)
  const [draft, setDraft] = useState<Draft>(emptyDraft())

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

  const connect = async (profile: Profile) => {
    setConnectingId(profile.id)
    setError(null)
    try {
      // Actual tunnel/attach flow lands in task B4 (`connect_profile`, plus
      // transient password prompt for auth: "password"). For now this only
      // exercises the connecting state and the store round-trip.
      await invoke('connect_profile', { profile })
    } catch (e) {
      setError(`Подключение пока не реализовано: ${String(e)}`)
    } finally {
      setConnectingId(null)
    }
  }

  return (
    <main className="mx-auto flex min-h-screen max-w-xl flex-col gap-6 px-6 py-10">
      <header>
        <h1 className="font-heading text-2xl text-foreground">Подключение</h1>
        <p className="text-sm text-muted-foreground">Выберите сохраненный профиль или добавьте новый.</p>
      </header>

      {error && (
        <div className="rounded-[var(--radius)] border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">
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
              className="flex items-center justify-between gap-3 rounded-[var(--radius)] border border-border bg-card px-4 py-3 shadow-xs"
            >
              <div className="flex flex-col">
                <span className="font-medium text-card-foreground">{p.name}</span>
                <span className="text-xs text-muted-foreground">
                  {modeLabels[p.mode]}
                  {p.mode === 'remote' ? ` · ${p.user}@${p.host}:${p.port}` : ` · порт панели ${p.panelPort}`}
                </span>
              </div>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => void connect(p)}
                  disabled={connectingId !== null}
                  className={cn(
                    'inline-flex items-center gap-2 rounded-[var(--radius)] bg-brand px-3 py-1.5 text-sm font-medium text-brand-foreground',
                    'disabled:opacity-50',
                  )}
                >
                  {connectingId === p.id ? <Spinner size={14} /> : null}
                  Подключить
                </button>
                <button
                  type="button"
                  onClick={() => void deleteProfile(p.id)}
                  className="rounded-[var(--radius)] border border-border px-3 py-1.5 text-sm text-muted-foreground hover:text-destructive"
                >
                  Удалить
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {formOpen ? (
        <form
          className="flex flex-col gap-3 rounded-[var(--radius)] border border-border bg-card p-4"
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
                  'flex-1 rounded-[var(--radius)] border px-3 py-1.5 text-sm',
                  draft.mode === m
                    ? 'border-brand bg-brand text-brand-foreground'
                    : 'border-border bg-transparent text-muted-foreground',
                )}
              >
                {modeLabels[m]}
              </button>
            ))}
          </div>

          <label className="flex flex-col gap-1 text-sm text-foreground">
            Название
            <input
              className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
              value={draft.name}
              onChange={(e) => setDraft((d) => ({ ...d, name: e.currentTarget.value }))}
              placeholder="Мой сервер"
              required
            />
          </label>

          {draft.mode === 'remote' && (
            <>
              <div className="flex gap-3">
                <label className="flex flex-1 flex-col gap-1 text-sm text-foreground">
                  Хост
                  <input
                    className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
                    value={draft.host}
                    onChange={(e) => setDraft((d) => ({ ...d, host: e.currentTarget.value }))}
                    placeholder="example.com"
                    required
                  />
                </label>
                <label className="flex w-24 flex-col gap-1 text-sm text-foreground">
                  SSH-порт
                  <input
                    type="number"
                    className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
                    value={draft.port}
                    onChange={(e) => setDraft((d) => ({ ...d, port: Number(e.currentTarget.value) }))}
                  />
                </label>
              </div>

              <label className="flex flex-col gap-1 text-sm text-foreground">
                Пользователь
                <input
                  className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
                  value={draft.user}
                  onChange={(e) => setDraft((d) => ({ ...d, user: e.currentTarget.value }))}
                  placeholder="admin"
                  required
                />
              </label>

              <label className="flex flex-col gap-1 text-sm text-foreground">
                Способ входа
                <select
                  className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
                  value={draft.auth}
                  onChange={(e) => setDraft((d) => ({ ...d, auth: e.currentTarget.value as AuthMethod }))}
                >
                  {(Object.keys(authLabels) as AuthMethod[]).map((a) => (
                    <option key={a} value={a}>
                      {authLabels[a]}
                    </option>
                  ))}
                </select>
              </label>

              {draft.auth === 'keyFile' && (
                <label className="flex flex-col gap-1 text-sm text-foreground">
                  Путь к файлу ключа
                  <input
                    className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
                    value={draft.keyPath ?? ''}
                    onChange={(e) => setDraft((d) => ({ ...d, keyPath: e.currentTarget.value || null }))}
                    placeholder="~/.ssh/id_ed25519"
                  />
                </label>
              )}
            </>
          )}

          <label className="flex flex-col gap-1 text-sm text-foreground">
            Порт панели
            <input
              type="number"
              className="rounded-[var(--radius)] border border-input bg-background px-3 py-1.5 text-sm"
              value={draft.panelPort}
              onChange={(e) => setDraft((d) => ({ ...d, panelPort: Number(e.currentTarget.value) }))}
            />
          </label>

          <div className="flex justify-end gap-2 pt-2">
            <button
              type="button"
              onClick={() => {
                setFormOpen(false)
                setDraft(emptyDraft())
              }}
              className="rounded-[var(--radius)] border border-border px-3 py-1.5 text-sm text-muted-foreground"
            >
              Отмена
            </button>
            <button
              type="submit"
              className="rounded-[var(--radius)] bg-brand px-3 py-1.5 text-sm font-medium text-brand-foreground"
            >
              Сохранить
            </button>
          </div>
        </form>
      ) : (
        <button
          type="button"
          onClick={() => setFormOpen(true)}
          className="self-start rounded-[var(--radius)] border border-dashed border-border px-4 py-2 text-sm text-muted-foreground hover:text-foreground"
        >
          + Добавить профиль
        </button>
      )}
    </main>
  )
}
