import type { ModSide, PackConfig } from './api'

export type ServerSettings = Pick<PackConfig['server'], 'memory' | 'jvm_args' | 'args' | 'graalvm' | 'properties' | 'bundle_mods'>

// We keep the last confirmed server state for the active pack, so switching back to Server
// renders right away while re-verifying in the background.
export const serverCache = $state({
  pack: null as string | null,
  mods: null as ModSide[] | null,
  settings: null as ServerSettings | null,
  testPort: 25565,
  icon: null as string | null,
  launch: null as { windows: string; unix: string } | null,
})
