import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"

interface EmptyStateProps {
  title: string
  description?: ReactNode | undefined
  action?: ReactNode | undefined
  isCompact?: boolean | undefined
  className?: string | undefined
}

export function EmptyState({ title, description, action, isCompact, className }: EmptyStateProps) {
  return (
    <div
      className={appendClassName(
        `flex flex-col items-center justify-center gap-1 text-center ${isCompact === true ? "px-3 py-4" : "px-6 py-8"}`,
        className,
      )}
    >
      <p className="text-body font-medium">{title}</p>
      {description === undefined || description === null ? null : (
        <p className="max-w-sm text-caption text-fg-muted">{description}</p>
      )}
      {action === undefined || action === null ? null : <div className="pt-2">{action}</div>}
    </div>
  )
}
