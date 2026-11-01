import type { CSSProperties } from "react"

declare module "react" {
  interface CSSProperties {
    "--color-accent"?: string | undefined
  }
}

export function accentStyle(color: string): CSSProperties {
  return { "--color-accent": color }
}
