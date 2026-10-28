/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { invoke } from '@tauri-apps/api/core'
import { useEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardFooter } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { cn } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'

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
  localPort: number | null
  auth: AuthMethod
  keyPath: string | null
  principal: string | null
  paircodeCommand: string | null
}

const DEFAULT_PANEL_PORT = 42617
const DEFAULT_SSH_PORT = 22

type Draft = Omit<Profile, 'id'>

const emptyDraft = (): Draft => ({
  name: '',
  mode: 'local',
  host: '127.0.0.1',
  user: '',
  port: DEFAULT_SSH_PORT,
  panelPort: DEFAULT_PANEL_PORT,
  localPort: null,
  auth: 'agent',
  keyPath: null,
  principal: null,
  paircodeCommand: null,
})

// i18n keys for the enum display labels. The `AuthMethod` / `ConnMode` values
// themselves are unchanged — only the localized display text differs.
const authLabelKey: Record<AuthMethod, string> = {
  agent: 'auth.agent',
  keyFile: 'auth.keyFile',
  password: 'auth.password',
}

const modeLabelKey: Record<ConnMode, string> = {
  local: 'mode.local',
  remote: 'mode.remote',
}

// `parse_profile_doc` and `save_profile` report a bad field as `field:<name>`
// (see `commands.rs`); these are the form labels to name it by.
const importFieldLabelKey: Record<string, string> = {
  name: 'field.name',
  host: 'field.host',
  user: 'field.user',
  port: 'field.sshPort',
  panelPort: 'field.panelPort',
  localPort: 'field.localPort',
  keyPath: 'field.keyPath',
  principal: 'field.principal',
  paircodeCommand: 'field.paircodeCommand',
}

const importButtonClass = cn(
  'border-dashed bg-transparent text-muted-foreground hover:bg-transparent hover:text-foreground',
)

/** The one-line "what is this profile" summary, shared by the profile cards
 *  and the import preview — the preview must read exactly like the card the
 *  import will produce. */
function summaryOf(p: Profile, t: (key: string, vars?: Record<string, string | number>) => string) {
  const tail =
    p.mode === 'remote'
      ? `${p.user}@${p.host}:${p.port} · ${t('card.panelPort', { port: p.panelPort })}`
      : t('card.panelPort', { port: p.panelPort })
  return `${t(modeLabelKey[p.mode])} · ${tail}`
}

/** Compact RU/EN switcher; the choice is persisted (see `lib/i18n`). */
function LanguageToggle() {
  const { lang, setLang } = useI18n()
  return (
    <div className="inline-flex items-center gap-0.5 rounded-[var(--radius)] border border-border p-0.5">
      {(['ru', 'en'] as const).map((l) => (
        <button
          key={l}
          type="button"
          onClick={() => setLang(l)}
          aria-pressed={lang === l}
          className={cn(
            'rounded-[calc(var(--radius)-0.2rem)] px-2 py-0.5 text-xs font-medium uppercase transition-colors',
            lang === l
              ? 'bg-primary text-primary-foreground'
              : 'text-muted-foreground hover:text-foreground',
          )}
        >
          {l}
        </button>
      ))}
    </div>
  )
}

/**
 * Connection-profile picker/editor. This is the app's only React view — see
 * `App.tsx`: once `connect()` resolves, the Rust side navigates the main
 * window's webview top-level to the daemon panel, so there is no
 * "connected" state to render here. Disconnecting reloads the webview back
 * to this screen from scratch.
 */
export default function Connect() {
  const { t, lang } = useI18n()
  const [profiles, setProfiles] = useState<Profile[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [connectingId, setConnectingId] = useState<string | null>(null)
  const [formOpen, setFormOpen] = useState(false)
  const [draft, setDraft] = useState<Draft>(emptyDraft())
  // null → the form creates a new profile; a profile id → it edits that
  // profile in place (save_profile upserts by id on the Rust side).
  const [editingId, setEditingId] = useState<string | null>(null)
  // Password-auth profiles need a transient prompt at connect time — the
  // password itself is never persisted (see `Profile` above).
  const [passwordPromptId, setPasswordPromptId] = useState<string | null>(null)
  const [passwordValue, setPasswordValue] = useState('')
  // A parsed-but-not-yet-saved imported profile, awaiting confirmation. An
  // imported profile carries a command that will be run on the target (see
  // `profile_doc.rs`), so nothing reaches the store before the user sees it.
  const [pending, setPending] = useState<Profile | null>(null)
  const [copiedId, setCopiedId] = useState<string | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  const refresh = async () => {
    try {
      const list = await invoke<Profile[]>('list_profiles')
      setProfiles(list)
      setError(null)
    } catch (e) {
      setError(t('err.load', { e: String(e) }))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  /** Turn a `parse_profile_doc` / `save_profile` rejection into a message. */
  const importError = (raw: string) => {
    const reason = raw.replace(/^Error:\s*/, '')
    if (reason.startsWith('field:')) {
      const field = reason.slice('field:'.length)
      const key = importFieldLabelKey[field]
      return t('err.import.field', { field: key ? t(key) : field })
    }
    if (reason === 'array') return t('err.import.array')
    return t('err.import.json')
  }

  /** One pipeline for both import sources: parse, then show the preview. */
  const importText = async (text: string) => {
    try {
      const parsed = await invoke<Profile>('parse_profile_doc', { text })
      setError(null)
      setPending(parsed)
    } catch (e) {
      setPending(null)
      setError(importError(String(e)))
    }
  }

  const importFromClipboard = async () => {
    try {
      const text = await navigator.clipboard.readText()
      await importText(text)
    } catch {
      // WebView2 can refuse the read (no user gesture, denied permission);
      // the file route always works.
      setPending(null)
      setError(t('err.import.clipboard'))
    }
  }

  const confirmImport = async () => {
    if (!pending) return
    try {
      // A document may carry no id — mint one, exactly as the form does.
      await invoke('save_profile', {
        profile: { ...pending, id: pending.id || crypto.randomUUID() },
      })
      setPending(null)
      await refresh()
    } catch (e) {
      setError(importError(String(e)))
    }
  }

  const exportProfile = async (p: Profile) => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(p, null, 2))
      setError(null)
      setCopiedId(p.id)
      setTimeout(() => setCopiedId((c) => (c === p.id ? null : c)), 3000)
    } catch {
      setError(t('err.export.clipboard'))
    }
  }

  const closeForm = () => {
    setFormOpen(false)
    setDraft(emptyDraft())
    setEditingId(null)
  }

  const startCreate = () => {
    setDraft(emptyDraft())
    setEditingId(null)
    setFormOpen(true)
  }

  const startEdit = (p: Profile) => {
    const { id: _id, ...rest } = p
    setDraft(rest)
    setEditingId(p.id)
    setFormOpen(true)
  }

  const saveDraft = async () => {
    if (!draft.name.trim()) {
      setError(t('err.nameRequired'))
      return
    }
    const profile: Profile = { id: editingId ?? crypto.randomUUID(), ...draft }
    try {
      await invoke('save_profile', { profile })
      closeForm()
      await refresh()
    } catch (e) {
      setError(t('err.save', { e: String(e) }))
    }
  }

  const deleteProfile = async (id: string) => {
    try {
      await invoke('delete_profile', { id })
      await refresh()
    } catch (e) {
      setError(t('err.delete', { e: String(e) }))
    }
  }

  const doConnect = async (profile: Profile, password?: string) => {
    setConnectingId(profile.id)
    setError(null)
    try {
      // The `connect` command navigates the main window's webview
      // top-level to the panel once it resolves — there is nothing further
      // to do here on success (this screen is about to be replaced). Pairing
      // happens lazily inside the panel afterwards (see src-tauri/src/pair.rs).
      // Guard against a command that never settles so the connect button can
      // never spin forever: after 90s the promise is rejected and the error is
      // shown (the underlying command, if it later resolves, navigates the
      // window on its own — see commands::connect).
      await Promise.race([
        invoke<{ baseUrl: string }>('connect', {
          profile,
          password,
          lang,
        }),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error(t('err.connectTimeout'))), 90_000),
        ),
      ])
      setPasswordPromptId(null)
      setPasswordValue('')
    } catch (e) {
      setError(t('err.connect', { e: String(e) }))
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
    <main className="mx-auto flex min-h-screen max-w-3xl flex-col gap-6 px-6 py-10">
      <header className="relative text-center">
        <div className="absolute right-0 top-0">
          <LanguageToggle />
        </div>
        <h1 className="font-heading text-2xl text-foreground">{t('app.title')}</h1>
        <p className="text-sm text-muted-foreground">{t('app.subtitle')}</p>
      </header>

      {error && (
        <div className="rounded-[var(--radius)] border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive shadow-xs">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex items-center gap-2 text-muted-foreground">
          <Spinner size={18} />
          <span>{t('list.loading')}</span>
        </div>
      ) : (
        <ul className="flex flex-col gap-3">
          {profiles.length === 0 && (
            <li className="rounded-[var(--radius)] border border-dashed border-border px-4 py-6 text-center text-sm text-muted-foreground">
              {t('list.empty')}
            </li>
          )}
          {profiles.map((p) => (
            <li key={p.id}>
              <Card className="gap-3 py-4">
                <CardContent className="flex items-center justify-between gap-3 px-5">
                  <div className="flex min-w-0 flex-col">
                    <span className="truncate font-medium text-card-foreground">{p.name}</span>
                    <span className="whitespace-nowrap text-xs text-muted-foreground">
                      {summaryOf(p, t)}
                    </span>
                  </div>
                  {/* Fluent: secondary/dismissive action on the left, primary
                      (positive) action rightmost. */}
                  <div className="flex shrink-0 items-center gap-2">
                    <Button variant="outline" onClick={() => void exportProfile(p)}>
                      {copiedId === p.id ? t('export.copied') : t('btn.export')}
                    </Button>
                    <Button variant="outline" onClick={() => startEdit(p)} disabled={connectingId !== null}>
                      {t('btn.edit')}
                    </Button>
                    <Button variant="outline" onClick={() => void deleteProfile(p.id)}>
                      {t('btn.delete')}
                    </Button>
                    <Button variant="default" onClick={() => connect(p)} disabled={connectingId !== null}>
                      {connectingId === p.id ? <Spinner size={14} /> : null}
                      {t('btn.connect')}
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
                        placeholder={t('pwd.placeholder')}
                        className="flex-1"
                      />
                      <Button
                        variant="outline"
                        onClick={() => {
                          setPasswordPromptId(null)
                          setPasswordValue('')
                        }}
                      >
                        {t('btn.cancel')}
                      </Button>
                      <Button type="submit" variant="default" disabled={connectingId !== null}>
                        {t('btn.login')}
                      </Button>
                    </form>
                  </CardFooter>
                )}
              </Card>
            </li>
          ))}
        </ul>
      )}

      {pending && (
        <Card className="gap-3 border-primary/40 py-5">
          <CardContent className="flex flex-col gap-3 px-5">
            <h2 className="font-heading text-lg text-foreground">{t('import.title')}</h2>
            <div className="flex flex-col">
              <span className="truncate font-medium text-card-foreground">{pending.name}</span>
              <span className="text-xs text-muted-foreground">{summaryOf(pending, t)}</span>
            </div>
            {profiles.some((p) => p.id === pending.id) && (
              <p className="text-xs text-muted-foreground">
                {t('import.replaces', {
                  name: profiles.find((p) => p.id === pending.id)?.name ?? pending.id,
                })}
              </p>
            )}
            {/* Everything an import decides on the user's behalf, named:
                which key file is read, how it authenticates, whose pairing
                code is minted, which loopback port is bound. */}
            <dl className="flex flex-col gap-0.5 text-xs text-muted-foreground">
              {([
                ['field.auth', t(authLabelKey[pending.auth])],
                ['field.keyPath', pending.keyPath],
                ['field.principal', pending.principal],
                ['field.localPort', pending.localPort?.toString()],
              ] as const)
                .filter(([, value]) => value)
                .map(([key, value]) => (
                  <div key={key} className="flex gap-2">
                    <dt className="shrink-0">{t(key)}:</dt>
                    <dd className="break-all text-foreground">{value}</dd>
                  </div>
                ))}
            </dl>
            {/* The pairing-code command is shown verbatim, and `validate`
                refuses the characters that would let it render as less than
                it runs (newlines, bidi overrides, zero-width). `pre-wrap`
                keeps the rest of the whitespace honest. */}
            {pending.paircodeCommand && (
              <div className="flex flex-col gap-1 rounded-[var(--radius)] border border-destructive/40 bg-destructive/10 px-3 py-2">
                <span className="text-xs text-destructive">
                  {t(
                    pending.mode === 'local'
                      ? 'import.paircodeWarning.local'
                      : 'import.paircodeWarning',
                  )}
                </span>
                <code className="whitespace-pre-wrap break-all text-xs text-foreground">
                  {pending.paircodeCommand}
                </code>
              </div>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <Button variant="outline" onClick={() => setPending(null)}>
                {t('btn.cancel')}
              </Button>
              <Button variant="default" onClick={() => void confirmImport()}>
                {t('import.confirm')}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {formOpen ? (
        <Card className="gap-4 py-5">
          <CardContent className="flex flex-col gap-4 px-5">
            <h2 className="font-heading text-lg text-foreground">
              {editingId ? t('form.editTitle') : t('form.newTitle')}
            </h2>
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
                        // Local has no tunnel: drop a port typed before the switch.
                        localPort: m === 'local' ? null : d.localPort,
                      }))
                    }
                  >
                    {t(modeLabelKey[m])}
                  </Button>
                ))}
              </div>

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="profile-name">{t('field.name')}</Label>
                <Input
                  id="profile-name"
                  value={draft.name}
                  onChange={(e) => {
                    const v = e.currentTarget.value
                    setDraft((d) => ({ ...d, name: v }))
                  }}
                  placeholder={t('field.name.ph')}
                  required
                />
              </div>

              {draft.mode === 'remote' && (
                <>
                  <div className="flex gap-3">
                    <div className="flex flex-1 flex-col gap-1.5">
                      <Label htmlFor="profile-host">{t('field.host')}</Label>
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
                      <Label htmlFor="profile-port">{t('field.sshPort')}</Label>
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
                    <Label htmlFor="profile-user">{t('field.user')}</Label>
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
                    <Label htmlFor="profile-auth">{t('field.auth')}</Label>
                    <Select
                      value={draft.auth}
                      onValueChange={(v) => setDraft((d) => ({ ...d, auth: v as AuthMethod }))}
                    >
                      <SelectTrigger id="profile-auth" className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {(Object.keys(authLabelKey) as AuthMethod[]).map((a) => (
                          <SelectItem key={a} value={a}>
                            {t(authLabelKey[a])}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <p className="-mt-2.5 text-xs text-muted-foreground">{t('auth.hint')}</p>

                  {draft.auth === 'keyFile' && (
                    <div className="flex flex-col gap-1.5">
                      <Label htmlFor="profile-key-path">{t('field.keyPath')}</Label>
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
                <Label htmlFor="profile-panel-port">{t('field.panelPort')}</Label>
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

              {/* Only `Remote` opens a tunnel — `Local` talks to the panel
                  directly (see `commands::connect`), so there is no local
                  forwarded port to pick there. */}
              {draft.mode === 'remote' && (
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="profile-local-port">{t('field.localPort')}</Label>
                  <Input
                    id="profile-local-port"
                    type="number"
                    placeholder={t('field.localPort.ph')}
                    value={draft.localPort ?? ''}
                    onChange={(e) => {
                      const v = e.currentTarget.value
                      const n = Number(v)
                      setDraft((d) => ({ ...d, localPort: v === '' ? null : Number.isNaN(n) ? null : n }))
                    }}
                  />
                  <p className="text-xs text-muted-foreground">{t('field.localPort.hint')}</p>
                </div>
              )}

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="profile-principal">{t('field.principal')}</Label>
                <Input
                  id="profile-principal"
                  placeholder="admin"
                  value={draft.principal ?? ''}
                  onChange={(e) => {
                    const v = e.currentTarget.value
                    setDraft((d) => ({ ...d, principal: v || null }))
                  }}
                />
                <p className="text-xs text-muted-foreground">{t('field.principal.hint')}</p>
              </div>

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="profile-paircode-command">{t('field.paircodeCommand')}</Label>
                <Input
                  id="profile-paircode-command"
                  placeholder={t('field.paircodeCommand.ph')}
                  value={draft.paircodeCommand ?? ''}
                  onChange={(e) => {
                    const v = e.currentTarget.value
                    setDraft((d) => ({ ...d, paircodeCommand: v || null }))
                  }}
                />
                <p className="text-xs text-muted-foreground">{t('field.paircodeCommand.hint')}</p>
              </div>

              {/* Fluent: form action row lives at the bottom-right, primary
                  action ("Save") rightmost, secondary ("Cancel") to its left. */}
              <div className="flex justify-end gap-2 pt-2">
                <Button type="button" variant="outline" onClick={closeForm}>
                  {t('btn.cancel')}
                </Button>
                <Button type="submit" variant="default">
                  {t('btn.save')}
                </Button>
              </div>
            </form>
          </CardContent>
        </Card>
      ) : (
        <div className="flex flex-wrap justify-end gap-2">
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            className="hidden"
            onChange={(e) => {
              const file = e.currentTarget.files?.[0]
              // Reset first: picking the same file twice must fire again.
              e.currentTarget.value = ''
              if (file) void file.text().then(importText)
            }}
          />
          <Button
            variant="outline"
            onClick={() => fileRef.current?.click()}
            className={importButtonClass}
          >
            {t('btn.importFile')}
          </Button>
          <Button
            variant="outline"
            onClick={() => void importFromClipboard()}
            className={importButtonClass}
          >
            {t('btn.importClipboard')}
          </Button>
          <Button variant="outline" onClick={startCreate} className={importButtonClass}>
            {t('btn.addProfile')}
          </Button>
        </div>
      )}
    </main>
  )
}
