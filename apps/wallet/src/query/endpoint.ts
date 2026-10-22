import { queryOptions } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type { ChainId, ProbeReport } from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

export const endpointQueries = {
  list: (chain: ChainId) =>
    queryOptions({
      queryKey: ["endpoints", "list", chain] as const,
      queryFn: () => unwrap(commands.listEndpoints(chain)),
    }),
}

export const endpointCommands = {
  add: (chain: ChainId, url: string, shouldAllowInsecure: boolean): Promise<null> =>
    unwrap(commands.addEndpoint(chain, url, shouldAllowInsecure)),

  remove: (chain: ChainId, url: string): Promise<null> =>
    unwrap(commands.deleteEndpointOverride(chain, url)),

  setEnabled: (chain: ChainId, url: string, isEnabled: boolean): Promise<null> =>
    unwrap(commands.setEndpointEnabled(chain, url, isEnabled)),

  setPinned: (chain: ChainId, url: string, isPinned: boolean): Promise<null> =>
    unwrap(commands.setEndpointPinned(chain, url, isPinned)),

  probe: (chain: ChainId, url: string): Promise<ProbeReport> =>
    unwrap(commands.probeEndpoint(chain, url)),
}
