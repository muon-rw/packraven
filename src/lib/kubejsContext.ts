import { api, type KubejsContext } from './api'

// The lint for every open script shares one copy, which we drop when you switch packs or reload
// ProbeJS typings.
let context: KubejsContext | null = null
let loading: Promise<KubejsContext | null> | null = null

export function loadKubejsContext(): Promise<KubejsContext | null> {
  loading ??= api.kubejsContext().then(
    (loaded) => (context = loaded),
    () => null,
  )
  return loading
}

export function currentKubejsContext(): KubejsContext | null {
  return context
}

export function resetKubejsContext() {
  context = null
  loading = null
}
