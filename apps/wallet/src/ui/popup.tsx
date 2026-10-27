import type { ReactNode } from "react"

import { Badge } from "@/ui/badge.tsx"

export const POPUP_SIDE_OFFSET_PX = 4

export const POPUP_TRIGGER_CLASSES =
  "flex w-fit items-center justify-between gap-2 rounded-md border border-edge bg-surface px-2 text-body whitespace-nowrap -outline-offset-1 transition-colors select-none hover:bg-hover data-placeholder:text-fg-faint data-popup-open:bg-active"

export const POPUP_LIST_CLASSES =
  "max-h-[min(--spacing(80),var(--available-height))] min-w-[max(--spacing(52),var(--anchor-width))] origin-(--transform-origin) motion-pop overflow-x-hidden overflow-y-auto rounded-lg overlay-sm p-1 outline-none app-region-no-drag"

export const POPUP_ITEM_CLASSES =
  "popup-item flex min-h-control-md w-full shrink-0 items-center gap-2 rounded-md px-2 text-body outline-none select-none data-checked:bg-hover data-selected:bg-hover [.popup-item~&]:mt-1 [:where(&>svg)]:pointer-events-none [:where(&>svg)]:size-icon-md [:where(&>svg)]:shrink-0"

export const POPUP_ITEM_TONE_CLASSES = {
  default: "data-highlighted:bg-hover [:where(&>svg)]:text-fg-muted",
  danger: "text-danger data-highlighted:bg-danger-subtle [:where(&>svg)]:text-danger",
} as const

export const POPUP_ITEM_LABEL_CLASSES = "flex min-w-0 flex-1 items-center gap-2"

interface ItemDetailsProps {
  description: ReactNode | undefined
  badges: readonly string[] | undefined
}

export function ItemDetails({ description, badges }: ItemDetailsProps) {
  return (
    <>
      {description === undefined ? null : (
        <span className="shrink-0 text-caption text-fg-faint">{description}</span>
      )}
      {badges === undefined ? null : (
        <span className="flex shrink-0 items-center gap-1">
          {badges.map((badge) => (
            <Badge key={badge}>{badge}</Badge>
          ))}
        </span>
      )}
    </>
  )
}
