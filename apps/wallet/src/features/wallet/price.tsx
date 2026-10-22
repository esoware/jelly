import { useQuery } from "@tanstack/react-query"
import { Fragment, useId, useLayoutEffect, useRef, useState } from "react"
import type { PointerEvent, ReactNode, RefObject } from "react"

import {
  AXIS_GAP,
  CHART_RANGES,
  chartCandles,
  chartPoints,
  coordinate,
  formatChange,
  formatChartTime,
  isRisingChange,
  nearestIndex,
  priceTicks,
  readChart,
  seriesChange,
  seriesDomain,
  seriesExtent,
  timeTicks,
  xOf,
  yOf,
} from "@/features/wallet/price-scale.ts"
import type {
  Bounds,
  Candle,
  ChartMode,
  ChartRange,
  ChartSeries,
  PositionedTick,
  Spot,
} from "@/features/wallet/price-scale.ts"
import type { AssetId, CandleInterval, FiatCurrency } from "@/ipc/bindings.gen.ts"
import { formatFiat } from "@/lib/fiat.ts"
import { NONE, unixSecondsOf } from "@/lib/format.ts"
import { moneyQueries } from "@/query/money.ts"
import { useFiatCurrency, useIsFiatStale, usePrice } from "@/store.ts"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Panel, PanelHeader } from "@/ui/panel.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { FiatValue } from "@/widgets/amounts.tsx"

function useLabelWidth(labels: readonly string[]): [RefObject<SVGGElement | null>, number] {
  const labelsRef = useRef<SVGGElement>(null)
  const [width, setWidth] = useState(0)

  useLayoutEffect(() => {
    const group = labelsRef.current
    setWidth(group === null || labels.length === 0 ? 0 : Math.ceil(group.getBBox().width))
  }, [labels])

  return [labelsRef, width]
}

interface ChartReadoutProps {
  x: number
  width: number
  children: ReactNode
}

function ChartReadout({ x, width, children }: ChartReadoutProps) {
  return (
    <div
      data-side={x > width / 2 ? "start" : "end"}
      style={{ left: x }}
      className="pointer-events-none absolute top-0 rounded-md overlay-sm px-2 py-1 text-caption whitespace-nowrap data-[side=end]:ml-3 data-[side=start]:-ml-3 data-[side=start]:-translate-x-full"
    >
      {children}
    </div>
  )
}

interface ChartAxesProps {
  bounds: Bounds
  priceAxis: readonly PositionedTick[]
  timeAxis: readonly PositionedTick[]
  priceLabelsRef: RefObject<SVGGElement | null>
}

function ChartAxes({ bounds, priceAxis, timeAxis, priceLabelsRef }: ChartAxesProps) {
  return (
    <g className="text-caption tabular-nums">
      {priceAxis.map((tick) => (
        <line
          key={tick.label}
          x1={bounds.left}
          x2={bounds.right}
          y1={tick.at}
          y2={tick.at}
          className="stroke-edge/50"
        />
      ))}
      <line
        x1={bounds.left}
        x2={bounds.left}
        y1={bounds.top}
        y2={bounds.bottom}
        className="stroke-edge"
      />
      <g ref={priceLabelsRef} textAnchor="end" className="fill-fg-faint">
        {priceAxis.map((tick) => (
          <text key={tick.label} x={bounds.left - AXIS_GAP} y={tick.at} dominantBaseline="central">
            {tick.label}
          </text>
        ))}
      </g>
      <g textAnchor="middle" className="fill-fg-faint">
        {timeAxis.map((tick) => (
          <text key={tick.at} x={tick.at} y={bounds.bottom + AXIS_GAP} dominantBaseline="hanging">
            {tick.label}
          </text>
        ))}
      </g>
    </g>
  )
}

const BODY_RADIUS = 1

interface CandleMarksProps {
  candles: readonly Candle[]
  spots: readonly Spot[]
  bodyWidth: number
  toY: (price: bigint) => number
  hoveredIndex: number | null
}

function CandleMarks({ candles, spots, bodyWidth, toY, hoveredIndex }: CandleMarksProps) {
  return candles.map((candle, i) => {
    const x = spots[i]?.x ?? 0
    const openY = toY(candle.open)
    const closeY = toY(candle.close)
    return (
      <g
        key={candle.openUnixSecs}
        data-tone={candle.close >= candle.open ? "rising" : "falling"}
        data-dimmed={hoveredIndex !== null && hoveredIndex !== i}
        className="transition-opacity data-[dimmed=true]:opacity-40 data-[tone=falling]:text-danger data-[tone=rising]:text-ok"
      >
        <line
          x1={x}
          x2={x}
          y1={toY(candle.high)}
          y2={toY(candle.low)}
          shapeRendering="crispEdges"
          className="stroke-current"
        />
        <rect
          x={x - bodyWidth / 2}
          y={Math.min(openY, closeY)}
          width={bodyWidth}
          height={Math.max(1, Math.abs(openY - closeY))}
          rx={BODY_RADIUS}
          className="fill-current"
        />
      </g>
    )
  })
}

const LINE_WIDTH = 1.5
const AREA_OPACITY = 0.24
const DOT_RADIUS = 3
const HALO_RADIUS = 7

interface LineMarksProps {
  spots: readonly Spot[]
  bounds: Bounds
  isRising: boolean
  hoveredSpot: Spot | undefined
}

function LineMarks({ spots, bounds, isRising, hoveredSpot }: LineMarksProps) {
  const baseId = useId()
  const fillId = `${baseId}-fill`
  const pastId = `${baseId}-past`
  const futureId = `${baseId}-future`
  const line = spots
    .map((spot, i) => `${i === 0 ? "M" : "L"}${coordinate(spot.x)} ${coordinate(spot.y)}`)
    .join("")
  const floor = coordinate(bounds.bottom)
  const area = `${line}L${coordinate(spots.at(-1)?.x ?? bounds.right)} ${floor}L${coordinate(spots.at(0)?.x ?? bounds.left)} ${floor}Z`

  const series = (
    <>
      <path d={area} fill={`url(#${fillId})`} />
      <path
        d={line}
        strokeWidth={LINE_WIDTH}
        strokeLinejoin="round"
        strokeLinecap="round"
        className="fill-none stroke-current"
      />
    </>
  )

  return (
    <g
      data-tone={isRising ? "rising" : "falling"}
      className="data-[tone=falling]:text-danger data-[tone=rising]:text-ok"
    >
      <defs>
        <linearGradient id={fillId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="currentColor" stopOpacity={AREA_OPACITY} />
          <stop offset="100%" stopColor="currentColor" stopOpacity={0} />
        </linearGradient>
        {hoveredSpot === undefined ? null : (
          <>
            <clipPath id={pastId}>
              <rect x={0} y={0} width={hoveredSpot.x} height={bounds.bottom} />
            </clipPath>
            <clipPath id={futureId}>
              <rect x={hoveredSpot.x} y={0} width={bounds.right} height={bounds.bottom} />
            </clipPath>
          </>
        )}
      </defs>

      {hoveredSpot === undefined ? (
        series
      ) : (
        <>
          <g clipPath={`url(#${pastId})`}>{series}</g>
          <g clipPath={`url(#${futureId})`} className="opacity-40">
            {series}
          </g>
          <circle
            cx={hoveredSpot.x}
            cy={hoveredSpot.y}
            r={HALO_RADIUS}
            className="fill-current opacity-25"
          />
          <circle cx={hoveredSpot.x} cy={hoveredSpot.y} r={DOT_RADIUS} className="fill-current" />
        </>
      )}
    </g>
  )
}

const INSET_TOP = 8
const INSET_RIGHT = 4
const TIME_AXIS_HEIGHT = 20
const EDGE_ROOM = 16
const MIN_PRICE_TICK_GAP = 28
const MIN_TIME_TICK_GAP = 72
const BODY_SHARE = 0.6
const MAX_BODY_WIDTH = 12

const OHLC_ROWS = [
  { label: "Open", pick: (candle: Candle) => candle.open },
  { label: "High", pick: (candle: Candle) => candle.high },
  { label: "Low", pick: (candle: Candle) => candle.low },
  { label: "Close", pick: (candle: Candle) => candle.close },
] as const

interface Size {
  width: number
  height: number
}

function pointerX(event: PointerEvent<SVGSVGElement>): number {
  return event.clientX - event.currentTarget.getBoundingClientRect().left
}

interface PriceChartProps {
  series: ChartSeries
  isRising: boolean
  interval: CandleInterval
  currency: FiatCurrency
}

function PriceChart({ series, isRising, interval, currency }: PriceChartProps) {
  const frameRef = useRef<HTMLDivElement>(null)
  const [size, setSize] = useState<Size | null>(null)
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  useLayoutEffect(() => {
    const frame = frameRef.current
    if (frame === null) {
      return undefined
    }
    function measure(): void {
      if (frame !== null) {
        setSize({ width: frame.clientWidth, height: frame.clientHeight })
      }
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(frame)
    return () => {
      observer.disconnect()
    }
  }, [])

  const extent = seriesExtent(series)
  const plotHeight = size === null ? 0 : size.height - INSET_TOP - TIME_AXIS_HEIGHT
  const priceAxisTicks =
    size === null
      ? []
      : priceTicks(extent.low, extent.high, Math.floor(plotHeight / MIN_PRICE_TICK_GAP), currency)
  const [priceLabelsRef, priceLabelWidth] = useLabelWidth(priceAxisTicks.map((tick) => tick.label))

  if (size === null) {
    return <div ref={frameRef} className="relative size-full" />
  }

  const { width, height } = size
  const bounds: Bounds = {
    left: priceLabelWidth + AXIS_GAP,
    right: width - INSET_RIGHT,
    top: INSET_TOP,
    bottom: height - TIME_AXIS_HEIGHT,
  }
  const domain = seriesDomain(series)
  function toY(price: bigint): number {
    return yOf(price, extent, bounds)
  }
  const spots =
    series.mode === "line"
      ? series.points.map((point) => ({
          x: xOf(point.unixSecs, domain, bounds),
          y: toY(point.price),
        }))
      : series.candles.map((candle) => ({
          x: xOf(candle.openUnixSecs + series.bucketSeconds / 2, domain, bounds),
          y: toY(candle.close),
        }))
  const timeAxisTicks = timeTicks(
    domain.startUnixSecs,
    domain.endUnixSecs,
    Math.floor((bounds.right - bounds.left) / MIN_TIME_TICK_GAP),
  )
    .map((tick) => ({ at: xOf(tick.unixSecs, domain, bounds), label: tick.label }))
    .filter((tick) => tick.at >= bounds.left + EDGE_ROOM && tick.at <= bounds.right - EDGE_ROOM)
  const hoveredSpot = hoveredIndex === null ? undefined : spots[hoveredIndex]

  function readout(spot: Spot, i: number): ReactNode {
    if (series.mode === "line") {
      const point = series.points[i]
      return point === undefined ? null : (
        <ChartReadout x={spot.x} width={width}>
          <p className="font-medium tabular-nums">
            {formatFiat(point.price, currency, { isPrice: true })}
          </p>
          <p className="text-fg-muted">{formatChartTime(point.labelUnixSecs, interval)}</p>
        </ChartReadout>
      )
    }
    const candle = series.candles[i]
    return candle === undefined ? null : (
      <ChartReadout x={spot.x} width={width}>
        <p className="text-fg-muted">{formatChartTime(candle.openUnixSecs, interval)}</p>
        <dl className="grid grid-cols-[auto_auto] gap-x-3 tabular-nums">
          {OHLC_ROWS.map(({ label, pick }) => (
            <Fragment key={label}>
              <dt className="text-fg-faint">{label}</dt>
              <dd className="text-right">
                {formatFiat(pick(candle), currency, { isPrice: true })}
              </dd>
            </Fragment>
          ))}
        </dl>
      </ChartReadout>
    )
  }

  return (
    <div ref={frameRef} className="relative size-full">
      <svg
        width={width}
        height={height}
        className="block"
        onPointerMove={(event) => {
          const x = pointerX(event)
          setHoveredIndex(x < bounds.left ? null : nearestIndex(spots, x))
        }}
        onPointerLeave={() => {
          setHoveredIndex(null)
        }}
      >
        <ChartAxes
          bounds={bounds}
          priceAxis={priceAxisTicks.map((tick) => ({ at: toY(tick.price), label: tick.label }))}
          timeAxis={timeAxisTicks}
          priceLabelsRef={priceLabelsRef}
        />

        {hoveredSpot === undefined ? null : (
          <line
            x1={hoveredSpot.x}
            x2={hoveredSpot.x}
            y1={bounds.top}
            y2={bounds.bottom}
            className="stroke-edge"
          />
        )}

        {series.mode === "line" ? (
          <LineMarks spots={spots} bounds={bounds} isRising={isRising} hoveredSpot={hoveredSpot} />
        ) : (
          <CandleMarks
            candles={series.candles}
            spots={spots}
            bodyWidth={Math.min(
              MAX_BODY_WIDTH,
              Math.max(
                1,
                ((bounds.right - bounds.left) * series.bucketSeconds * BODY_SHARE) /
                  (domain.endUnixSecs - domain.startUnixSecs),
              ),
            )}
            toY={toY}
            hoveredIndex={hoveredIndex}
          />
        )}
      </svg>

      {hoveredIndex === null || hoveredSpot === undefined
        ? null
        : readout(hoveredSpot, hoveredIndex)}
    </div>
  )
}

const MODES = [
  { value: "line", label: "Line" },
  { value: "candles", label: "Candles" },
] as const satisfies readonly { value: ChartMode; label: string }[]

const RANGES = [
  { value: "1d", label: "1D" },
  { value: "1w", label: "1W" },
  { value: "1m", label: "1M" },
  { value: "3m", label: "3M" },
  { value: "6m", label: "6M" },
  { value: "1y", label: "1Y" },
  { value: "max", label: "Max" },
] as const satisfies readonly { value: ChartRange; label: string }[]

interface PricePanelProps {
  asset: AssetId
}

export function PricePanel({ asset }: PricePanelProps) {
  const currency = useFiatCurrency()
  const livePrice = usePrice(asset)
  const isStale = useIsFiatStale(asset)
  const [range, setRange] = useState<ChartRange>("1d")
  const [mode, setMode] = useState<ChartMode>("line")
  const { interval } = CHART_RANGES[range]
  const ranged = useQuery({
    ...moneyQueries.priceChart(asset, interval, currency),
    select: readChart,
  })

  const live = livePrice === null || isStale ? null : { price: livePrice, currency }
  const nowUnixSecs = unixSecondsOf(ranged.dataUpdatedAt)
  const points = ranged.data === undefined ? [] : chartPoints(ranged.data, range, live, nowUnixSecs)
  const rangeChange = seriesChange(points)

  function chartArea(): ReactNode {
    if (ranged.data === undefined && !ranged.isError) {
      return <Skeleton className="size-full" />
    }
    if (ranged.data === undefined || rangeChange === null) {
      return (
        <EmptyState
          isCompact
          title="Price history is unavailable"
          description="Jelly will try again in a few minutes."
          className="size-full"
        />
      )
    }
    return (
      <PriceChart
        series={
          mode === "line" ? { mode, points } : chartCandles(ranged.data, range, live, nowUnixSecs)
        }
        isRising={isRisingChange(rangeChange)}
        interval={interval}
        currency={ranged.data.currency}
      />
    )
  }

  return (
    <Panel className="h-60 shrink-0">
      <PanelHeader actions={<Segmented value={mode} onValueChange={setMode} items={MODES} />}>
        <div className="flex min-w-0 pl-2">
          {livePrice === null ? (
            <span className="text-title text-fg-faint">{NONE}</span>
          ) : (
            <FiatValue
              value={livePrice}
              currency={currency}
              isStale={isStale}
              isPrice
              className="truncate text-title"
            />
          )}
        </div>
      </PanelHeader>

      <div className="min-h-0 flex-1 px-2">{chartArea()}</div>

      <div className="flex shrink-0 items-center gap-2 p-2">
        <Segmented value={range} onValueChange={setRange} items={RANGES} />
        {rangeChange === null ? null : (
          <span
            className={`ml-auto pr-2 text-caption font-medium tabular-nums ${isRisingChange(rangeChange) ? "text-ok" : "text-danger"}`}
          >
            {formatChange(rangeChange)}
          </span>
        )}
      </div>
    </Panel>
  )
}
