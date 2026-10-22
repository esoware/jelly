import { Icon } from "@/ui/icon.tsx"
import type { IconName } from "@/ui/icon.tsx"

type CalloutTone = "info" | "warn" | "danger"

const SURFACE_CLASSES = {
  info: "bg-surface",
  warn: "bg-warn-subtle",
  danger: "bg-danger-subtle",
} as const satisfies Record<CalloutTone, string>

const TONE_CLASSES = {
  info: "text-fg-muted",
  warn: "text-warn",
  danger: "text-danger",
} as const satisfies Record<CalloutTone, string>

const GLYPHS = {
  info: "info",
  warn: "warning",
  danger: "danger",
} as const satisfies Record<CalloutTone, IconName>

interface CalloutProps {
  tone?: CalloutTone | undefined
  heading: string
  children: string
}

export function Callout({ tone = "info", heading, children }: CalloutProps) {
  return (
    <div className={`flex items-start gap-2 rounded-lg p-3 text-body ${SURFACE_CLASSES[tone]}`}>
      <Icon name={GLYPHS[tone]} className={`size-icon-lg shrink-0 ${TONE_CLASSES[tone]}`} />
      <div className="min-w-0 space-y-1 wrap-anywhere">
        <p className="font-semibold text-fg">{heading}</p>
        <p className="text-fg-muted">{children}</p>
      </div>
    </div>
  )
}
