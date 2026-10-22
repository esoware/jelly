import { Input as BaseInput } from "@base-ui/react/input"
import { useRef } from "react"
import type { ComponentPropsWithoutRef, ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { Button } from "@/ui/button.tsx"

type InputProps = Omit<ComponentPropsWithoutRef<"input">, "className" | "size" | "color"> & {
  size?: "md" | "sm" | undefined
  isMono?: boolean | undefined
  icon?: ReactNode
  attachment?: ReactNode
  className?: string | undefined
}

export function Input({ className, size = "md", isMono, icon, attachment, ...props }: InputProps) {
  const inputRef = useRef<HTMLInputElement>(null)

  return (
    <div
      className={appendClassName(
        `flex w-full min-w-0 items-center rounded-md border border-edge bg-surface outline-2 -outline-offset-1 outline-transparent transition-colors has-[>input:focus-visible]:outline-fg-muted has-[>input[data-disabled]]:pointer-events-none has-[>input[data-disabled]]:opacity-40 has-[>input[data-invalid]]:border-danger ${size === "sm" ? "h-control-sm" : "h-control-md"}`,
        className,
      )}
      onPointerDown={(e) => {
        if (e.button === 0 && e.target === e.currentTarget) {
          e.preventDefault()
          inputRef.current?.focus()
        }
      }}
    >
      {icon === undefined || icon === null ? null : (
        <span
          inert
          className={`pointer-events-none ml-2 flex shrink-0 items-center justify-center text-fg-faint select-none [:where(&>*)]:size-full ${size === "sm" ? "size-icon-sm" : "size-icon-md"}`}
        >
          {icon}
        </span>
      )}
      <BaseInput
        ref={inputRef}
        className={`h-full min-w-0 flex-1 bg-transparent px-2 text-body text-fg outline-none placeholder:text-fg-faint ${isMono === true ? "font-mono" : ""}`}
        {...props}
      />
      {attachment === undefined || attachment === null ? null : (
        <span className="flex shrink-0 items-center gap-1 pr-0.75 [:where(&>svg)]:mx-1 [:where(&>svg)]:size-icon-md [:where(&>svg)]:shrink-0">
          {attachment}
        </span>
      )}
    </div>
  )
}

interface InputButtonProps {
  onClick: () => void
  children: ReactNode
}

export function InputButton({ onClick, children }: InputButtonProps) {
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      tabIndex={-1}
      className="text-fg-faint hover:text-fg"
      onMouseDown={(e) => {
        e.preventDefault()
      }}
      onClick={onClick}
    >
      {children}
    </Button>
  )
}
