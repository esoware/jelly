import { Tabs } from "@base-ui/react/tabs"
import { createContext, use, useLayoutEffect, useRef } from "react"
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

const ActiveContext = createContext<string | null>(null)

const PillContext = createContext<RefObject<HTMLSpanElement | null> | null>(null)

const VariantContext = createContext<SegmentedVariant>("bar")

function slide(element: HTMLElement, from: DOMRect, to: DOMRect): void {
  element.style.setProperty("--slide-x", `${String(from.left - to.left)}px`)
  element.style.setProperty("--slide-y", `${String(from.top - to.top)}px`)
  element.style.setProperty("--slide-width", `${String(from.width)}px`)
  element.style.setProperty("--slide-height", `${String(from.height)}px`)
  element.dataset["sliding"] = ""
}

interface SegmentedTabsProps<T extends string> {
  value: T
  values: readonly T[]
  onValueChange: (value: T) => void
  orientation?: "horizontal" | "vertical" | undefined
  children: ReactNode
}

export function SegmentedTabs<T extends string>({
  value,
  values,
  onValueChange,
  orientation,
  children,
}: SegmentedTabsProps<T>) {
  const pillRef = useRef<HTMLSpanElement>(null)
  const pillTabRef = useRef<HTMLElement | null>(null)
  const flyerRef = useRef<HTMLSpanElement>(null)

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
    const from = source.getBoundingClientRect()
    const to = target.getBoundingClientRect()
    const group = target.closest<HTMLElement>(".segmented")
    const variant = group?.dataset["variant"]
    const flyer = flyerRef.current
    const origin = flyer?.parentElement?.getBoundingClientRect()
    if (
      source.closest(".segmented") === group ||
      variant === undefined ||
      flyer === null ||
      origin === undefined
    ) {
      slide(pill, from, to)
      return
    }
    flyer.dataset["variant"] = variant
    flyer.style.left = `${String(to.left - origin.left)}px`
    flyer.style.top = `${String(to.top - origin.top)}px`
    flyer.style.width = `${String(to.width)}px`
    flyer.style.height = `${String(to.height)}px`
    pill.dataset["waiting"] = ""
    slide(flyer, from, to)
  })

  return (
    <Tabs.Root
      value={value}
      orientation={orientation}
      className="relative isolate"
      onValueChange={(next: unknown) => {
        const picked = values.find((candidate) => candidate === next)
        if (picked !== undefined) {
          onValueChange(picked)
        }
      }}
    >
      <span
        key={value}
        ref={flyerRef}
        className="pointer-events-none absolute -z-1 hidden data-sliding:block data-sliding:animate-slide data-[variant=accent]:rounded-full data-[variant=accent]:bg-accent-active data-[variant=bar]:rounded-full data-[variant=bar]:bg-active"
        onAnimationEnd={(event) => {
          delete event.currentTarget.dataset["sliding"]
          const pill = pillRef.current
          if (pill !== null) {
            delete pill.dataset["waiting"]
          }
        }}
      />
      <ActiveContext value={value}>
        <PillContext value={pillRef}>{children}</PillContext>
      </ActiveContext>
    </Tabs.Root>
  )
}

interface SegmentedListProps {
  variant?: SegmentedVariant | undefined
  className?: string | undefined
  children: ReactNode
}

export function SegmentedList({ variant, className, children }: SegmentedListProps) {
  return (
    <Tabs.List
      data-variant={variant}
      className={({ orientation }) =>
        variant === undefined
          ? className
          : appendClassName(
              `segmented relative z-0 flex ${GROUP_CLASSES[variant]} ${orientation === "vertical" ? "flex-col" : ""}`,
              className,
            )
      }
    >
      <VariantContext value={variant ?? "bar"}>{children}</VariantContext>
    </Tabs.List>
  )
}

interface SegmentedGroupProps {
  variant: SegmentedVariant
  className?: string | undefined
  children?: ReactNode | undefined
}

export function SegmentedGroup({ variant, className, children }: SegmentedGroupProps) {
  return (
    <div
      data-variant={variant}
      className={appendClassName(
        `segmented relative z-0 flex ${GROUP_CLASSES[variant]}`,
        className,
      )}
    >
      <VariantContext value={variant}>{children}</VariantContext>
    </div>
  )
}

interface SegmentedTabProps<T extends string> {
  item: SegmentedItem<T>
}

export function SegmentedTab<T extends string>({ item }: SegmentedTabProps<T>) {
  const active = use(ActiveContext)
  const pillRef = use(PillContext)
  const variant = use(VariantContext)
  const hasPill = variant !== "cards" && active === item.value

  return (
    <Tabs.Tab
      value={item.value}
      className={`relative flex shrink-0 transition-colors select-none ${ITEM_CLASSES[variant]} ${item.icon === undefined ? "" : "pl-2"}`}
    >
      {hasPill ? (
        <span
          ref={pillRef}
          className={`absolute top-0 left-0 -z-1 size-full rounded-full data-sliding:animate-slide data-waiting:invisible ${variant === "accent" ? "bg-accent-active" : "bg-active"}`}
        />
      ) : null}
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
  return (
    <SegmentedTabs
      value={value}
      values={items.map((item) => item.value)}
      orientation={orientation}
      onValueChange={onValueChange}
    >
      <SegmentedList variant={variant} className={className}>
        {items.map((item) => (
          <SegmentedTab key={item.value} item={item} />
        ))}
      </SegmentedList>
    </SegmentedTabs>
  )
}
