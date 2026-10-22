import { appendClassName } from "@/lib/class-name.ts"
import { TooltipText } from "@/ui/tooltip.tsx"

const TAIL_CHARS = 6

interface MiddleTruncateProps {
  value: string
  hasTooltip?: boolean | undefined
  className?: string | undefined
}

export function MiddleTruncate({ value, hasTooltip = true, className }: MiddleTruncateProps) {
  const cut = Math.max(0, value.length - TAIL_CHARS)
  return (
    <TooltipText
      content={hasTooltip ? value : null}
      render={<span className={appendClassName("inline-flex max-w-full min-w-0", className)} />}
    >
      <span className="truncate">{value.slice(0, cut)}</span>
      <span className="shrink-0">{value.slice(cut)}</span>
    </TooltipText>
  )
}
