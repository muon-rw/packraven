import { api, type ClientSlot, type MinecraftAccount } from './api'

// Each account stays undefined until we've read it, so no view claims you're signed out before we know.
export const minecraft = $state({
  account: undefined as MinecraftAccount | null | undefined,
  second: undefined as MinecraftAccount | null | undefined,
  signingIn: null as ClientSlot | null,
})

export const SIGN_IN_FIRST = 'Sign in to Minecraft to launch the client'

export function accountIn(slot: ClientSlot) {
  return slot === 'first' ? minecraft.account : minecraft.second
}

function setAccount(slot: ClientSlot, account: MinecraftAccount | null) {
  if (slot === 'first') minecraft.account = account
  else minecraft.second = account
}

export async function refreshAccounts() {
  const accounts = await api.minecraftAccounts().catch(() => null)
  if (!accounts) return
  minecraft.account = accounts.first
  minecraft.second = accounts.second
}

// Resolves to null when you cancel. A failure throws, so each caller shows it where you pressed the button.
export async function signIn(slot: ClientSlot = 'first'): Promise<MinecraftAccount | null> {
  minecraft.signingIn = slot
  try {
    const signedIn = await api.minecraftSignIn(slot)
    if (signedIn) setAccount(slot, signedIn)
    return signedIn
  } finally {
    if (minecraft.signingIn === slot) minecraft.signingIn = null
  }
}

export function cancelSignIn() {
  return api.minecraftSignInCancel()
}

export async function signOut(slot: ClientSlot = 'first') {
  await api.minecraftSignOut(slot)
  setAccount(slot, null)
}
