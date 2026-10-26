import { appendClassName } from "@/lib/class-name.ts"

interface MonoProps {
  value: string
  className?: string | undefined
}

export function Mono({ value, className }: MonoProps) {
  return (
    <span className={appendClassName("font-mono text-body break-all select-text", className)}>
      {value}
    </span>
  )
}
