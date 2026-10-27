import { Menu as BaseMenu } from "@base-ui/react/menu"
import { createContext, use } from "react"
import type { ComponentProps, ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { Icon } from "@/ui/icon.tsx"
import {
  ItemDetails,
  POPUP_ITEM_CLASSES,
  POPUP_ITEM_LABEL_CLASSES,
  POPUP_ITEM_TONE_CLASSES,
  POPUP_LIST_CLASSES,
  POPUP_SIDE_OFFSET_PX,
} from "@/ui/popup.tsx"

const GROUP_LABEL_CLASSES = "px-2 pt-2 pb-1 text-caption text-fg-faint"

const POPUP_INSET_PX = 4

export const Menu = BaseMenu.Root

export const MenuTrigger = BaseMenu.Trigger

interface MenuPopupProps {
  align?: ComponentProps<typeof BaseMenu.Positioner>["align"]
  alignOffset?: number | undefined
  anchor?: ComponentProps<typeof BaseMenu.Positioner>["anchor"]
  shouldMatchAnchor?: boolean | undefined
  className?: string | undefined
  children: ReactNode
}

export function MenuPopup({
  align,
  alignOffset,
  anchor,
  shouldMatchAnchor = false,
  className,
  children,
}: MenuPopupProps) {
  return (
    <BaseMenu.Portal>
      <BaseMenu.Positioner
        className="outline-none"
        align={align}
        alignOffset={alignOffset}
        anchor={anchor}
        sideOffset={POPUP_SIDE_OFFSET_PX}
      >
        <BaseMenu.Popup
          className={appendClassName(
            `${POPUP_LIST_CLASSES} ${shouldMatchAnchor ? "w-(--anchor-width)" : ""}`,
            className,
          )}
        >
          {children}
        </BaseMenu.Popup>
      </BaseMenu.Positioner>
    </BaseMenu.Portal>
  )
}

interface MenuGroupProps {
  label?: string | undefined
  children: ReactNode
}

export function MenuGroup({ label, children }: MenuGroupProps) {
  return (
    <BaseMenu.Group className="not-first:mt-2">
      {label === undefined ? null : (
        <BaseMenu.GroupLabel className={GROUP_LABEL_CLASSES}>{label}</BaseMenu.GroupLabel>
      )}
      {children}
    </BaseMenu.Group>
  )
}

interface MenuItemProps {
  tone?: keyof typeof POPUP_ITEM_TONE_CLASSES | undefined
  onClick: () => void
  children: ReactNode
}

export function MenuItem({ tone = "default", onClick, children }: MenuItemProps) {
  return (
    <BaseMenu.Item
      className={`${POPUP_ITEM_CLASSES} ${POPUP_ITEM_TONE_CLASSES[tone]}`}
      onClick={onClick}
    >
      {children}
    </BaseMenu.Item>
  )
}

const CheckedValueContext = createContext<string | null>(null)

interface MenuRadioGroupProps<T extends string> {
  label?: string | undefined
  value: T
  onValueChange: (value: T) => void
  children: ReactNode
}

export function MenuRadioGroup<T extends string>({
  label,
  value,
  onValueChange,
  children,
}: MenuRadioGroupProps<T>) {
  return (
    <BaseMenu.RadioGroup
      className="not-first:mt-2"
      value={value}
      onValueChange={(next: T) => {
        onValueChange(next)
      }}
    >
      {label === undefined ? null : (
        <BaseMenu.GroupLabel className={GROUP_LABEL_CLASSES}>{label}</BaseMenu.GroupLabel>
      )}
      <CheckedValueContext value={value}>{children}</CheckedValueContext>
    </BaseMenu.RadioGroup>
  )
}

interface MenuRadioItemProps<T extends string> {
  value: T
  description?: ReactNode | undefined
  badges?: readonly string[] | undefined
  shouldStayOpen?: boolean | undefined
  children: ReactNode
}

export function MenuRadioItem<T extends string>({
  value,
  description,
  badges,
  shouldStayOpen = false,
  children,
}: MenuRadioItemProps<T>) {
  return (
    <BaseMenu.RadioItem
      value={value}
      disabled={value === use(CheckedValueContext)}
      closeOnClick={!shouldStayOpen}
      className={`${POPUP_ITEM_CLASSES} ${POPUP_ITEM_TONE_CLASSES.default}`}
    >
      <span className={POPUP_ITEM_LABEL_CLASSES}>{children}</span>
      <ItemDetails description={description} badges={badges} />
    </BaseMenu.RadioItem>
  )
}

interface MenuSubmenuProps {
  label: ReactNode
  description?: ReactNode | undefined
  isChecked?: boolean | undefined
  className?: string | undefined
  children: ReactNode
}

export function MenuSubmenu({
  label,
  description,
  isChecked = false,
  className,
  children,
}: MenuSubmenuProps) {
  return (
    <BaseMenu.SubmenuRoot>
      <BaseMenu.SubmenuTrigger
        data-checked={isChecked ? "" : undefined}
        className={`${POPUP_ITEM_CLASSES} ${POPUP_ITEM_TONE_CLASSES.default} data-popup-open:bg-hover`}
      >
        <span className={POPUP_ITEM_LABEL_CLASSES}>{label}</span>
        <ItemDetails description={description} badges={undefined} />
        <Icon name="next" />
      </BaseMenu.SubmenuTrigger>
      <MenuPopup alignOffset={-POPUP_INSET_PX} className={className}>
        {children}
      </MenuPopup>
    </BaseMenu.SubmenuRoot>
  )
}
