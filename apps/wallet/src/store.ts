import { Toast } from "@base-ui/react/toast"
import { useRef, useSyncExternalStore } from "react"

import type {
  Amount,
  AssetId,
  DailyPrice,
  FiatCurrency,
  PriceHistory,
  PriceSnapshot,
  StartupStatus,
} from "@/ipc/bindings.gen.ts"
import { pow10 } from "@/lib/amount.ts"
import { assetDecimals, assetKey } from "@/lib/chain.ts"
import {
  PRICE_DECIMALS,
  fiatValue,
  isPriceStale,
  nextStaleAtUnixSecs,
  priceForAsset,
} from "@/lib/fiat.ts"
import type { Prices, Quote } from "@/lib/fiat.ts"
import { MS_PER_SECOND, SECONDS_PER_DAY, unixSecondsOf } from "@/lib/format.ts"

interface ReadonlySignal<T> {
  get: () => T
  subscribe: (listener: () => void) => () => void
}

interface Signal<T> extends ReadonlySignal<T> {
  set: (next: T) => void
}

function createSignal<T>(initial: T): Signal<T> {
  let current = initial
  const listeners = new Set<() => void>()

  return {
    get: () => current,
    set: (next) => {
      if (Object.is(next, current)) {
        return
      }
      current = next
      for (const listener of listeners) {
        listener()
      }
    },
    subscribe: (listener) => {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
  }
}

function useSignal<T>(signal: ReadonlySignal<T>): T {
  return useSyncExternalStore(signal.subscribe, signal.get, signal.get)
}

interface Selection<T, S> {
  source: T
  select: (value: T) => S
  value: S
}

function useSignalSelector<T, S>(signal: ReadonlySignal<T>, select: (value: T) => S): S {
  const cached = useRef<Selection<T, S> | null>(null)

  function read(): S {
    const source = signal.get()
    const selection = cached.current
    if (selection !== null && Object.is(selection.source, source) && selection.select === select) {
      return selection.value
    }
    const value = select(source)
    cached.current = { source, select, value }
    return value
  }

  return useSyncExternalStore(signal.subscribe, read, read)
}

export const toastManager = Toast.createToastManager()

const SUCCESS_TIMEOUT_MS = 4000
const WARNING_TIMEOUT_MS = 6000
const FAILURE_TIMEOUT_MS = 8000

export const notify = {
  success: (title: string, description?: string): void => {
    toastManager.add({ title, description, type: "success", timeout: SUCCESS_TIMEOUT_MS })
  },

  warning: (title: string, description?: string): void => {
    toastManager.add({ title, description, type: "warning", timeout: WARNING_TIMEOUT_MS })
  },

  failure: (title: string, description: string): void => {
    toastManager.add({ title, description, type: "error", timeout: FAILURE_TIMEOUT_MS })
  },
}

const startupState = createSignal<StartupStatus | null>(null)

const startupSignal: ReadonlySignal<StartupStatus | null> = startupState

export function recordStartupStatus(status: StartupStatus): void {
  const current = startupState.get()
  if (status.kind === "starting" && current !== null && current.kind !== "starting") {
    return
  }
  startupState.set(status)
}

export function useStartupStatus(): StartupStatus | null {
  return useSignal(startupSignal)
}

const NO_PRICES: Prices = { currency: "usd", quotes: {}, judgedAtUnixSecs: 0 }

const priceState = createSignal<Prices>(NO_PRICES)

export const priceSignal: ReadonlySignal<Prices> = priceState

let staleTimer: ReturnType<typeof setTimeout> | null = null

function judge(currency: FiatCurrency, quotes: Readonly<Record<string, Quote>>): void {
  if (staleTimer !== null) {
    clearTimeout(staleTimer)
    staleTimer = null
  }
  const judgedAtUnixSecs = unixSecondsOf(Date.now())
  priceState.set({ currency, quotes, judgedAtUnixSecs })

  const staleAtUnixSecs = nextStaleAtUnixSecs(quotes, judgedAtUnixSecs)
  if (staleAtUnixSecs === null) {
    return
  }
  staleTimer = setTimeout(
    () => {
      const current = priceState.get()
      judge(current.currency, current.quotes)
    },
    Math.max(0, staleAtUnixSecs - judgedAtUnixSecs) * MS_PER_SECOND,
  )
}

export function setPrices(snapshot: PriceSnapshot): void {
  const quotes: Record<string, Quote> = {}
  for (const record of snapshot.prices) {
    quotes[assetKey(record.asset)] = {
      price: BigInt(record.price),
      asOfUnixSecs: record.asOfUnixSecs,
    }
  }
  judge(snapshot.currency, quotes)
}

export function resetPrices(): void {
  judge(priceState.get().currency, {})
}

const PRICE_SCALE = pow10(PRICE_DECIMALS)

type DailyPrices = ReadonlyMap<number, bigint>

interface PriceHistoryState {
  prices: Readonly<Record<string, DailyPrices>>
  rates: Partial<Readonly<Record<FiatCurrency, DailyPrices>>>
}

const NO_HISTORY: PriceHistoryState = { prices: {}, rates: {} }

const historyState = createSignal<PriceHistoryState>(NO_HISTORY)

const priceHistorySignal: ReadonlySignal<PriceHistoryState> = historyState

function dailyPrices(days: readonly DailyPrice[]): DailyPrices {
  return new Map(days.map((day) => [day.dayUnixSecs, BigInt(day.price)]))
}

export function setPriceHistory(history: PriceHistory): void {
  const prices: Record<string, DailyPrices> = {}
  const rates: Partial<Record<FiatCurrency, DailyPrices>> = {}
  for (const series of history.prices) {
    const days = dailyPrices(series.prices)
    for (const asset of series.assets) {
      prices[assetKey(asset)] = days
    }
  }
  for (const series of history.rates) {
    rates[series.currency] = dailyPrices(series.prices)
  }
  historyState.set({ prices, rates })
}

export function resetPriceHistory(): void {
  historyState.set(NO_HISTORY)
}

function historicalPrice(
  history: PriceHistoryState,
  asset: AssetId,
  currency: FiatCurrency,
  unixSeconds: number | null,
): bigint | null {
  if (unixSeconds === null) {
    return null
  }
  const dayUnixSecs = Math.floor(unixSeconds / SECONDS_PER_DAY) * SECONDS_PER_DAY
  const usd = history.prices[assetKey(asset)]?.get(dayUnixSecs) ?? null
  if (usd === null || currency === "usd") {
    return usd
  }
  const rate = history.rates[currency]?.get(dayUnixSecs)
  return rate === undefined ? null : (usd * rate) / PRICE_SCALE
}

export function usePrices(): Prices {
  return useSignal(priceSignal)
}

export function useFiatCurrency(): FiatCurrency {
  return useSignalSelector(priceSignal, (prices) => prices.currency)
}

export function useIsFiatStale(asset: AssetId): boolean {
  return useSignalSelector(priceSignal, (prices) => isPriceStale(asset, prices))
}

export function usePrice(asset: AssetId): bigint | null {
  return useSignalSelector(priceSignal, (prices) => priceForAsset(asset, prices))
}

export function useFiatValue(amount: Amount | bigint | null, asset: AssetId): bigint | null {
  const price = usePrice(asset)
  return price === null ? null : fiatValue(amount, assetDecimals(asset), price)
}

export function useHistoricalPrice(
  asset: AssetId,
  currency: FiatCurrency,
  unixSeconds: number | null,
): bigint | null {
  return useSignalSelector(priceHistorySignal, (history) =>
    historicalPrice(history, asset, currency, unixSeconds),
  )
}
