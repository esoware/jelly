import { priceTicks, seriesDomain, seriesExtent, timeTicks } from "@/features/wallet/price-scale.ts"
import type { ChartSeries, PriceTick, TimeTick } from "@/features/wallet/price-scale.ts"
import { lerp } from "@/ui/use-morph.ts"

export const AXIS_GAP = 6
const INSET_TOP = 8
const INSET_RIGHT = 4
const TIME_AXIS_HEIGHT = 20
const EDGE_ROOM = 16
const MIN_PRICE_TICK_GAP = 28
const MIN_TIME_TICK_GAP = 72
const BODY_SHARE = 0.6
const MAX_BODY_WIDTH = 12

interface Bounds {
  left: number
  right: number
  top: number
  bottom: number
}

interface Scale<V> {
  origin: V
  at: number
  perUnit: number
}

interface Affine {
  offset: number
  factor: number
}

interface Relation {
  x: Affine
  y: Affine
}

interface Pace {
  progress: number
  x: number
  y: number
}

interface TickMark<T> {
  tick: T
  opacity: number
}

interface Vertex {
  unixSecs: number
  y: number
}

export interface LineMark {
  vertices: readonly Vertex[]
  rise: number
  opacity: number
}

export interface CandleMark {
  key: string
  startUnixSecs: number
  endUnixSecs: number
  x: number
  width: number
  openY: number
  closeY: number
  highY: number
  lowY: number
  rise: number
  opacity: number
}

export interface Plot {
  bounds: Bounds
  time: Scale<number>
  price: Scale<bigint>
  priceTicks: readonly TickMark<PriceTick>[]
  timeTicks: readonly TickMark<TimeTick>[]
  line: LineMark | null
  candles: readonly CandleMark[]
}

interface PlotSize {
  width: number
  height: number
  labelWidth: number
}

export function placeTime(scale: Scale<number>, unixSecs: number): number {
  return scale.at + scale.perUnit * (unixSecs - scale.origin)
}

export function placePrice(scale: Scale<bigint>, price: bigint): number {
  return scale.at + scale.perUnit * Number(price - scale.origin)
}

export function drawPlot(series: ChartSeries, size: PlotSize): Plot {
  const bounds: Bounds = {
    left: size.labelWidth + AXIS_GAP,
    right: size.width - INSET_RIGHT,
    top: INSET_TOP,
    bottom: size.height - TIME_AXIS_HEIGHT,
  }
  const plotWidth = Math.max(1, bounds.right - bounds.left)
  const plotHeight = Math.max(1, bounds.bottom - bounds.top)
  const extent = seriesExtent(series)
  const domain = seriesDomain(series)
  const priceSpan = extent.high - extent.low
  const flatSpan = extent.low > 1n ? extent.low : 1n
  const scaleSpan = priceSpan > 0n ? priceSpan : flatSpan
  const price: Scale<bigint> = {
    origin: extent.low - (scaleSpan - priceSpan) / 2n,
    at: bounds.bottom,
    perUnit: -plotHeight / Number(scaleSpan),
  }
  const time: Scale<number> = {
    origin: domain.startUnixSecs,
    at: bounds.left,
    perUnit: plotWidth / Math.max(1, domain.endUnixSecs - domain.startUnixSecs),
  }

  return {
    bounds,
    time,
    price,
    priceTicks: priceTicks(
      extent.low,
      extent.high,
      Math.floor(plotHeight / MIN_PRICE_TICK_GAP),
      series.currency,
    ).map((tick) => ({ tick, opacity: 1 })),
    timeTicks: timeTicks(
      domain.startUnixSecs,
      domain.endUnixSecs,
      Math.floor(plotWidth / MIN_TIME_TICK_GAP),
    )
      .filter((tick) => {
        const x = placeTime(time, tick.unixSecs)
        return x >= bounds.left + EDGE_ROOM && x <= bounds.right - EDGE_ROOM
      })
      .map((tick) => ({ tick, opacity: 1 })),
    line:
      series.mode === "line"
        ? {
            vertices: series.points.map((point) => ({
              unixSecs: point.unixSecs,
              y: placePrice(price, point.price),
            })),
            rise: (series.points.at(-1)?.price ?? 0n) >= (series.points.at(0)?.price ?? 0n) ? 1 : 0,
            opacity: 1,
          }
        : null,
    candles:
      series.mode === "candles"
        ? series.candles.map((candle) => ({
            key: String(candle.openUnixSecs),
            startUnixSecs: candle.openUnixSecs,
            endUnixSecs: candle.openUnixSecs + series.bucketSeconds,
            x: placeTime(time, candle.openUnixSecs + series.bucketSeconds / 2),
            width: Math.min(
              MAX_BODY_WIDTH,
              Math.max(1, series.bucketSeconds * time.perUnit * BODY_SHARE),
            ),
            openY: placePrice(price, candle.open),
            closeY: placePrice(price, candle.close),
            highY: placePrice(price, candle.high),
            lowY: placePrice(price, candle.low),
            rise: candle.close >= candle.open ? 1 : 0,
            opacity: 1,
          }))
        : [],
  }
}

function zoomReach<V>(from: Scale<V>, to: Scale<V>, progress: number): number {
  const zoom = Math.log(to.perUnit / from.perUnit)
  return zoom === 0 ? progress : Math.expm1(zoom * progress) / Math.expm1(zoom)
}

function blendScale<V>(from: Scale<V>, to: Scale<V>, shift: number, reach: number): Scale<V> {
  return {
    origin: to.origin,
    at: lerp(from.at - from.perUnit * shift, to.at, reach),
    perUnit: lerp(from.perUnit, to.perUnit, reach),
  }
}

function relate<V>(scale: Scale<V>, onto: Scale<V>, shift: number): Affine {
  const factor = onto.perUnit / scale.perUnit
  return { factor, offset: onto.at + onto.perUnit * shift - factor * scale.at }
}

function invert(affine: Affine): Affine {
  return { factor: 1 / affine.factor, offset: -affine.offset / affine.factor }
}

function apply(affine: Affine, y: number): number {
  return affine.offset + affine.factor * y
}

function hiddenLine(line: LineMark, y: Affine): LineMark {
  return {
    vertices: line.vertices.map((vertex) => ({
      unixSecs: vertex.unixSecs,
      y: apply(y, vertex.y),
    })),
    rise: line.rise,
    opacity: 0,
  }
}

function traceAlong(vertices: readonly Vertex[], times: readonly number[]): number[] {
  let index = 0
  return times.map((unixSecs) => {
    while ((vertices[index]?.unixSecs ?? Number.POSITIVE_INFINITY) < unixSecs) {
      index += 1
    }
    const next = vertices[index]
    const previous = vertices[index - 1]
    if (next === undefined) {
      return previous?.y ?? 0
    }
    if (previous === undefined || next.unixSecs === unixSecs) {
      return next.y
    }
    const share = (unixSecs - previous.unixSecs) / (next.unixSecs - previous.unixSecs)
    return lerp(previous.y, next.y, share)
  })
}

function blendLines(from: LineMark, to: LineMark, pace: Pace): LineMark {
  const times = [
    ...new Set([...from.vertices, ...to.vertices].map((vertex) => vertex.unixSecs)),
  ].toSorted((a, b) => a - b)
  const fromYs = traceAlong(from.vertices, times)
  const toYs = traceAlong(to.vertices, times)
  return {
    vertices: times.map((unixSecs, i) => ({
      unixSecs,
      y: lerp(fromYs[i] ?? 0, toYs[i] ?? 0, pace.y),
    })),
    rise: lerp(from.rise, to.rise, pace.progress),
    opacity: lerp(from.opacity, to.opacity, pace.progress),
  }
}

function covers(host: CandleMark, candle: CandleMark): boolean {
  const middle = (candle.startUnixSecs + candle.endUnixSecs) / 2
  return host.startUnixSecs <= middle && middle < host.endUnixSecs
}

function absentCandle(candle: CandleMark, line: LineMark | null, view: Relation): CandleMark {
  const x = apply(view.x, candle.x)
  if (line === null) {
    return {
      ...candle,
      x,
      openY: apply(view.y, candle.openY),
      closeY: apply(view.y, candle.closeY),
      highY: apply(view.y, candle.highY),
      lowY: apply(view.y, candle.lowY),
      opacity: 0,
    }
  }
  const [openY = 0, closeY = 0] = traceAlong(line.vertices, [
    candle.startUnixSecs,
    candle.endUnixSecs,
  ])
  const inside = line.vertices
    .filter(
      (vertex) => vertex.unixSecs > candle.startUnixSecs && vertex.unixSecs < candle.endUnixSecs,
    )
    .map((vertex) => vertex.y)
  return {
    ...candle,
    x,
    width: 0,
    openY,
    closeY,
    highY: Math.min(openY, closeY, ...inside),
    lowY: Math.max(openY, closeY, ...inside),
    rise: line.rise,
    opacity: 0,
  }
}

function mixCandles(key: string, from: CandleMark, to: CandleMark, pace: Pace): CandleMark {
  return {
    key,
    startUnixSecs: lerp(from.startUnixSecs, to.startUnixSecs, pace.progress),
    endUnixSecs: lerp(from.endUnixSecs, to.endUnixSecs, pace.progress),
    x: lerp(from.x, to.x, pace.x),
    width: lerp(from.width, to.width, pace.x),
    openY: lerp(from.openY, to.openY, pace.y),
    closeY: lerp(from.closeY, to.closeY, pace.y),
    highY: lerp(from.highY, to.highY, pace.y),
    lowY: lerp(from.lowY, to.lowY, pace.y),
    rise: lerp(from.rise, to.rise, pace.progress),
    opacity: lerp(from.opacity, to.opacity, pace.progress),
  }
}

function blendCandles(
  from: Plot,
  to: Plot,
  forward: Relation,
  backward: Relation,
  pace: Pace,
): CandleMark[] {
  const toKeys = new Set(to.candles.map((candle) => candle.key))
  const fromByKey = new Map(from.candles.map((candle) => [candle.key, candle]))
  const leaving = from.candles
    .filter((candle) => !toKeys.has(candle.key))
    .map((candle) => {
      const host =
        to.candles.find((target) => covers(target, candle)) ??
        absentCandle(candle, to.line, forward)
      return mixCandles(candle.key, candle, host, pace)
    })
  const arriving = to.candles.map((candle) => {
    const source =
      fromByKey.get(candle.key) ??
      from.candles.find((shown) => covers(shown, candle)) ??
      absentCandle(candle, from.line, backward)
    return mixCandles(candle.key, source, candle, pace)
  })
  return [...leaving, ...arriving]
}

function blendTicks<T>(
  from: readonly TickMark<T>[],
  to: readonly TickMark<T>[],
  keyOf: (tick: T) => string,
  reach: number,
): TickMark<T>[] {
  const toKeys = new Set(to.map((mark) => keyOf(mark.tick)))
  const fromByKey = new Map(from.map((mark) => [keyOf(mark.tick), mark]))
  const fadeOut = Math.min(1, reach * 2)
  const fadeIn = Math.max(0, reach * 2 - 1)
  return [
    ...from
      .filter((mark) => !toKeys.has(keyOf(mark.tick)))
      .map((mark) => ({ tick: mark.tick, opacity: lerp(mark.opacity, 0, fadeOut) })),
    ...to.map((mark) => {
      const shown = fromByKey.get(keyOf(mark.tick))
      return {
        tick: mark.tick,
        opacity:
          shown === undefined
            ? lerp(0, mark.opacity, fadeIn)
            : lerp(shown.opacity, mark.opacity, reach),
      }
    }),
  ]
}

export function blendPlots(from: Plot, to: Plot, progress: number): Plot {
  const timeShift = from.time.origin - to.time.origin
  const priceShift = Number(from.price.origin - to.price.origin)
  const forward = {
    x: relate(from.time, to.time, timeShift),
    y: relate(from.price, to.price, priceShift),
  }
  const backward = { x: invert(forward.x), y: invert(forward.y) }
  const fromLine = from.line ?? (to.line === null ? null : hiddenLine(to.line, backward.y))
  const toLine = to.line ?? (from.line === null ? null : hiddenLine(from.line, forward.y))
  const pace = {
    progress,
    x: zoomReach(from.time, to.time, progress),
    y: zoomReach(from.price, to.price, progress),
  }

  return {
    bounds: {
      left: lerp(from.bounds.left, to.bounds.left, progress),
      right: lerp(from.bounds.right, to.bounds.right, progress),
      top: lerp(from.bounds.top, to.bounds.top, progress),
      bottom: lerp(from.bounds.bottom, to.bounds.bottom, progress),
    },
    time: blendScale(from.time, to.time, timeShift, pace.x),
    price: blendScale(from.price, to.price, priceShift, pace.y),
    priceTicks: blendTicks(from.priceTicks, to.priceTicks, (tick) => tick.price.toString(), pace.y),
    timeTicks: blendTicks(from.timeTicks, to.timeTicks, (tick) => String(tick.unixSecs), pace.x),
    line: fromLine === null || toLine === null ? null : blendLines(fromLine, toLine, pace),
    candles: blendCandles(from, to, forward, backward, pace),
  }
}
