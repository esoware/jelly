import { queryOptions } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type { ChainId } from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

export const chainKeys = {
  root: ["chains"] as const,
}

export const chainQueries = {
  enabled: () =>
    queryOptions({
      queryKey: [...chainKeys.root, "enabled"] as const,
      queryFn: () => unwrap(commands.listEnabledChains()),
    }),
}

export const chainCommands = {
  syncNow: (chain: ChainId | null): Promise<ChainId[]> => unwrap(commands.syncNow(chain)),

  setEnabled: (chain: ChainId, isEnabled: boolean): Promise<null> =>
    unwrap(commands.setChainEnabled(chain, isEnabled)),

  setPollInterval: (chain: ChainId, pollSecs: number | null): Promise<null> =>
    unwrap(commands.setPollInterval(chain, pollSecs)),

  setRestoreHeight: (chain: ChainId, height: number | null): Promise<null> =>
    unwrap(commands.setRestoreHeight(chain, height)),
}
