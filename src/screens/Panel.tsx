/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { invoke } from '@tauri-apps/api/core'
import { useState } from 'react'
import { Spinner } from '@/components/ui/spinner'

interface PanelProps {
  /** Base URL returned by `connect()` — a loopback origin (direct panel port
   * in Local mode, or B4's forwarded tunnel port in Remote mode). */
  baseUrl: string
  /** Called once the tunnel/connection has been torn down; the parent
   * switches back to `Connect`. */
  onDisconnect: () => void
}

/**
 * Hosts the daemon-served admin panel in an iframe pointed at the
 * already-connected loopback/tunnel origin. The panel, its REST API, and its
 * WebSocket (ACP/live) are all same-origin from the iframe's perspective, so
 * no extra proxying is needed here — `connect()` already did the tunneling.
 */
export default function Panel({ baseUrl, onDisconnect }: PanelProps) {
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const panelUrl = `${baseUrl.replace(/\/+$/, '')}/_app/`

  const disconnect = async () => {
    try {
      await invoke('disconnect')
    } catch (e) {
      // Tunnel teardown failing shouldn't strand the user on a dead panel.
      console.error('disconnect failed', e)
    } finally {
      onDisconnect()
    }
  }

  return (
    <div className="relative flex h-screen w-screen flex-col bg-background">
      {!loaded && !error && (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-background">
          <Spinner size={32} />
          <span className="text-sm text-muted-foreground">Загрузка панели…</span>
        </div>
      )}

      {error && (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-background px-6 text-center">
          <p className="text-sm text-destructive">{error}</p>
          <button
            type="button"
            onClick={() => void disconnect()}
            className="rounded-[var(--radius)] border border-border px-4 py-1.5 text-sm text-muted-foreground hover:text-foreground"
          >
            Назад к подключениям
          </button>
        </div>
      )}

      <iframe
        src={panelUrl}
        title="Панель"
        className="h-full w-full flex-1 border-0"
        onLoad={() => setLoaded(true)}
        onError={() => setError('Не удалось загрузить панель')}
      />
    </div>
  )
}
