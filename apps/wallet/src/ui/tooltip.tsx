import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip"
import type { ComponentProps, ReactElement, ReactNode } from "react"

import { POPUP_SIDE_OFFSET_PX } from "@/ui/popup.tsx"

const TOOLTIP_DELAY_MS = 300

interface TooltipProviderProps {
  children: ReactNode
}

export function TooltipProvider({ children }: TooltipProviderProps) {
  return <BaseTooltip.Provider delay={TOOLTIP_DELAY_MS}>{children}</BaseTooltip.Provider>
}

interface TooltipPopupProps {
  side?: ComponentProps<typeof BaseTooltip.Positioner>["side"]
  children: ReactNode
}

function TooltipPopup({ side, children }: TooltipPopupProps) {
  return (
    <BaseTooltip.Portal>
      <BaseTooltip.Positioner
        className="outline-none"
        side={side}
        sideOffset={POPUP_SIDE_OFFSET_PX}
      >
        <BaseTooltip.Popup className="max-w-sm origin-(--transform-origin) motion-pop rounded-md overlay-sm px-2 py-1 text-caption wrap-anywhere data-instant:transition-none">
          {children}
        </BaseTooltip.Popup>
      </BaseTooltip.Positioner>
    </BaseTooltip.Portal>
  )
}

interface TooltipTextProps {
  content: ReactNode
  side?: TooltipPopupProps["side"]
  render: ReactElement
  children: ReactNode
}

export function TooltipText({ content, side, render, children }: TooltipTextProps) {
  const isEmpty = content === null || content === undefined
  return (
    <BaseTooltip.Root disabled={isEmpty}>
      <BaseTooltip.Trigger render={render}>{children}</BaseTooltip.Trigger>
      {isEmpty ? null : <TooltipPopup side={side}>{content}</TooltipPopup>}
    </BaseTooltip.Root>
  )
}
