import { useLayoutEffect, useRef } from "react"
import type { RefObject } from "react"

const ACCENT = "--color-accent"

export function useAccentColor<T extends HTMLElement>(color: string | null): RefObject<T | null> {
  const root = useRef<T>(null)

  useLayoutEffect(() => {
    const style = root.current?.style
    if (color === null) {
      style?.removeProperty(ACCENT)
    } else {
      style?.setProperty(ACCENT, color)
    }
  }, [color])

  return root
}
