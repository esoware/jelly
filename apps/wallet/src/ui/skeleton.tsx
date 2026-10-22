import type { ComponentProps } from "react"

import { appendClassName } from "@/lib/class-name.ts"

export function Skeleton({ className, ...props }: ComponentProps<"div">) {
  return (
    <div className={appendClassName("animate-pulse rounded-md bg-surface", className)} {...props} />
  )
}
