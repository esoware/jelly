import { appendClassName } from "@/lib/class-name.ts"

interface MonoProps {
  value: string
  isFull?: boolean | undefined
  className?: string | undefined
}

export function Mono({ value, isFull, className }: MonoProps) {
  return (
    <span
      className={appendClassName(
        `font-mono text-body select-text ${isFull === true ? "break-all" : ""}`,
        className,
      )}
    >
      {value}
    </span>
  )
}
