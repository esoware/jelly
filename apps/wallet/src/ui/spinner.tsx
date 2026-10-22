import { appendClassName } from "@/lib/class-name.ts"
import { Icon } from "@/ui/icon.tsx"

interface SpinnerProps {
  size?: "sm" | "md" | undefined
  className?: string | undefined
}

export function Spinner({ size = "md", className }: SpinnerProps) {
  return (
    <Icon
      name="pending"
      className={appendClassName(
        `animate-spin ${size === "sm" ? "size-icon-sm" : "size-icon-md"}`,
        className,
      )}
    />
  )
}
