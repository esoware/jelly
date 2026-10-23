import { queryOptions } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type {
  Platform,
  ProxySetting,
  StartupStatus,
  WindowBackdrop,
  WindowControls,
} from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"
import { installCatalog } from "@/lib/chain.ts"

export const appQueries = {
  version: () =>
    queryOptions({
      queryKey: ["app", "version"] as const,
      queryFn: () => unwrap(commands.getAppVersion()),
    }),

  dataDir: () =>
    queryOptions({
      queryKey: ["app", "data-dir"] as const,
      queryFn: () => unwrap(commands.getDataDir()),
    }),

  windowBackdrop: () =>
    queryOptions({
      queryKey: ["app", "window-backdrop"] as const,
      queryFn: () => appCommands.windowBackdrop(),
    }),
}

export const appCommands = {
  startupStatus: (): Promise<StartupStatus> => unwrap(commands.getStartupStatus()),
  repairProxy: (proxy: ProxySetting | null): Promise<null> => unwrap(commands.repairProxy(proxy)),
  platform: (): Promise<Platform> => unwrap(commands.getPlatform()),
  windowBackdrop: (): Promise<WindowBackdrop> => unwrap(commands.getWindowBackdrop()),
  windowControls: (): Promise<WindowControls> => unwrap(commands.getWindowControls()),
}

export async function loadCatalog(): Promise<void> {
  const [chains, assets] = await Promise.all([
    unwrap(commands.listChainMetadata()),
    unwrap(commands.listAssetMetadata()),
  ])
  installCatalog(chains, assets)
}
