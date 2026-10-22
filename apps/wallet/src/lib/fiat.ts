import type { Amount, AssetId, FiatCurrency } from "@/ipc/bindings.gen.ts"
import {
  U128_MAX,
  amountToBigInt,
  formatAmount,
  pow10,
  signPrefix,
  toBaseUnits,
} from "@/lib/amount.ts"
import type { Parsed } from "@/lib/amount.ts"
import { assetKey } from "@/lib/chain.ts"

export const PRICE_DECIMALS = 8

const PRICE_SIGNIFICANT_DIGITS = 4

interface CurrencyInfo {
  code: string
  name: string
  prefix: string
  fractionDigits: number
}

export const CURRENCIES: Readonly<Record<FiatCurrency, CurrencyInfo>> = {
  usd: { code: "USD", name: "US dollar", prefix: "$", fractionDigits: 2 },
  eur: { code: "EUR", name: "Euro", prefix: "€", fractionDigits: 2 },
  gbp: { code: "GBP", name: "British pound", prefix: "£", fractionDigits: 2 },
  aud: { code: "AUD", name: "Australian dollar", prefix: "A$", fractionDigits: 2 },
  cad: { code: "CAD", name: "Canadian dollar", prefix: "CA$", fractionDigits: 2 },
  jpy: { code: "JPY", name: "Japanese yen", prefix: "¥", fractionDigits: 0 },
  chf: { code: "CHF", name: "Swiss franc", prefix: "CHF ", fractionDigits: 2 },
}

export function fiatValue(
  amount: Amount | bigint | null,
  decimals: number,
  price: bigint,
): bigint | null {
  const raw = amountToBigInt(amount)
  return raw === null ? null : (raw * price) / pow10(decimals)
}

export function fiatToBaseUnits(
  input: string,
  currency: FiatCurrency,
  decimals: number,
  price: bigint | null,
): Parsed<Amount> {
  const { fractionDigits } = CURRENCIES[currency]
  const typed = toBaseUnits(input, fractionDigits)
  if (!typed.isOk) {
    return typed
  }
  if (price === null || price <= 0n) {
    return {
      isOk: false,
      error: "No price is available for this asset. Switch to units to enter the amount.",
    }
  }
  const fiat = BigInt(typed.value) * pow10(PRICE_DECIMALS - fractionDigits)
  const value = (fiat * pow10(decimals)) / price
  if (value > U128_MAX) {
    return { isOk: false, error: "Enter a smaller amount." }
  }
  return { isOk: true, value: value.toString() }
}

interface FormatFiatOptions {
  isSigned?: boolean
  isPrice?: boolean
  fractionDigits?: number
}

function shownFractionDigits(
  magnitude: bigint,
  currency: FiatCurrency,
  options: FormatFiatOptions,
): number {
  if (options.fractionDigits !== undefined) {
    return options.fractionDigits
  }
  const { fractionDigits } = CURRENCIES[currency]
  const wholeDigits = magnitude.toString().length - PRICE_DECIMALS
  if (options.isPrice !== true || wholeDigits > 0) {
    return fractionDigits
  }
  return Math.min(PRICE_DECIMALS, Math.max(fractionDigits, PRICE_SIGNIFICANT_DIGITS - wholeDigits))
}

export function formatFiat(
  value: bigint,
  currency: FiatCurrency,
  options: FormatFiatOptions = {},
): string {
  const { prefix } = CURRENCIES[currency]
  const sign = signPrefix(value, options.isSigned === true)
  const magnitude = value < 0n ? -value : value
  const fractionDigits = shownFractionDigits(magnitude, currency, options)
  const formatOptions = { maxFractionDigits: fractionDigits, shouldKeepTrailingZeros: true }
  const smallest = pow10(PRICE_DECIMALS - fractionDigits)
  if (magnitude > 0n && magnitude < smallest) {
    return `${sign}<${prefix}${formatAmount(smallest, PRICE_DECIMALS, formatOptions)}`
  }
  return `${sign}${prefix}${formatAmount(magnitude, PRICE_DECIMALS, formatOptions)}`
}

const SECONDS_PER_MINUTE = 60

const FRESH_MINUTES = 15

const FRESH_SECONDS = FRESH_MINUTES * SECONDS_PER_MINUTE

export const STALE_PRICE_NOTICE = "This value uses an outdated price."

export const HISTORICAL_PRICE_NOTICE =
  "Historical value uses that day's closing price and exchange rate in UTC."

export interface Quote {
  price: bigint
  asOfUnixSecs: number
}

export interface Prices {
  currency: FiatCurrency
  quotes: Readonly<Record<string, Quote>>
  judgedAtUnixSecs: number
}

function isQuoteStale(quote: Quote, judgedAtUnixSecs: number): boolean {
  return judgedAtUnixSecs - quote.asOfUnixSecs >= FRESH_SECONDS
}

export function nextStaleAtUnixSecs(
  quotes: Readonly<Record<string, Quote>>,
  judgedAtUnixSecs: number,
): number | null {
  const staleAtUnixSecs = Object.values(quotes)
    .filter((quote) => !isQuoteStale(quote, judgedAtUnixSecs))
    .map((quote) => quote.asOfUnixSecs + FRESH_SECONDS)
  return staleAtUnixSecs.length === 0 ? null : Math.min(...staleAtUnixSecs)
}

export function priceForAsset(asset: AssetId, prices: Prices): bigint | null {
  return prices.quotes[assetKey(asset)]?.price ?? null
}

export function isPriceStale(asset: AssetId, prices: Prices): boolean {
  const quote = prices.quotes[assetKey(asset)]
  return quote !== undefined && isQuoteStale(quote, prices.judgedAtUnixSecs)
}
