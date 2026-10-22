import { queryOptions } from "@tanstack/react-query"
import type { QueryClient } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type {
  AmountEntry,
  AppSettings,
  AssetId,
  BackdropSetting,
  FiatCurrency,
  ProxySetting,
} from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

export const settingKeys = {
  root: ["settings"] as const,
}

export const settingQueries = {
  all: () =>
    queryOptions({
      queryKey: settingKeys.root,
      queryFn: () => unwrap(commands.getSettings()),
    }),
}

export function patchSettings(
  client: QueryClient,
  patch: Partial<AppSettings> | ((settings: AppSettings) => Partial<AppSettings>),
): void {
  client.setQueryData(settingQueries.all().queryKey, (current) => {
    if (current === undefined) {
      return current
    }
    return { ...current, ...(typeof patch === "function" ? patch(current) : patch) }
  })
}

export const settingCommands = {
  setEtherscanApiKey: (key: string | null): Promise<null> =>
    unwrap(commands.setEtherscanApiKey(key)),

  setTrongridApiKey: (key: string | null): Promise<null> => unwrap(commands.setTrongridApiKey(key)),

  setAutoLockMinutes: (minutes: number): Promise<null> =>
    unwrap(commands.setAutoLockMinutes(minutes)),

  setPriceFeedEnabled: (isEnabled: boolean): Promise<null> =>
    unwrap(commands.setPriceFeedEnabled(isEnabled)),

  setPriceChartEnabled: (isEnabled: boolean): Promise<null> =>
    unwrap(commands.setPriceChartEnabled(isEnabled)),

  setFiatCurrency: (currency: FiatCurrency): Promise<null> =>
    unwrap(commands.setFiatCurrency(currency)),

  setAmountEntry: (entry: AmountEntry): Promise<null> => unwrap(commands.setAmountEntry(entry)),

  setProxy: (proxy: ProxySetting | null): Promise<null> => unwrap(commands.setProxy(proxy)),

  setBackdrop: (backdrop: BackdropSetting): Promise<null> => unwrap(commands.setBackdrop(backdrop)),

  setShownAsset: (asset: AssetId): Promise<null> => unwrap(commands.setShownAsset(asset)),
}
