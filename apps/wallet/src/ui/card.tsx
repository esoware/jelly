import type { ComponentProps } from "react"

import { appendClassName } from "@/lib/class-name.ts"

export function Card({ className, ...props }: ComponentProps<"div">) {
  return <div className={appendClassName("rounded-lg bg-surface", className)} {...props} />
}
