import type { CandleInterval, FiatCurrency, PriceChart } from "@/ipc/bindings.gen.ts"
import { formatAmount, pow10 } from "@/lib/amount.ts"
import { CURRENCIES, PRICE_DECIMALS, formatFiat } from "@/lib/fiat.ts"
import { MS_PER_SECOND, SECONDS_PER_DAY } from "@/lib/format.ts"

export type ChartRange = "1d" | "1w" | "1m" | "3m" | "6m" | "1y" | "max"

export type ChartMode = "line" | "candles"

const INTERVAL_SECONDS = {
  hour: 3600,
  day: SECONDS_PER_DAY,
  week: 604_800,
} as const satisfies Record<CandleInterval, number>

interface RangeSpec {
  interval: CandleInterval
  spanDays: number | null
  candleIntervals: number | null
}

export const CHART_RANGES = {
  "1d": { interval: "hour", spanDays: 1, candleIntervals: 1 },
  "1w": { interval: "hour", spanDays: 7, candleIntervals: 4 },
  "1m": { interval: "day", spanDays: 30, candleIntervals: 1 },
  "3m": { interval: "day", spanDays: 90, candleIntervals: 2 },
  "6m": { interval: "day", spanDays: 180, candleIntervals: 3 },
  "1y": { interval: "day", spanDays: 365, candleIntervals: 7 },
  max: { interval: "week", spanDays: null, candleIntervals: null },
} as const satisfies Record<ChartRange, RangeSpec>

const MAX_CANDLES = 60

const CANDLE_WEEKS = {
  week: 1,
  fortnight: 2,
  month: 4,
  quarter: 13,
  halfYear: 26,
  year: 52,
} as const

const BASIS_POINTS_PER_WHOLE = 10_000n

const PERCENT_DECIMALS = 2

export interface Candle {
  openUnixSecs: number
  open: bigint
  high: bigint
  low: bigint
  close: bigint
}

interface Chart {
  currency: FiatCurrency
  candles: readonly Candle[]
}

interface LivePrice {
  price: bigint
  currency: FiatCurrency
}

interface ChartPoint {
  unixSecs: number
  labelUnixSecs: number
  price: bigint
}

export type ChartSeries =
  | { mode: "line"; points: readonly ChartPoint[] }
  | { mode: "candles"; candles: readonly Candle[]; bucketSeconds: number }

interface Change {
  from: bigint
  to: bigint
}

function larger(a: bigint, b: bigint): bigint {
  return a > b ? a : b
}

function smaller(a: bigint, b: bigint): bigint {
  return a < b ? a : b
}

function spanSeconds(range: ChartRange): number | null {
  const { spanDays } = CHART_RANGES[range]
  return spanDays === null ? null : spanDays * SECONDS_PER_DAY
}

export function readChart(chart: PriceChart): Chart {
  return {
    currency: chart.currency,
    candles: chart.candles.map((candle) => ({
      openUnixSecs: candle.openUnixSecs,
      open: BigInt(candle.open),
      high: BigInt(candle.high),
      low: BigInt(candle.low),
      close: BigInt(candle.close),
    })),
  }
}

function withLivePrice(chart: Chart, live: LivePrice | null): readonly Candle[] {
  const last = chart.candles.at(-1)
  if (last === undefined || live === null || live.currency !== chart.currency) {
    return chart.candles
  }
  const patched: Candle = {
    ...last,
    high: larger(last.high, live.price),
    low: smaller(last.low, live.price),
    close: live.price,
  }
  return [...chart.candles.slice(0, -1), patched]
}

export function chartPoints(
  chart: Chart,
  range: ChartRange,
  live: LivePrice | null,
  nowUnixSecs: number,
): ChartPoint[] {
  const { interval } = CHART_RANGES[range]
  const intervalSeconds = INTERVAL_SECONDS[interval]
  const span = spanSeconds(range)
  const candles = withLivePrice(chart, live)
  function closedAt(candle: Candle): number {
    return Math.min(candle.openUnixSecs + intervalSeconds, nowUnixSecs)
  }
  const startIndex =
    span === null ? 0 : candles.findLastIndex((candle) => closedAt(candle) <= nowUnixSecs - span)
  return candles.slice(Math.max(0, startIndex)).map((candle) => ({
    unixSecs: closedAt(candle),
    labelUnixSecs: interval === "hour" ? closedAt(candle) : candle.openUnixSecs,
    price: candle.close,
  }))
}

function candleWeeks(candles: readonly Candle[]): number {
  const first = candles.at(0)
  const last = candles.at(-1)
  const weeks =
    first === undefined || last === undefined
      ? 0
      : (last.openUnixSecs - first.openUnixSecs) / INTERVAL_SECONDS.week + 1
  const multiples: readonly number[] = Object.values(CANDLE_WEEKS)
  return multiples.find((count) => Math.ceil(weeks / count) <= MAX_CANDLES) ?? CANDLE_WEEKS.year
}

export function chartCandles(
  chart: Chart,
  range: ChartRange,
  live: LivePrice | null,
  nowUnixSecs: number,
): ChartSeries {
  const { interval, candleIntervals } = CHART_RANGES[range]
  const candles = withLivePrice(chart, live)
  const bucketSeconds = (candleIntervals ?? candleWeeks(candles)) * INTERVAL_SECONDS[interval]
  const span = spanSeconds(range)
  function bucketOf(unixSecs: number): number {
    return Math.floor(unixSecs / bucketSeconds) * bucketSeconds
  }
  const startUnixSecs = span === null ? Number.NEGATIVE_INFINITY : bucketOf(nowUnixSecs - span)

  const buckets: Candle[] = []
  for (const candle of candles.filter((shown) => shown.openUnixSecs >= startUnixSecs)) {
    const openUnixSecs = bucketOf(candle.openUnixSecs)
    const current = buckets.at(-1)
    if (current?.openUnixSecs === openUnixSecs) {
      buckets[buckets.length - 1] = {
        ...current,
        high: larger(current.high, candle.high),
        low: smaller(current.low, candle.low),
        close: candle.close,
      }
    } else {
      buckets.push({ ...candle, openUnixSecs })
    }
  }
  return { mode: "candles", candles: buckets, bucketSeconds }
}

export function seriesChange(points: readonly ChartPoint[]): Change | null {
  const first = points.at(0)
  const last = points.at(-1)
  if (first === undefined || last === undefined || points.length < 2 || first.price <= 0n) {
    return null
  }
  return { from: first.price, to: last.price }
}

export function isRisingChange(change: Change): boolean {
  return change.to >= change.from
}

export function formatChange(change: Change): string {
  const delta = change.to - change.from
  const magnitude = delta < 0n ? -delta : delta
  const basisPoints = (magnitude * BASIS_POINTS_PER_WHOLE * 2n + change.from) / (change.from * 2n)
  const signed = delta < 0n ? -basisPoints : basisPoints
  const text = formatAmount(signed, PERCENT_DECIMALS, {
    shouldKeepTrailingZeros: true,
    isSigned: true,
  })
  return `${text}%`
}

export const AXIS_GAP = 6
const COORDINATE_DIGITS = 1
const MIDDLE = 0.5

export interface Bounds {
  left: number
  right: number
  top: number
  bottom: number
}

interface Extent {
  low: bigint
  high: bigint
}

interface Domain {
  startUnixSecs: number
  endUnixSecs: number
}

export interface Spot {
  x: number
  y: number
}

export interface PositionedTick {
  at: number
  label: string
}

const NO_EXTENT: Extent = { low: 0n, high: 0n }

function extentOf(prices: readonly bigint[]): Extent {
  return (
    prices.reduce<Extent | null>(
      (extent, price) =>
        extent === null
          ? { low: price, high: price }
          : {
              low: price < extent.low ? price : extent.low,
              high: price > extent.high ? price : extent.high,
            },
      null,
    ) ?? NO_EXTENT
  )
}

export function seriesExtent(series: ChartSeries): Extent {
  return series.mode === "line"
    ? extentOf(series.points.map((point) => point.price))
    : extentOf(series.candles.flatMap((candle) => [candle.low, candle.high]))
}

export function seriesDomain(series: ChartSeries): Domain {
  if (series.mode === "line") {
    return {
      startUnixSecs: series.points.at(0)?.unixSecs ?? 0,
      endUnixSecs: series.points.at(-1)?.unixSecs ?? 0,
    }
  }
  return {
    startUnixSecs: series.candles.at(0)?.openUnixSecs ?? 0,
    endUnixSecs: (series.candles.at(-1)?.openUnixSecs ?? 0) + series.bucketSeconds,
  }
}

export function yOf(price: bigint, extent: Extent, bounds: Bounds): number {
  const span = extent.high - extent.low
  const share = span === 0n ? MIDDLE : Number(price - extent.low) / Number(span)
  return bounds.bottom - share * (bounds.bottom - bounds.top)
}

export function xOf(unixSecs: number, domain: Domain, bounds: Bounds): number {
  const span = domain.endUnixSecs - domain.startUnixSecs
  if (span === 0) {
    return (bounds.left + bounds.right) / 2
  }
  return bounds.left + ((unixSecs - domain.startUnixSecs) / span) * (bounds.right - bounds.left)
}

export function coordinate(value: number): string {
  return value.toFixed(COORDINATE_DIGITS)
}

export function nearestIndex(spots: readonly Spot[], x: number): number {
  let nearest = 0
  let distance = Number.POSITIVE_INFINITY
  spots.forEach((spot, i) => {
    if (Math.abs(spot.x - x) < distance) {
      nearest = i
      distance = Math.abs(spot.x - x)
    }
  })
  return nearest
}

const STEP_MULTIPLIERS = { one: 1n, two: 2n, five: 5n, ten: 10n } as const

type TimeUnit = "hour" | "day" | "month" | "year"

interface TimeStep {
  unit: TimeUnit
  count: number
}

const TIME_STEPS: readonly TimeStep[] = [
  { unit: "hour", count: 1 },
  { unit: "hour", count: 2 },
  { unit: "hour", count: 3 },
  { unit: "hour", count: 6 },
  { unit: "hour", count: 12 },
  { unit: "day", count: 1 },
  { unit: "day", count: 2 },
  { unit: "day", count: 7 },
  { unit: "day", count: 14 },
  { unit: "month", count: 1 },
  { unit: "month", count: 2 },
  { unit: "month", count: 3 },
  { unit: "month", count: 6 },
  { unit: "year", count: 1 },
  { unit: "year", count: 2 },
  { unit: "year", count: 5 },
  { unit: "year", count: 10 },
]

const UNIT_SECONDS = {
  hour: 3600,
  day: SECONDS_PER_DAY,
  month: 2_629_800,
  year: 31_557_600,
} as const satisfies Record<TimeUnit, number>

const DAYS_PER_WEEK = 7

const MONDAY = 1

const CLOCK_FORMAT = new Intl.DateTimeFormat("en-US", {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
})

const TICK_DAY_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" })

const TICK_MONTH_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short" })

const HOUR_FORMAT = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
})

const DAY_FORMAT = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
  timeZone: "UTC",
})

interface PriceTick {
  price: bigint
  label: string
}

interface TimeTick {
  unixSecs: number
  label: string
}

function stepFractionDigits(step: bigint, currency: FiatCurrency): number {
  let digits = PRICE_DECIMALS
  let rest = step
  while (digits > 0 && rest % STEP_MULTIPLIERS.ten === 0n) {
    rest /= STEP_MULTIPLIERS.ten
    digits -= 1
  }
  return digits === 0 ? 0 : Math.max(CURRENCIES[currency].fractionDigits, digits)
}

export function priceTicks(
  low: bigint,
  high: bigint,
  maxCount: number,
  currency: FiatCurrency,
): PriceTick[] {
  const span = high - low
  if (span <= 0n || maxCount < 1) {
    return [{ price: low, label: formatFiat(low, currency, { isPrice: true }) }]
  }
  const rough = span / BigInt(maxCount)
  const magnitude = pow10(rough.toString().length - 1)
  const step =
    Object.values(STEP_MULTIPLIERS)
      .map((multiplier) => multiplier * magnitude)
      .find((candidate) => candidate >= rough) ?? magnitude * STEP_MULTIPLIERS.ten
  const fractionDigits = stepFractionDigits(step, currency)
  const ticks: PriceTick[] = []
  for (let price = ((low + step - 1n) / step) * step; price <= high; price += step) {
    ticks.push({ price, label: formatFiat(price, currency, { fractionDigits }) })
  }
  return ticks
}

function unitStart(unixSecs: number, unit: TimeUnit): Date {
  const at = new Date(unixSecs * MS_PER_SECOND)
  switch (unit) {
    case "hour": {
      return new Date(at.getFullYear(), at.getMonth(), at.getDate(), at.getHours())
    }
    case "day": {
      return new Date(at.getFullYear(), at.getMonth(), at.getDate())
    }
    case "month": {
      return new Date(at.getFullYear(), at.getMonth())
    }
    case "year": {
      return new Date(at.getFullYear(), 0)
    }
  }
}

function advance(at: Date, unit: TimeUnit, count: number): Date {
  switch (unit) {
    case "hour": {
      return new Date(at.getFullYear(), at.getMonth(), at.getDate(), at.getHours() + count)
    }
    case "day": {
      return new Date(at.getFullYear(), at.getMonth(), at.getDate() + count)
    }
    case "month": {
      return new Date(at.getFullYear(), at.getMonth() + count)
    }
    case "year": {
      return new Date(at.getFullYear() + count, 0)
    }
  }
}

function isOnStep(at: Date, step: TimeStep): boolean {
  switch (step.unit) {
    case "hour": {
      return at.getHours() % step.count === 0
    }
    case "day": {
      return step.count < DAYS_PER_WEEK || at.getDay() === MONDAY
    }
    case "month": {
      return at.getMonth() % step.count === 0
    }
    case "year": {
      return at.getFullYear() % step.count === 0
    }
  }
}

function tickLabel(at: Date, unit: TimeUnit): string {
  switch (unit) {
    case "hour": {
      return CLOCK_FORMAT.format(at)
    }
    case "day": {
      return TICK_DAY_FORMAT.format(at)
    }
    case "month": {
      return TICK_MONTH_FORMAT.format(at)
    }
    case "year": {
      return String(at.getFullYear())
    }
  }
}

export function timeTicks(
  startUnixSecs: number,
  endUnixSecs: number,
  maxCount: number,
): TimeTick[] {
  const span = endUnixSecs - startUnixSecs
  const step = TIME_STEPS.find(
    (candidate) => span / (candidate.count * UNIT_SECONDS[candidate.unit]) <= maxCount,
  )
  if (step === undefined || maxCount < 1) {
    return []
  }
  const ticks: TimeTick[] = []
  let at = unitStart(startUnixSecs, step.unit)
  while (at.getTime() <= endUnixSecs * MS_PER_SECOND) {
    const isTick = isOnStep(at, step)
    if (isTick && at.getTime() >= startUnixSecs * MS_PER_SECOND) {
      ticks.push({ unixSecs: at.getTime() / MS_PER_SECOND, label: tickLabel(at, step.unit) })
    }
    at = advance(at, step.unit, isTick ? step.count : 1)
  }
  return ticks
}

export function formatChartTime(unixSecs: number, interval: CandleInterval): string {
  const at = new Date(unixSecs * MS_PER_SECOND)
  return interval === "hour" ? HOUR_FORMAT.format(at) : DAY_FORMAT.format(at)
}
