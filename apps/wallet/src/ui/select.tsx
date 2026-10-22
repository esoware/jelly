import { Select as BaseSelect } from "@base-ui/react/select"
import { createContext, use } from "react"
import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { Icon } from "@/ui/icon.tsx"
import {
  ItemDetails,
  POPUP_ITEM_CLASSES,
  POPUP_ITEM_LABEL_CLASSES,
  POPUP_ITEM_TONE_CLASSES,
  POPUP_LIST_CLASSES,
  POPUP_SIDE_OFFSET_PX,
  POPUP_TRIGGER_CLASSES,
} from "@/ui/popup.tsx"

const SelectedValueContext = createContext<string | null>(null)

const SelectedValuesContext = createContext<readonly string[] | null>(null)

interface SelectProps<T extends string> {
  value: T
  onValueChange: (value: T) => void
  children: ReactNode
}

export function Select<T extends string>({ value, onValueChange, children }: SelectProps<T>) {
  return (
    <BaseSelect.Root
      value={value}
      onValueChange={(next: T | null) => {
        if (next !== null) {
          onValueChange(next)
        }
      }}
    >
      <SelectedValueContext value={value}>{children}</SelectedValueContext>
    </BaseSelect.Root>
  )
}

interface MultiSelectProps<T extends string> {
  values: readonly T[]
  onValuesChange: (values: T[]) => void
  children: ReactNode
}

export function MultiSelect<T extends string>({
  values,
  onValuesChange,
  children,
}: MultiSelectProps<T>) {
  return (
    <BaseSelect.Root
      multiple
      value={values}
      onValueChange={(next: T[]) => {
        onValuesChange(next)
      }}
    >
      <SelectedValuesContext value={values}>{children}</SelectedValuesContext>
    </BaseSelect.Root>
  )
}

interface SelectTriggerProps {
  size?: "sm" | "md" | undefined
  className?: string | undefined
  children: ReactNode
}

export function SelectTrigger({ size = "md", className, children }: SelectTriggerProps) {
  return (
    <BaseSelect.Trigger
      className={appendClassName(
        `${POPUP_TRIGGER_CLASSES} ${size === "sm" ? "h-control-sm" : "h-control-md"}`,
        className,
      )}
    >
      <BaseSelect.Value className="flex items-center gap-2 overflow-hidden">
        {children}
      </BaseSelect.Value>
      <BaseSelect.Icon
        className="size-icon-sm shrink-0 text-fg-faint"
        render={<Icon name="caret" />}
      />
    </BaseSelect.Trigger>
  )
}

interface SelectPopupProps {
  shouldMatchAnchor?: boolean | undefined
  children: ReactNode
}

export function SelectPopup({ shouldMatchAnchor = false, children }: SelectPopupProps) {
  return (
    <BaseSelect.Portal>
      <BaseSelect.Positioner
        className="outline-none"
        sideOffset={POPUP_SIDE_OFFSET_PX}
        alignItemWithTrigger={false}
      >
        <BaseSelect.Popup
          className={`${POPUP_LIST_CLASSES} ${shouldMatchAnchor ? "w-(--anchor-width)" : ""}`}
        >
          <BaseSelect.List>{children}</BaseSelect.List>
        </BaseSelect.Popup>
      </BaseSelect.Positioner>
    </BaseSelect.Portal>
  )
}

interface SelectItemProps<T extends string> {
  value: T
  description?: ReactNode | undefined
  badges?: readonly string[] | undefined
  children: ReactNode
}

export function SelectItem<T extends string>({
  value,
  description,
  badges,
  children,
}: SelectItemProps<T>) {
  const selected = use(SelectedValueContext)
  const values = use(SelectedValuesContext)
  const isMultiple = values !== null
  const isSole = isMultiple ? values.length === 1 && values[0] === value : selected === value
  return (
    <BaseSelect.Item
      value={value}
      disabled={isSole}
      className={`${POPUP_ITEM_CLASSES} ${POPUP_ITEM_TONE_CLASSES.default}`}
    >
      {isMultiple ? (
        <span className="flex size-icon-md shrink-0 items-center justify-center text-fg-muted">
          <BaseSelect.ItemIndicator className="size-icon-md" render={<Icon name="check" />} />
        </span>
      ) : null}
      <BaseSelect.ItemText className={POPUP_ITEM_LABEL_CLASSES}>{children}</BaseSelect.ItemText>
      <ItemDetails description={description} badges={badges} />
    </BaseSelect.Item>
  )
}
