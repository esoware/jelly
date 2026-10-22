import { Children, isValidElement } from "react"
import type { ComponentProps } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { Icon } from "@/ui/icon.tsx"
import { Spinner } from "@/ui/spinner.tsx"

type ButtonVariant = "primary" | "secondary" | "subtle" | "ghost" | "danger" | "ghost-danger"

type ButtonSize = "md" | "sm" | "xs" | "icon-sm" | "icon-xs"

const VARIANT_CLASSES = {
  primary: "bg-fg text-canvas hover:bg-fg/80",
  secondary: "bg-surface text-fg hover:bg-hover",
  subtle:
    "bg-surface text-fg-muted hover:bg-hover hover:text-fg data-popup-open:bg-active data-popup-open:text-fg",
  ghost: "text-fg-muted hover:bg-hover hover:text-fg",
  danger: "bg-danger-subtle text-danger hover:bg-danger/20",
  "ghost-danger": "text-danger hover:bg-danger-subtle",
} as const satisfies Record<ButtonVariant, string>

const SIZE_CLASSES = {
  md: "h-control-md gap-2 rounded-full px-3 text-body [:where(&_svg)]:size-icon-md",
  sm: "h-control-sm gap-1 rounded-full px-3 text-caption [:where(&_svg)]:size-icon-sm",
  xs: "h-control-xs gap-1 rounded-full px-2 text-caption [:where(&_svg)]:size-icon-sm",
  "icon-sm": "size-control-sm rounded-md [:where(&_svg)]:size-icon-md",
  "icon-xs": "size-control-xs rounded-md [:where(&_svg)]:size-icon-sm",
} as const satisfies Record<ButtonSize, string>

const SPINNER_SIZES = {
  md: "md",
  sm: "sm",
  xs: "sm",
  "icon-sm": "md",
  "icon-xs": "sm",
} as const satisfies Record<ButtonSize, "md" | "sm">

type ButtonProps = Omit<ComponentProps<"button">, "className"> & {
  variant?: ButtonVariant | undefined
  size?: ButtonSize | undefined
  className?: string | undefined
  isLoading?: boolean | undefined
}

export function Button({
  className,
  variant = "primary",
  size = "md",
  isLoading,
  disabled,
  children,
  ...props
}: ButtonProps) {
  const spinnerSize = SPINNER_SIZES[size]
  const nodes = Children.toArray(children)
  const iconIndex = nodes.findIndex((node) => isValidElement(node) && node.type === Icon)
  const loadingChildren =
    iconIndex === -1 ? (
      <>
        <span className="invisible contents">{children}</span>
        <Spinner size={spinnerSize} className="absolute inset-0 m-auto" />
      </>
    ) : (
      nodes.with(iconIndex, <Spinner key="spinner" size={spinnerSize} />)
    )

  return (
    <button
      type="button"
      {...props}
      disabled={isLoading === true || disabled}
      className={appendClassName(
        `relative inline-flex shrink-0 items-center justify-center font-medium whitespace-nowrap transition-colors select-none disabled:pointer-events-none disabled:opacity-40 [:where(&_svg)]:pointer-events-none [:where(&_svg)]:shrink-0 ${VARIANT_CLASSES[variant]} ${SIZE_CLASSES[size]}`,
        className,
      )}
    >
      {isLoading === true ? loadingChildren : children}
    </button>
  )
}
