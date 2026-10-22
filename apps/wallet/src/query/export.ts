import { useEffect } from "react"

import { commands } from "@/ipc/bindings.gen.ts"
import type { ChainId } from "@/ipc/bindings.gen.ts"
import { fireAndForget, unwrap } from "@/ipc/client.ts"

export const exportCommands = {
  seedPhrase: (password: string): Promise<string> => unwrap(commands.exportSeedPhrase(password)),

  seedPassphrase: (password: string): Promise<string> =>
    unwrap(commands.exportSeedPassphrase(password)),

  seedEntropyHex: (password: string): Promise<string> =>
    unwrap(commands.exportSeedEntropyHex(password)),

  accountSeed: (chain: ChainId, password: string): Promise<string> =>
    unwrap(commands.exportAccountSeed(chain, password)),

  privateKey: (addressId: number, password: string): Promise<string> =>
    unwrap(commands.exportPrivateKey(addressId, password)),

  setSecretScreen: (isProtected: boolean): void => {
    fireAndForget(commands.setSecretScreen(isProtected))
  },
}

export function useSecretScreen(): void {
  useEffect(() => {
    exportCommands.setSecretScreen(true)
    return () => {
      exportCommands.setSecretScreen(false)
    }
  }, [])
}
