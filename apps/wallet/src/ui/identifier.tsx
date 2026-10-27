import type { ReactNode } from "react"

import { MiddleTruncate } from "@/ui/middle-truncate.tsx"

const HEAD_CHARS = 6
const TAIL_CHARS = 6

interface IdentifierProps {
  value: string
  isTruncated?: boolean | undefined
  hasTooltip?: boolean | undefined
  className?: string | undefined
}

export function Identifier({ value, isTruncated = false, hasTooltip, className }: IdentifierProps) {
  const middleStart = Math.min(HEAD_CHARS, value.length)
  const middleEnd = Math.max(middleStart, value.length - TAIL_CHARS)

  function renderSlice(start: number, end: number): ReactNode {
    function clamp(index: number): number {
      return Math.min(Math.max(index, start), end)
    }
    return (
      <>
        {value.slice(start, clamp(middleStart))}
        <span className="opacity-80">{value.slice(clamp(middleStart), clamp(middleEnd))}</span>
        {value.slice(clamp(middleEnd), end)}
      </>
    )
  }

  return isTruncated ? (
    <MiddleTruncate
      value={value}
      hasTooltip={hasTooltip}
      className={className}
      renderSlice={renderSlice}
    />
  ) : (
    <span className={className}>{renderSlice(0, value.length)}</span>
  )
}
