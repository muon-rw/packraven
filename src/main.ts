import { mount } from 'svelte'
import '@fontsource-variable/atkinson-hyperlegible-next/wght.css'
import '@fontsource-variable/atkinson-hyperlegible-next/wght-italic.css'
import '@fontsource/jetbrains-mono/400.css'
import './app.css'
import App from './App.svelte'
import { applyTheme } from './lib/theme.svelte'

if (import.meta.env.DEV) {
  const bridge = await import('./dev/bridge')
  if (bridge.isBridgeHost()) {
    bridge.startBridgeHost()
  } else if (!('__TAURI_INTERNALS__' in window)) {
    if (bridge.wantsRealBackend()) {
      await bridge.installBridgeClient()
    } else {
      const { installMock } = await import('./dev/mock')
      installMock()
    }
  }
}

applyTheme()

// The webview's own menu (Back, Reload, Print) makes no sense in an app, so we only keep it
// where its Cut, Copy and Paste help: in text fields and over selected text. In dev builds,
// hold Shift to reach Inspect.
window.addEventListener('contextmenu', (e) => {
  if (import.meta.env.DEV && e.shiftKey) return
  const editable = e.target instanceof Element && e.target.closest('input, textarea, [contenteditable]:not([contenteditable="false"])')
  if (editable || window.getSelection()?.isCollapsed === false) return
  e.preventDefault()
})

export default mount(App, { target: document.getElementById('app')! })
