/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import Connect from './screens/Connect'
import { LanguageProvider } from './lib/i18n'

/**
 * The React app is only ever the Connect screen. Once `connect()` resolves,
 * the Rust side navigates the main window's webview top-level to the daemon
 * panel (`commands::connect` / `WebviewWindow::navigate`) — the panel is no
 * longer hosted in an `<iframe>` (blocked by the daemon's
 * `Content-Security-Policy: frame-ancestors 'none'`), so it is not rendered
 * by React at all. "Connected" == the webview has navigated away from this
 * app entirely; there is nothing for this component to track.
 *
 * Disconnecting (tray "Отключиться", or a panel-side action that calls the
 * `disconnect` command) navigates the webview back to this app's own entry
 * URL, which reloads it fresh — so this component simply remounts back to
 * the Connect screen. No `baseUrl` state or disconnect event listener is
 * needed here.
 */
function App() {
  return (
    <LanguageProvider>
      <Connect />
    </LanguageProvider>
  )
}

export default App
