import { mergeProps } from "@base-ui/react/merge-props"
import { useRender } from "@base-ui/react/use-render"
import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"

type RowProps = Omit<useRender.ComponentProps<"div">, "className"> & {
  size?: "md" | "sm" | undefined
  isActive?: boolean | undefined
  onActivate?: (() => void) | undefined
  className?: string | undefined
}

export function Row({ render, size = "md", isActive, onActivate, className, ...props }: RowProps) {
  return useRender({
    defaultTagName: "div",
    ...(render === undefined ? {} : { render }),
    props: mergeProps<"div">(
      {
        className: appendClassName(
          `group/row flex w-full min-w-0 items-center rounded-md px-2 text-left outline-hidden transition-colors hover:bg-hover focus-visible:bg-hover ${size === "sm" ? "gap-2 py-1" : "gap-3 py-2"} ${isActive === true ? "bg-hover" : ""}`,
          className,
        ),
      },
      onActivate === undefined
        ? undefined
        : {
            tabIndex: 0,
            onClick: onActivate,
            onKeyDown: (e) => {
              if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
                e.preventDefault()
                e.currentTarget.click()
              }
            },
          },
      props,
    ),
  })
}

interface RowActionsProps {
  children: ReactNode
}

export function RowActions({ children }: RowActionsProps) {
  return (
    <span
      className="flex shrink-0 items-center opacity-0 transition-opacity group-focus-within/row:opacity-100 group-hover/row:opacity-100 has-data-popup-open:opacity-100"
      onClick={(e) => {
        e.stopPropagation()
      }}
    >
      {children}
    </span>
  )
}
