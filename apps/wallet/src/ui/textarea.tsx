import { Field as BaseField } from "@base-ui/react/field"
import type { ComponentPropsWithoutRef } from "react"

export const TEXT_CONTROL_CLASSES =
  "w-full min-w-0 rounded-md border border-edge bg-surface font-mono text-body text-fg -outline-offset-1 transition-colors placeholder:text-fg-faint data-disabled:pointer-events-none data-disabled:opacity-40 data-invalid:border-danger"

type TextareaProps = Omit<ComponentPropsWithoutRef<"textarea">, "className" | "color">

export function Textarea(props: TextareaProps) {
  return (
    <BaseField.Control
      className={`${TEXT_CONTROL_CLASSES} scroll-y-2 py-1`}
      render={<textarea {...props} />}
    />
  )
}
