import type { ComponentProps, ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"

export function Panel({ className, ...props }: ComponentProps<"section">) {
  return (
    <section
      className={appendClassName(
        "flex min-h-0 flex-col overflow-hidden rounded-xl bg-surface",
        className,
      )}
      {...props}
    />
  )
}

interface PanelHeaderProps {
  title?: string | undefined
  count?: number | undefined
  actions?: ReactNode | undefined
  children?: ReactNode | undefined
}

export function PanelHeader({ title, count, actions, children }: PanelHeaderProps) {
  return (
    <div className="flex h-11 shrink-0 items-center gap-2 px-2">
      {title === undefined ? null : <h2 className="pl-2 text-title">{title}</h2>}
      {count === undefined ? null : (
        <span className="text-caption text-fg-faint tabular-nums">{count}</span>
      )}
      {children}
      {actions === undefined ? null : (
        <div className="ml-auto flex items-center gap-1">{actions}</div>
      )}
    </div>
  )
}

export function PanelBody({ className, ...props }: ComponentProps<"div">) {
  return <div className={appendClassName("min-h-0 flex-1 scroll-y-2 pb-2", className)} {...props} />
}
