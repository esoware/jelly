import { Field as BaseField } from "@base-ui/react/field"
import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"

export const FIELD_LABEL_CLASSES = "shrink-0 text-body font-medium text-fg-muted select-none"

interface FieldProps {
  label: string
  hint?: ReactNode | undefined
  error?: string | null | undefined
  className?: string | undefined
  children: ReactNode
}

export function Field({ label, hint, error = null, className, children }: FieldProps) {
  return (
    <BaseField.Root
      invalid={error !== null}
      className={appendClassName("flex flex-col gap-1", className)}
    >
      <div className="flex items-baseline justify-between gap-3">
        <BaseField.Label className={FIELD_LABEL_CLASSES}>{label}</BaseField.Label>
        {error === null ? null : (
          <BaseField.Error match className="min-w-0 text-right text-caption text-danger">
            {error}
          </BaseField.Error>
        )}
      </div>
      {children}
      {hint === undefined || hint === null ? null : (
        <BaseField.Description className="text-caption text-fg-faint">{hint}</BaseField.Description>
      )}
    </BaseField.Root>
  )
}
