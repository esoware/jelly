import { useQuery } from "@tanstack/react-query"
import { Fragment, useId, useLayoutEffect, useRef, useState } from "react"
import type { PointerEvent, ReactNode } from "react"

import {
  AXIS_GAP,
  blendPlots,
  drawPlot,
  placePrice,
  placeTime,
} from "@/features/wallet/price-plot.ts"
import type { CandleMark, LineMark, Plot } from "@/features/wallet/price-plot.ts"
import {
  CHART_RANGES,
  chartCandles,
  chartPoints,
  coordinate,
  formatChange,
  formatChartTime,
  isRisingChange,
  nearestIndex,
  readChart,
  seriesChange,
} from "@/features/wallet/price-scale.ts"
import type {
  Candle,
  Chart,
  ChartMode,
  ChartRange,
  ChartSeries,
  Spot,
} from "@/features/wallet/price-scale.ts"
import type { AssetId, CandleInterval } from "@/ipc/bindings.gen.ts"
import { assetKey } from "@/lib/chain.ts"
import { formatFiat } from "@/lib/fiat.ts"
import { NONE, unixSecondsOf } from "@/lib/format.ts"
import { moneyQueries } from "@/query/money.ts"
import { useFiatCurrency, useIsFiatStale, usePrice } from "@/store.ts"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Panel, PanelHeader } from "@/ui/panel.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { mixColors, useMorph } from "@/ui/use-morph.ts"
import { FiatValue } from "@/widgets/amounts.tsx"

const RISING_COLOR = "var(--color-ok)"
const FALLING_COLOR = "var(--color-danger)"

function riseColor(rise: number): string {
  return mixColors(FALLING_COLOR, RISING_COLOR, rise)
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
  plot: Plot
  clipPath: string
}

function ChartAxes({ plot, clipPath }: ChartAxesProps) {
  const { bounds } = plot
  return (
    <g className="text-caption tabular-nums">
      <g clipPath={clipPath}>
        {plot.priceTicks.map(({ tick, opacity }) => {
          const y = placePrice(plot.price, tick.price)
          return (
            <line
              key={tick.price.toString()}
              x1={bounds.left}
              x2={bounds.right}
              y1={y}
              y2={y}
              opacity={opacity}
              className="stroke-edge/50"
            />
          )
        })}
      </g>
      <line
        x1={bounds.left}
        x2={bounds.left}
        y1={bounds.top}
        y2={bounds.bottom}
        className="stroke-edge"
      />
      <g textAnchor="end" className="fill-fg-faint">
        {plot.priceTicks.map(({ tick, opacity }) => (
          <text
            key={tick.price.toString()}
            x={bounds.left - AXIS_GAP}
            y={placePrice(plot.price, tick.price)}
            opacity={opacity}
            dominantBaseline="central"
          >
            {tick.label}
          </text>
        ))}
      </g>
      <g textAnchor="middle" className="fill-fg-faint">
        {plot.timeTicks.map(({ tick, opacity }) => (
          <text
            key={tick.unixSecs}
            x={placeTime(plot.time, tick.unixSecs)}
            y={bounds.bottom + AXIS_GAP}
            opacity={opacity}
            dominantBaseline="hanging"
          >
            {tick.label}
          </text>
        ))}
      </g>
    </g>
  )
}

const BODY_RADIUS = 1

interface CandleMarksProps {
  candles: readonly CandleMark[]
  hoveredKey: string | null
}

function CandleMarks({ candles, hoveredKey }: CandleMarksProps) {
  return candles.map((candle) => (
    <g
      key={candle.key}
      data-dimmed={hoveredKey !== null && hoveredKey !== candle.key}
      className="transition-opacity data-[dimmed=true]:opacity-40"
    >
      <g opacity={candle.opacity} style={{ color: riseColor(candle.rise) }}>
        <line
          x1={candle.x}
          x2={candle.x}
          y1={candle.highY}
          y2={candle.lowY}
          shapeRendering="crispEdges"
          className="stroke-current"
        />
        <rect
          x={candle.x - candle.width / 2}
          y={Math.min(candle.openY, candle.closeY)}
          width={candle.width}
          height={Math.max(1, Math.abs(candle.openY - candle.closeY))}
          rx={BODY_RADIUS}
          className="fill-current"
        />
      </g>
    </g>
  ))
}

const LINE_WIDTH = 1.5
const AREA_OPACITY = 0.24
const DOT_RADIUS = 3
const HALO_RADIUS = 7

interface LineMarksProps {
  line: LineMark
  plot: Plot
  hoveredSpot: Spot | undefined
}

function LineMarks({ line, plot, hoveredSpot }: LineMarksProps) {
  const baseId = useId()
  const fillId = `${baseId}-fill`
  const pastId = `${baseId}-past`
  const futureId = `${baseId}-future`
  const { bounds } = plot
  const spots = line.vertices.map((vertex) => ({
    x: placeTime(plot.time, vertex.unixSecs),
    y: vertex.y,
  }))
  const path = spots
    .map((spot, i) => `${i === 0 ? "M" : "L"}${coordinate(spot.x)} ${coordinate(spot.y)}`)
    .join("")
  const floor = coordinate(bounds.bottom)
  const area = `${path}L${coordinate(spots.at(-1)?.x ?? bounds.right)} ${floor}L${coordinate(spots.at(0)?.x ?? bounds.left)} ${floor}Z`

  const series = (
    <>
      <path d={area} fill={`url(#${fillId})`} />
      <path
        d={path}
        strokeWidth={LINE_WIDTH}
        strokeLinejoin="round"
        strokeLinecap="round"
        className="fill-none stroke-current"
      />
    </>
  )

  return (
    <g opacity={line.opacity} style={{ color: riseColor(line.rise) }}>
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

interface ChartPlotProps {
  series: ChartSeries
  interval: CandleInterval
  width: number
  height: number
}

function ChartPlot({ series, interval, width, height }: ChartPlotProps) {
  const labelsRef = useRef<SVGGElement>(null)
  const clipId = useId()
  const [labelWidth, setLabelWidth] = useState(0)
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  const plotSize = { width, height, labelWidth }
  const target = drawPlot(series, plotSize)
  const plot = useMorph(series, (source) => drawPlot(source, plotSize), blendPlots)
  const labels = target.priceTicks.map((mark) => mark.tick.label)

  useLayoutEffect(() => {
    const group = labelsRef.current
    setLabelWidth(group === null || labels.length === 0 ? 0 : Math.ceil(group.getBBox().width))
  }, [labels])

  const spots =
    target.line === null
      ? target.candles.map((candle) => ({ x: candle.x, y: candle.closeY }))
      : target.line.vertices.map((vertex) => ({
          x: placeTime(target.time, vertex.unixSecs),
          y: vertex.y,
        }))
  const hoveredSpot = hoveredIndex === null ? undefined : spots[hoveredIndex]
  const hoveredKey = hoveredIndex === null ? null : (target.candles[hoveredIndex]?.key ?? null)
  const clipPath = `url(#${clipId})`

  function readout(spot: Spot, i: number): ReactNode {
    if (series.mode === "line") {
      const point = series.points[i]
      return point === undefined ? null : (
        <ChartReadout x={spot.x} width={width}>
          <p className="font-medium tabular-nums">
            {formatFiat(point.price, series.currency, { isPrice: true })}
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
                {formatFiat(pick(candle), series.currency, { isPrice: true })}
              </dd>
            </Fragment>
          ))}
        </dl>
      </ChartReadout>
    )
  }

  return (
    <>
      <svg
        width={width}
        height={height}
        className="block"
        onPointerMove={(event) => {
          const x = pointerX(event)
          setHoveredIndex(x < target.bounds.left ? null : nearestIndex(spots, x))
        }}
        onPointerLeave={() => {
          setHoveredIndex(null)
        }}
      >
        <defs>
          <clipPath id={clipId}>
            <rect
              x={plot.bounds.left}
              y={0}
              width={Math.max(0, width - plot.bounds.left)}
              height={plot.bounds.bottom + LINE_WIDTH}
            />
          </clipPath>
        </defs>

        <ChartAxes plot={plot} clipPath={clipPath} />

        <g ref={labelsRef} className="invisible text-caption tabular-nums">
          {labels.map((label) => (
            <text key={label}>{label}</text>
          ))}
        </g>

        {hoveredSpot === undefined ? null : (
          <line
            x1={hoveredSpot.x}
            x2={hoveredSpot.x}
            y1={target.bounds.top}
            y2={target.bounds.bottom}
            className="stroke-edge"
          />
        )}

        <g clipPath={clipPath}>
          {plot.line === null ? null : (
            <LineMarks line={plot.line} plot={plot} hoveredSpot={hoveredSpot} />
          )}
          <CandleMarks candles={plot.candles} hoveredKey={hoveredKey} />
        </g>
      </svg>

      {hoveredIndex === null || hoveredSpot === undefined
        ? null
        : readout(hoveredSpot, hoveredIndex)}
    </>
  )
}

interface PriceChartProps {
  series: ChartSeries
  interval: CandleInterval
}

function PriceChart({ series, interval }: PriceChartProps) {
  const frameRef = useRef<HTMLDivElement>(null)
  const [size, setSize] = useState<Size | null>(null)

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

  return (
    <div ref={frameRef} className="relative size-full">
      {size === null ? null : (
        <ChartPlot series={series} interval={interval} width={size.width} height={size.height} />
      )}
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

interface ShownChart {
  asset: string
  chart: Chart
  range: ChartRange
  nowUnixSecs: number
}

function isSameChart(kept: ShownChart | null, loaded: ShownChart): boolean {
  return (
    kept !== null &&
    kept.asset === loaded.asset &&
    kept.chart === loaded.chart &&
    kept.range === loaded.range &&
    kept.nowUnixSecs === loaded.nowUnixSecs
  )
}

interface PricePanelProps {
  asset: AssetId
}

export function PricePanel({ asset }: PricePanelProps) {
  const currency = useFiatCurrency()
  const livePrice = usePrice(asset)
  const isStale = useIsFiatStale(asset)
  const [range, setRange] = useState<ChartRange>("1d")
  const [mode, setMode] = useState<ChartMode>("line")
  const ranged = useQuery({
    ...moneyQueries.priceChart(asset, CHART_RANGES[range].interval, currency),
    select: readChart,
  })
  const loaded =
    ranged.data === undefined
      ? null
      : {
          asset: assetKey(asset),
          chart: ranged.data,
          range,
          nowUnixSecs: unixSecondsOf(ranged.dataUpdatedAt),
        }
  const [kept, setKept] = useState(loaded)
  if (loaded !== null && !isSameChart(kept, loaded)) {
    setKept(loaded)
  }
  const shown =
    kept?.asset !== assetKey(asset) || (ranged.isError && ranged.data === undefined) ? null : kept

  const live = livePrice === null || isStale ? null : { price: livePrice, currency }
  const points =
    shown === null ? [] : chartPoints(shown.chart, shown.range, live, shown.nowUnixSecs)
  const rangeChange = seriesChange(points)

  function chartArea(): ReactNode {
    if (shown === null && !ranged.isError) {
      return <Skeleton className="size-full" />
    }
    if (shown === null || rangeChange === null) {
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
          mode === "line"
            ? { mode, currency: shown.chart.currency, points }
            : chartCandles(shown.chart, shown.range, live, shown.nowUnixSecs)
        }
        interval={CHART_RANGES[shown.range].interval}
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
