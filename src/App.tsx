import { listen } from '@tauri-apps/api/event'
import { useEffect, useState } from 'react'
import Connect from './screens/Connect'
import Panel from './screens/Panel'
import './App.css'

function App() {
  const [baseUrl, setBaseUrl] = useState<string | null>(null)

  // The tray's "Отключиться" menu item tears the tunnel down on the Rust
  // side directly (it doesn't go through the WebView), then emits this
  // event so the UI falls back to the connect screen too.
  useEffect(() => {
    const unlisten = listen('voltadmin-disconnected', () => {
      setBaseUrl(null)
    })
    return () => {
      void unlisten.then((f) => f())
    }
  }, [])

  if (baseUrl) {
    return <Panel baseUrl={baseUrl} onDisconnect={() => setBaseUrl(null)} />
  }

  return <Connect onConnected={setBaseUrl} />
}

export default App
