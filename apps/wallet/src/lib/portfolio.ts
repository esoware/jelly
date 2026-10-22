import type {
  Amount,
  AssetId,
  AssetTotal,
  BalanceRecord,
  ChainId,
  FiatCurrency,
} from "@/ipc/bindings.gen.ts"
import { isZeroAmount } from "@/lib/amount.ts"
import { assetChain, assetDecimals, assetKey, nativeAsset } from "@/lib/chain.ts"
import { fiatValue, isPriceStale, priceForAsset } from "@/lib/fiat.ts"
import type { Prices } from "@/lib/fiat.ts"

const WEIGHT_SCALE = 1_000_000n

const TENTHS_PER_PERCENT = 10n

const TENTHS_PER_WHOLE = 1000n

interface Position {
  asset: AssetId
  amount: Amount
  decimals: number
}

export interface Holdings extends Position {
  chain: ChainId
  tokens: readonly Position[]
}

export function holdingsFor(chain: ChainId, totals: readonly AssetTotal[]): Holdings {
  const asset = nativeAsset(chain)
  const native =
    totals.find((row) => row.asset.kind === "native" && row.asset.value === chain) ?? null
  return {
    chain,
    asset,
    amount: native?.amount ?? "0",
    decimals: assetDecimals(asset),
    tokens: totals
      .filter((row) => row.asset.kind === "token" && assetChain(row.asset) === chain)
      .map((row) => ({ asset: row.asset, amount: row.amount, decimals: assetDecimals(row.asset) })),
  }
}

export function positionOf(holdings: Holdings, asset: AssetId): Position {
  const key = assetKey(asset)
  const held = [holdings, ...holdings.tokens].find((position) => assetKey(position.asset) === key)
  return held ?? { asset, amount: "0", decimals: assetDecimals(asset) }
}

export function addressBalance(
  balances: readonly BalanceRecord[],
  addressId: number,
  asset: AssetId,
): BalanceRecord | undefined {
  const key = assetKey(asset)
  return balances.find((row) => row.addressId === addressId && assetKey(row.asset) === key)
}

export function holdsFunds(
  balances: readonly BalanceRecord[],
  addressIds: readonly number[],
): boolean {
  return balances.some((row) => addressIds.includes(row.addressId) && !isZeroAmount(row.amount))
}

export interface AssetValue extends Position {
  value: bigint
  isPriced: boolean
  isStale: boolean
}

export interface Allocation {
  currency: FiatCurrency
  assets: readonly AssetValue[]
  total: bigint
  hasUnpriced: boolean
  isStale: boolean
}

export function allocate(holdings: readonly Holdings[], prices: Prices): Allocation {
  const assets = holdings
    .flatMap((entry) => [entry, ...entry.tokens])
    .filter((position) => !isZeroAmount(position.amount))
    .map(({ asset, amount, decimals }): AssetValue => {
      const price = priceForAsset(asset, prices)
      const value = price === null ? null : fiatValue(amount, decimals, price)
      return {
        asset,
        amount,
        decimals,
        value: value ?? 0n,
        isPriced: value !== null,
        isStale: value !== null && isPriceStale(asset, prices),
      }
    })
    .toSorted((a, b) => {
      if (a.value === b.value) {
        return 0
      }
      return a.value > b.value ? -1 : 1
    })
  return {
    currency: prices.currency,
    assets,
    total: assets.reduce((sum, entry) => sum + entry.value, 0n),
    hasUnpriced: assets.some((entry) => !entry.isPriced),
    isStale: assets.some((entry) => entry.isStale),
  }
}

interface SliceWeight {
  asset: AssetId
  weight: number
}

export function sliceWeights(allocation: Allocation): SliceWeight[] {
  if (allocation.total <= 0n) {
    return []
  }
  return allocation.assets
    .filter((entry) => entry.value > 0n)
    .map(({ asset, value }) => ({
      asset,
      weight: Math.max(1, Number((value * WEIGHT_SCALE) / allocation.total)),
    }))
}

export function formatShare(part: bigint, whole: bigint): string {
  if (part <= 0n || whole <= 0n) {
    return "0%"
  }
  const tenths = (part * TENTHS_PER_WHOLE * 2n + whole) / (whole * 2n)
  if (tenths === 0n) {
    return "<0.1%"
  }
  const percent = (tenths / TENTHS_PER_PERCENT).toString()
  const fraction = tenths % TENTHS_PER_PERCENT
  return fraction === 0n ? `${percent}%` : `${percent}.${fraction.toString()}%`
}
