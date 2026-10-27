import { queryOptions } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type { AssetId, CandleInterval, FiatCurrency } from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"
import { assetKey } from "@/lib/chain.ts"
import { setPriceHistory, setPrices } from "@/store.ts"

export const PRICE_CHART_REFRESH_MS = 300_000

export const moneyKeys = {
  root: ["money"] as const,
}

export const moneyQueries = {
  balances: () =>
    queryOptions({
      queryKey: [...moneyKeys.root, "balances"] as const,
      queryFn: () => unwrap(commands.listBalances()),
    }),

  totals: () =>
    queryOptions({
      queryKey: [...moneyKeys.root, "totals"] as const,
      queryFn: () => unwrap(commands.listBalanceTotals()),
    }),

  priceChart: (asset: AssetId, interval: CandleInterval, currency: FiatCurrency) =>
    queryOptions({
      queryKey: ["price-chart", assetKey(asset), interval, currency] as const,
      queryFn: () => unwrap(commands.getPriceChart(asset, interval)),
      staleTime: PRICE_CHART_REFRESH_MS,
      refetchInterval: PRICE_CHART_REFRESH_MS,
    }),
}

export async function loadPrices(): Promise<void> {
  try {
    await Promise.all([
      unwrap(commands.listPrices()).then(setPrices),
      unwrap(commands.listPriceHistory()).then(setPriceHistory),
    ])
  } catch {}
}
