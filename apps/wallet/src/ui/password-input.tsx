import { useState } from "react"
import type { ComponentPropsWithoutRef } from "react"

import { Icon } from "@/ui/icon.tsx"
import { Input, InputButton } from "@/ui/input.tsx"

type PasswordInputProps = Omit<
  ComponentPropsWithoutRef<typeof Input>,
  "type" | "value" | "attachment"
> & {
  value: string
}

export function PasswordInput({ value, ...props }: PasswordInputProps) {
  const [isRevealed, setIsRevealed] = useState(false)
  const hasValue = value !== ""
  if (isRevealed && !hasValue) {
    setIsRevealed(false)
  }

  return (
    <Input
      type={isRevealed ? "text" : "password"}
      value={value}
      attachment={
        hasValue ? (
          <InputButton
            onClick={() => {
              setIsRevealed(!isRevealed)
            }}
          >
            <Icon name={isRevealed ? "hide" : "reveal"} />
          </InputButton>
        ) : null
      }
      {...props}
    />
  )
}
