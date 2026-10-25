import { useEffect, useMemo, useState } from "react"

const MORPH_MS = 500
const EASE_POWER = 3
const PERCENT = 100
const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)"

interface Morph<S, P> {
  source: S
  from: P | null
  startedAtMs: number | null
  nowMs: number
}

interface Morphed<P> {
  target: P
  shown: P
}

function easedProgress<S, P>(morph: Morph<S, P>): number {
  if (morph.startedAtMs === null) {
    return 0
  }
  const linear = Math.min(1, (morph.nowMs - morph.startedAtMs) / MORPH_MS)
  return 1 - (1 - linear) ** EASE_POWER
}

function isSameData(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) {
    return true
  }
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
    return false
  }
  const aEntries = Object.entries(a)
  const bValues = new Map(Object.entries(b))
  return (
    aEntries.length === bValues.size &&
    aEntries.every(([key, value]) => bValues.has(key) && isSameData(value, bValues.get(key)))
  )
}

export function lerp(from: number, to: number, progress: number): number {
  return from + (to - from) * progress
}

export function mixColors(from: string, to: string, progress: number): string {
  if (from === to || progress >= 1) {
    return to
  }
  if (progress <= 0) {
    return from
  }
  return `color-mix(in oklab, ${to} ${String(Math.round(progress * PERCENT))}%, ${from})`
}

export function useMorph<S, P>(
  source: S,
  draw: (source: S) => P,
  blend: (from: P, to: P) => (progress: number) => P,
): Morphed<P> {
  const [morph, setMorph] = useState<Morph<S, P>>({
    source,
    from: null,
    startedAtMs: null,
    nowMs: 0,
  })
  const isMorphing = morph.from !== null

  useEffect(() => {
    if (!isMorphing) {
      return undefined
    }
    const durationMs = matchMedia(REDUCED_MOTION_QUERY).matches ? 0 : MORPH_MS
    let frame = 0
    function tick(nowMs: number): void {
      setMorph((current) => {
        const startedAtMs = current.startedAtMs ?? nowMs
        return nowMs - startedAtMs < durationMs
          ? { ...current, startedAtMs, nowMs }
          : { ...current, from: null, startedAtMs: null, nowMs }
      })
      frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => {
      cancelAnimationFrame(frame)
    }
  }, [isMorphing])

  const target = useMemo(() => draw(source), [draw, source])
  const play = useMemo(
    () => (morph.from === null ? null : blend(morph.from, target)),
    [blend, morph.from, target],
  )

  if (!isSameData(morph.source, source)) {
    const previous = draw(morph.source)
    const shown = morph.from === null ? previous : blend(morph.from, previous)(easedProgress(morph))
    setMorph({ source, from: shown, startedAtMs: null, nowMs: morph.nowMs })
    return { target, shown }
  }

  return { target, shown: play === null ? target : play(easedProgress(morph)) }
}
