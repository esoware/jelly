import { useId } from "react"
import type { ReactNode } from "react"

import { assetKey } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { sliceWeights } from "@/lib/portfolio.ts"
import type { Allocation } from "@/lib/portfolio.ts"
import { lerp, mixColors, useMorph } from "@/ui/use-morph.ts"
import { assetBrand } from "@/widgets/logos.tsx"

const VIEW = 100
const CENTER = VIEW / 2
const FULL_TURN = 2 * Math.PI
const COORDINATE_DIGITS = 2
const HIT_SLOP = 4
const CORNER_SHARE = 0.8

interface Geometry {
  thickness: number
  gap: number
  corner: number
}

const GEOMETRIES = {
  pie: { thickness: CENTER, gap: 7, corner: 0 },
  sm: { thickness: 22, gap: 5, corner: 0 },
  lg: { thickness: 13, gap: 1.6, corner: 1.8 },
} as const satisfies Record<string, Geometry>

export interface DonutSlice {
  id: string
  weight: number
  color: string
  gradient?: readonly [string, string] | undefined
}

interface Ring {
  inner: number
  outer: number
}

interface Sector {
  slice: DonutSlice
  start: number
  end: number
}

function polar(radius: number, angle: number): string {
  const x = CENTER + radius * Math.sin(angle)
  const y = CENTER - radius * Math.cos(angle)
  return `${x.toFixed(COORDINATE_DIGITS)} ${y.toFixed(COORDINATE_DIGITS)}`
}

function arc(radius: number, from: number, to: number, isClockwise: boolean): string {
  const isLarge = Math.abs(to - from) > Math.PI
  const flags = `0 ${isLarge ? "1" : "0"} ${isClockwise ? "1" : "0"}`
  return `A${String(radius)} ${String(radius)} ${flags} ${polar(radius, to)}`
}

function sectorPath(start: number, end: number, ring: Ring, inset: number): string | null {
  const outerTrim = Math.asin(Math.min(1, inset / ring.outer))
  const outerStart = start + outerTrim
  const outerEnd = end - outerTrim
  if (outerEnd <= outerStart) {
    return null
  }
  const head = `M${polar(ring.outer, outerStart)}${arc(ring.outer, outerStart, outerEnd, true)}`

  if (inset < ring.inner) {
    const innerTrim = Math.asin(inset / ring.inner)
    const innerStart = start + innerTrim
    const innerEnd = end - innerTrim
    if (innerEnd > innerStart) {
      const tail = arc(ring.inner, innerEnd, innerStart, false)
      return `${head}L${polar(ring.inner, innerEnd)}${tail}Z`
    }
  }

  const half = (end - start) / 2
  const apex = inset / Math.sin(half)
  return apex < ring.outer ? `${head}L${polar(apex, start + half)}Z` : null
}

function layoutSectors(slices: readonly DonutSlice[], geometry: Geometry): Sector[] {
  const inner = CENTER - geometry.thickness
  const visibleSlices = slices.filter((slice) => slice.weight > 0)
  const minSweep =
    inner > 0
      ? Math.min(2 * Math.asin(Math.min(1, geometry.gap / inner)), FULL_TURN / visibleSlices.length)
      : 0
  const raisedSlices = new Set<DonutSlice>()
  let freeTurn = FULL_TURN
  let freeWeight = visibleSlices.reduce((total, slice) => total + slice.weight, 0)
  for (const slice of visibleSlices.toSorted((a, b) => a.weight - b.weight)) {
    if (slice.weight * (freeTurn / freeWeight) >= minSweep) {
      break
    }
    raisedSlices.add(slice)
    freeTurn -= minSweep
    freeWeight -= slice.weight
  }
  const scale = freeTurn / freeWeight
  const sectors: Sector[] = []
  let angle = 0
  for (const slice of visibleSlices) {
    const end = angle + (raisedSlices.has(slice) ? minSweep : slice.weight * scale)
    sectors.push({ slice, start: angle, end })
    angle = end
  }
  return sectors
}

function blendSectors(from: readonly Sector[], to: readonly Sector[], progress: number): Sector[] {
  const fromById = new Map(from.map((sector) => [sector.slice.id, sector]))
  const toById = new Map(to.map((sector) => [sector.slice.id, sector]))
  const blended: Sector[] = []
  let toEnd = 0
  for (const sector of from) {
    const match = toById.get(sector.slice.id)
    if (match === undefined) {
      blended.push({
        slice: sector.slice,
        start: lerp(sector.start, toEnd, progress),
        end: lerp(sector.end, toEnd, progress),
      })
    } else {
      toEnd = match.end
    }
  }
  let fromEnd = 0
  for (const sector of to) {
    const match = fromById.get(sector.slice.id)
    const fromStart = match?.start ?? fromEnd
    fromEnd = match?.end ?? fromEnd
    blended.push({
      slice:
        match === undefined
          ? sector.slice
          : { ...sector.slice, color: mixColors(match.slice.color, sector.slice.color, progress) },
      start: lerp(fromStart, sector.start, progress),
      end: lerp(fromEnd, sector.end, progress),
    })
  }
  return blended
}

interface DonutProps {
  slices: readonly DonutSlice[]
  size?: keyof typeof GEOMETRIES | undefined
  activeId?: string | null | undefined
  onActiveChange?: ((id: string | null) => void) | undefined
  onSelect?: ((id: string) => void) | undefined
  className?: string | undefined
  children?: ReactNode
}

export function Donut({
  slices,
  size = "lg",
  activeId = null,
  onActiveChange,
  onSelect,
  className,
  children,
}: DonutProps) {
  const baseId = useId()
  const geometry: Geometry = GEOMETRIES[size]
  const inner = CENTER - geometry.thickness
  const middle = CENTER - geometry.thickness / 2
  const sectors = layoutSectors(slices, geometry)
  const shownSectors = useMorph(
    slices,
    (source) => layoutSectors(source, geometry),
    blendSectors,
  ).filter((sector) => sector.end > sector.start)
  const isWhole = sectors.length === 1
  const isShownWhole = shownSectors.length === 1
  const reach = { inner: inner - HIT_SLOP, outer: CENTER }

  function gradientId(slice: DonutSlice): string {
    return `${baseId}-${slice.id}`
  }

  return (
    <div className={appendClassName("relative shrink-0", className)}>
      <svg
        viewBox={`0 0 ${String(VIEW)} ${String(VIEW)}`}
        className="size-full animate-sweep mask-[conic-gradient(#000_var(--sweep),transparent_0)] motion-reduce:animate-none"
        onPointerLeave={() => {
          onActiveChange?.(null)
        }}
      >
        <defs>
          {shownSectors.map(({ slice }) =>
            slice.gradient === undefined ? null : (
              <linearGradient key={slice.id} id={gradientId(slice)} x1="0" y1="1" x2="1" y2="0">
                <stop offset="0%" stopColor={slice.gradient[0]} />
                <stop offset="100%" stopColor={slice.gradient[1]} />
              </linearGradient>
            ),
          )}
        </defs>

        {sectors.length === 0 ? (
          <circle
            cx={CENTER}
            cy={CENTER}
            r={middle}
            strokeWidth={geometry.thickness}
            className="fill-none stroke-surface"
          />
        ) : null}

        {shownSectors.map(({ slice, start, end }) => {
          const paint = slice.gradient === undefined ? slice.color : `url(#${gradientId(slice)})`
          const isDimmed = activeId !== null && activeId !== slice.id
          if (isShownWhole) {
            return (
              <circle
                key={slice.id}
                cx={CENTER}
                cy={CENTER}
                r={middle}
                fill="none"
                stroke={paint}
                strokeWidth={geometry.thickness}
                data-dimmed={isDimmed}
                className="transition-opacity data-[dimmed=true]:opacity-40"
              />
            )
          }
          const sine = Math.sin(Math.min((end - start) / 2, Math.PI / 2))
          const gap = Math.min(geometry.gap, CENTER * sine)
          const outerRoom = (CENTER * sine - gap / 2) / (1 + sine)
          const innerRoom = inner > 0 ? (inner * sine - gap / 2) / (1 - sine) : outerRoom
          const room = Math.min(outerRoom, innerRoom)
          const corner = Math.max(0, Math.min(geometry.corner, room * CORNER_SHARE))
          const shrunk = { inner: inner + corner, outer: CENTER - corner }
          const d = sectorPath(start, end, shrunk, gap / 2 + corner)
          return d === null ? null : (
            <path
              key={slice.id}
              d={d}
              fill={paint}
              stroke={paint}
              strokeWidth={corner * 2}
              strokeLinejoin="round"
              data-dimmed={isDimmed}
              className="transition-opacity data-[dimmed=true]:opacity-40"
            />
          )
        })}

        {onActiveChange === undefined ? null : (
          <>
            <circle
              cx={CENTER}
              cy={CENTER}
              r={reach.inner}
              className="fill-transparent"
              onPointerEnter={() => {
                onActiveChange(null)
              }}
            />
            {sectors.map(({ slice, start, end }) => {
              const handlers = {
                onPointerEnter: () => {
                  onActiveChange(slice.id)
                },
                onClick: () => {
                  onSelect?.(slice.id)
                },
              }
              if (isWhole) {
                return (
                  <circle
                    key={slice.id}
                    cx={CENTER}
                    cy={CENTER}
                    r={middle}
                    strokeWidth={geometry.thickness + HIT_SLOP}
                    className="fill-none stroke-transparent"
                    {...handlers}
                  />
                )
              }
              const d = sectorPath(start, end, reach, 0)
              return d === null ? null : (
                <path key={slice.id} d={d} className="fill-transparent" {...handlers} />
              )
            })}
          </>
        )}
      </svg>
      {children === undefined ? null : (
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center text-center">
          {children}
        </div>
      )}
    </div>
  )
}

export function allocationSlices(allocation: Allocation): DonutSlice[] {
  return sliceWeights(allocation).map(({ asset, weight }) => {
    const brand = assetBrand(asset)
    return {
      id: assetKey(asset),
      weight,
      color: brand.color,
      gradient: brand.gradient,
    }
  })
}

interface AllocationMarkProps {
  allocation: Allocation
  className?: string | undefined
}

export function AllocationMark({ allocation, className }: AllocationMarkProps) {
  return <Donut slices={allocationSlices(allocation)} size="sm" className={className} />
}
