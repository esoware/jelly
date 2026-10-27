import { Tabs } from "@base-ui/react/tabs"
import { useLayoutEffect, useRef } from "react"
import type { ReactNode, RefObject } from "react"

import { appendClassName } from "@/lib/class-name.ts"

interface SegmentedItem<T extends string> {
  value: T
  label: ReactNode
  icon?: ReactNode | undefined
}

type SegmentedVariant = "bar" | "accent" | "cards"

const GROUP_CLASSES = {
  bar: "rounded-full border-2 border-transparent bg-surface",
  accent: "rounded-full border-4 border-transparent bg-surface",
  cards: "gap-2",
} as const satisfies Record<SegmentedVariant, string>

const ITEM_CLASSES = {
  bar: "h-control-sm items-center justify-center gap-2 rounded-full px-3 text-caption font-medium text-fg-muted hover:text-fg data-active:text-fg",
  accent:
    "h-8 items-center justify-center gap-2 rounded-full px-3 text-body font-medium text-fg-muted hover:text-fg data-active:text-fg",
  cards:
    "min-w-0 flex-1 items-start gap-2 rounded-lg bg-surface px-3 py-2 text-left text-fg-muted hover:not-data-active:bg-hover data-active:bg-active data-active:text-fg",
} as const satisfies Record<SegmentedVariant, string>

const MARK_CLASSES = {
  bar: "size-icon-md",
  accent: "size-5",
  cards: "size-icon-lg",
} as const satisfies Record<SegmentedVariant, string>

function slide(element: HTMLElement, from: DOMRect, to: DOMRect): void {
  element.style.setProperty("--slide-x", `${String(from.left - to.left)}px`)
  element.style.setProperty("--slide-y", `${String(from.top - to.top)}px`)
  element.style.setProperty("--slide-width", `${String(from.width)}px`)
  element.style.setProperty("--slide-height", `${String(from.height)}px`)
  element.dataset["sliding"] = ""
}

interface SegmentedTabProps<T extends string> {
  item: SegmentedItem<T>
  variant: SegmentedVariant
  pillRef: RefObject<HTMLSpanElement | null> | null
}

function SegmentedTab<T extends string>({ item, variant, pillRef }: SegmentedTabProps<T>) {
  return (
    <Tabs.Tab
      value={item.value}
      className={`relative flex shrink-0 transition-colors select-none ${ITEM_CLASSES[variant]} ${item.icon === undefined ? "" : "pl-2"}`}
    >
      {pillRef === null ? null : (
        <span
          ref={pillRef}
          className={`absolute top-0 left-0 -z-1 size-full rounded-full data-sliding:animate-slide ${variant === "accent" ? "bg-accent-active" : "bg-active"}`}
        />
      )}
      {item.icon === undefined ? null : (
        <span
          className={`flex shrink-0 items-center justify-center opacity-70 transition-opacity in-data-active:opacity-100 [:where(&>*)]:size-full ${MARK_CLASSES[variant]}`}
        >
          {item.icon}
        </span>
      )}
      <span className={`min-w-0 ${variant === "cards" ? "flex flex-1 flex-col items-start" : ""}`}>
        {item.label}
      </span>
    </Tabs.Tab>
  )
}

interface SegmentedProps<T extends string> {
  value: T
  onValueChange: (value: T) => void
  items: readonly SegmentedItem<T>[]
  variant?: SegmentedVariant | undefined
  orientation?: "horizontal" | "vertical" | undefined
  className?: string | undefined
}

export function Segmented<T extends string>({
  value,
  onValueChange,
  items,
  variant = "bar",
  orientation,
  className,
}: SegmentedProps<T>) {
  const pillRef = useRef<HTMLSpanElement>(null)
  const pillTabRef = useRef<HTMLElement | null>(null)

  useLayoutEffect(() => {
    const pill = pillRef.current
    const source = pillTabRef.current
    const target = pill?.parentElement ?? null
    pillTabRef.current = target
    if (
      pill === null ||
      target === null ||
      source === null ||
      source === target ||
      !source.isConnected
    ) {
      return
    }
    slide(pill, source.getBoundingClientRect(), target.getBoundingClientRect())
  })

  return (
    <Tabs.Root
      value={value}
      orientation={orientation}
      onValueChange={(next: unknown) => {
        const picked = items.find((item) => item.value === next)
        if (picked !== undefined) {
          onValueChange(picked.value)
        }
      }}
    >
      <Tabs.List
        className={(state) =>
          appendClassName(
            `relative z-0 flex ${GROUP_CLASSES[variant]} ${state.orientation === "vertical" ? "flex-col" : ""}`,
            className,
          )
        }
      >
        {items.map((item) => (
          <SegmentedTab
            key={item.value}
            item={item}
            variant={variant}
            pillRef={variant !== "cards" && item.value === value ? pillRef : null}
          />
        ))}
      </Tabs.List>
    </Tabs.Root>
  )
}
