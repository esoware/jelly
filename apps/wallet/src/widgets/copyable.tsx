import { useEffect, useState } from "react"
import type { ReactNode } from "react"

import { copyToClipboard } from "@/ipc/client.ts"
import { Button } from "@/ui/button.tsx"
import { Card } from "@/ui/card.tsx"
import { Icon } from "@/ui/icon.tsx"
import type { IconName } from "@/ui/icon.tsx"
import { Identifier } from "@/ui/identifier.tsx"
import { Mono } from "@/ui/mono.tsx"
import { Row } from "@/ui/row.tsx"

const RESULT_TIMEOUT_MS = 1400

type CopyResult = "copied" | "failed"

type CopyState = "idle" | CopyResult

const TONE_CLASSES = {
  idle: "text-fg-faint hover:text-fg",
  copied: "text-ok hover:text-ok",
  failed: "text-danger hover:text-danger",
} as const satisfies Record<CopyState, string>

const GLYPHS = {
  idle: "copy",
  copied: "check",
  failed: "warning",
} as const satisfies Record<CopyState, IconName>

interface CopyButtonProps {
  value: string
  size?: "icon-sm" | "icon-xs" | undefined
  isSensitive?: boolean | undefined
}

export function CopyButton({ value, size = "icon-xs", isSensitive = false }: CopyButtonProps) {
  const [feedback, setFeedback] = useState<{ result: CopyResult } | null>(null)
  const state: CopyState = feedback?.result ?? "idle"

  useEffect(() => {
    if (feedback === null) {
      return undefined
    }
    const handle = setTimeout(() => {
      setFeedback(null)
    }, RESULT_TIMEOUT_MS)
    return () => {
      clearTimeout(handle)
    }
  }, [feedback])

  async function copy(): Promise<void> {
    const isCopied = await copyToClipboard(value, isSensitive)
    setFeedback({ result: isCopied ? "copied" : "failed" })
  }

  return (
    <Button
      variant="ghost"
      size={size}
      className={TONE_CLASSES[state]}
      onClick={(e) => {
        e.stopPropagation()
        void copy()
      }}
    >
      <Icon name={GLYPHS[state]} />
    </Button>
  )
}

interface MonoWithCopyProps {
  value: string
  className?: string | undefined
}

export function MonoWithCopy({ value, className }: MonoWithCopyProps) {
  return (
    <span className="inline-flex min-w-0 items-center gap-1">
      <Mono value={value} className={className} />
      <CopyButton value={value} />
    </span>
  )
}

interface IdentifierWithCopyProps {
  value: string
}

export function IdentifierWithCopy({ value }: IdentifierWithCopyProps) {
  return (
    <span className="inline-flex min-w-0 items-center gap-1">
      <Identifier value={value} className="font-mono text-body break-all select-text" />
      <CopyButton value={value} />
    </span>
  )
}

export interface DetailRow {
  label: string
  value: ReactNode
  copy?: string | undefined
}

interface DetailListProps {
  items: readonly DetailRow[]
}

export function DetailList({ items }: DetailListProps) {
  if (items.length === 0) {
    return null
  }
  return (
    <Card className="p-1 text-body">
      <dl>
        {items.map((item) => (
          <Row key={item.label} size="sm" className="items-baseline">
            <dt className="w-32 shrink-0 text-fg-muted">{item.label}</dt>
            <dd className="flex min-w-0 flex-1 items-baseline gap-2">
              <span className="min-w-0 flex-1 font-mono break-all">{item.value}</span>
              {item.copy === undefined ? null : <CopyButton value={item.copy} />}
            </dd>
          </Row>
        ))}
      </dl>
    </Card>
  )
}
