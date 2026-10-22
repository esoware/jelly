import { Collapsible as BaseCollapsible } from "@base-ui/react/collapsible"
import type { ReactNode } from "react"

import { Icon } from "@/ui/icon.tsx"

export const Collapsible = BaseCollapsible.Root

interface CollapsibleTriggerProps {
  children: ReactNode
}

export function CollapsibleTrigger({ children }: CollapsibleTriggerProps) {
  return (
    <BaseCollapsible.Trigger className="group/collapsible flex items-center gap-1 rounded-sm text-caption text-fg-muted transition-colors select-none hover:text-fg">
      <Icon
        name="disclosure"
        className="size-icon-sm shrink-0 transition-transform group-data-panel-open/collapsible:rotate-90"
      />
      {children}
    </BaseCollapsible.Trigger>
  )
}

interface CollapsiblePanelProps {
  className?: string | undefined
  children: ReactNode
}

export function CollapsiblePanel({ className, children }: CollapsiblePanelProps) {
  return (
    <BaseCollapsible.Panel className="h-(--collapsible-panel-height) overflow-hidden transition-[height,opacity] duration-base ease-enter data-ending-style:h-0 data-ending-style:opacity-0 data-ending-style:duration-fast data-ending-style:ease-exit data-starting-style:h-0 data-starting-style:opacity-0">
      <div className={className}>{children}</div>
    </BaseCollapsible.Panel>
  )
}
