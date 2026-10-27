import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { TooltipText } from "@/ui/tooltip.tsx"

const TAIL_CHARS = 6

interface MiddleTruncateProps {
  value: string
  hasTooltip?: boolean | undefined
  className?: string | undefined
  renderSlice?: ((start: number, end: number) => ReactNode) | undefined
}

export function MiddleTruncate({
  value,
  hasTooltip = true,
  className,
  renderSlice,
}: MiddleTruncateProps) {
  const cut = Math.max(0, value.length - TAIL_CHARS)

  function slice(start: number, end: number): ReactNode {
    return renderSlice === undefined ? value.slice(start, end) : renderSlice(start, end)
  }

  return (
    <TooltipText
      content={hasTooltip ? value : null}
      render={<span className={appendClassName("inline-flex max-w-full min-w-0", className)} />}
    >
      <span className="truncate">{slice(0, cut)}</span>
      <span className="shrink-0">{slice(cut, value.length)}</span>
    </TooltipText>
  )
}
