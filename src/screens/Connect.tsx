/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { invoke } from '@tauri-apps/api/core'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardFooter } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
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

/**
 * Connection-profile picker/editor. This is the app's only React view — see
 * `App.tsx`: once `connect()` resolves, the Rust side navigates the main
 * window's webview top-level to the daemon panel, so there is no
 * "connected" state to render here. Disconnecting reloads the webview back
 * to this screen from scratch.
 */
export default function Connect() {
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
      // The `connect` command navigates the main window's webview
      // top-level to the panel once it resolves — there is nothing further
      // to do here on success (this screen is about to be replaced).
      await invoke('connect', { profile, password })
      setPasswordPromptId(null)
      setPasswordValue('')
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
      <header className="text-center">
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
            <li key={p.id}>
              <Card className="gap-3 py-4 transition-[box-shadow,border-color] duration-150 hover:border-brand/30 hover:shadow-sm">
                <CardContent className="flex items-center justify-between gap-3 px-5">
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
                      variant="outline"
                      onClick={() => void deleteProfile(p.id)}
                      className="hover:border-destructive/50 hover:bg-destructive/5 hover:text-destructive"
                    >
                      Удалить
                    </Button>
                    <Button variant="default" onClick={() => connect(p)} disabled={connectingId !== null}>
                      {connectingId === p.id ? <Spinner size={14} /> : null}
                      Подключить
                    </Button>
                  </div>
                </CardContent>

                {passwordPromptId === p.id && (
                  <CardFooter className="px-5">
                    <form
                      className="flex w-full items-center gap-2"
                      onSubmit={(e) => {
                        e.preventDefault()
                        void doConnect(p, passwordValue)
                      }}
                    >
                      <Input
                        type="password"
                        autoFocus
                        value={passwordValue}
                        onChange={(e) => {
                          const v = e.currentTarget.value
                          setPasswordValue(v)
                        }}
                        placeholder="Пароль SSH"
                        className="flex-1"
                      />
                      <Button
                        variant="outline"
                        onClick={() => {
                          setPasswordPromptId(null)
                          setPasswordValue('')
                        }}
                      >
                        Отмена
                      </Button>
                      <Button type="submit" variant="default" disabled={connectingId !== null}>
                        Войти
                      </Button>
                    </form>
                  </CardFooter>
                )}
              </Card>
            </li>
          ))}
        </ul>
      )}

      {formOpen ? (
        <Card className="gap-4 py-5">
          <CardContent className="flex flex-col gap-4 px-5">
            <form
              className="flex flex-col gap-4"
              onSubmit={(e) => {
                e.preventDefault()
                void saveDraft()
              }}
            >
              <div className="flex gap-2">
                {(['local', 'remote'] as const).map((m) => (
                  <Button
                    key={m}
                    type="button"
                    variant={draft.mode === m ? 'default' : 'outline'}
                    className="flex-1"
                    onClick={() =>
                      setDraft((d) => ({
                        ...d,
                        mode: m,
                        host: m === 'local' ? '127.0.0.1' : d.host === '127.0.0.1' ? '' : d.host,
                      }))
                    }
                  >
                    {modeLabels[m]}
                  </Button>
                ))}
              </div>

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="profile-name">Название</Label>
                <Input
                  id="profile-name"
                  value={draft.name}
                  onChange={(e) => {
                    const v = e.currentTarget.value
                    setDraft((d) => ({ ...d, name: v }))
                  }}
                  placeholder="Мой сервер"
                  required
                />
              </div>

              {draft.mode === 'remote' && (
                <>
                  <div className="flex gap-3">
                    <div className="flex flex-1 flex-col gap-1.5">
                      <Label htmlFor="profile-host">Хост</Label>
                      <Input
                        id="profile-host"
                        value={draft.host}
                        onChange={(e) => {
                          const v = e.currentTarget.value
                          setDraft((d) => ({ ...d, host: v }))
                        }}
                        placeholder="example.com"
                        required
                      />
                    </div>
                    <div className="flex w-24 flex-col gap-1.5">
                      <Label htmlFor="profile-port">SSH-порт</Label>
                      <Input
                        id="profile-port"
                        type="number"
                        value={draft.port}
                        onChange={(e) => {
                          const v = Number(e.currentTarget.value)
                          setDraft((d) => ({ ...d, port: v }))
                        }}
                      />
                    </div>
                  </div>

                  <div className="flex flex-col gap-1.5">
                    <Label htmlFor="profile-user">Пользователь</Label>
                    <Input
                      id="profile-user"
                      value={draft.user}
                      onChange={(e) => {
                        const v = e.currentTarget.value
                        setDraft((d) => ({ ...d, user: v }))
                      }}
                      placeholder="admin"
                      required
                    />
                  </div>

                  <div className="flex flex-col gap-1.5">
                    <Label htmlFor="profile-auth">Способ входа</Label>
                    <Select
                      value={draft.auth}
                      onValueChange={(v) => setDraft((d) => ({ ...d, auth: v as AuthMethod }))}
                    >
                      <SelectTrigger id="profile-auth" className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {(Object.keys(authLabels) as AuthMethod[]).map((a) => (
                          <SelectItem key={a} value={a}>
                            {authLabels[a]}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <p className="-mt-2.5 text-xs text-muted-foreground">
                    Ключ по умолчанию берется из ssh-agent/~/.ssh. Пароль — только если ключа нет.
                  </p>

                  {draft.auth === 'keyFile' && (
                    <div className="flex flex-col gap-1.5">
                      <Label htmlFor="profile-key-path">Путь к файлу ключа</Label>
                      <Input
                        id="profile-key-path"
                        value={draft.keyPath ?? ''}
                        onChange={(e) => {
                          const v = e.currentTarget.value
                          setDraft((d) => ({ ...d, keyPath: v || null }))
                        }}
                        placeholder="~/.ssh/id_ed25519"
                      />
                    </div>
                  )}
                </>
              )}

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="profile-panel-port">Порт панели</Label>
                <Input
                  id="profile-panel-port"
                  type="number"
                  value={draft.panelPort}
                  onChange={(e) => {
                    const v = Number(e.currentTarget.value)
                    setDraft((d) => ({ ...d, panelPort: v }))
                  }}
                />
              </div>

              {/* Fluent: form action row lives at the bottom-right, primary
                  action ("Сохранить") rightmost, secondary ("Отмена") to its left. */}
              <div className="flex justify-end gap-2 pt-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    setFormOpen(false)
                    setDraft(emptyDraft())
                  }}
                >
                  Отмена
                </Button>
                <Button type="submit" variant="default">
                  Сохранить
                </Button>
              </div>
            </form>
          </CardContent>
        </Card>
      ) : (
        <Button
          variant="outline"
          onClick={() => setFormOpen(true)}
          className={cn('self-start border-dashed bg-transparent text-muted-foreground hover:bg-transparent hover:text-foreground')}
        >
          + Добавить профиль
        </Button>
      )}
    </main>
  )
}
