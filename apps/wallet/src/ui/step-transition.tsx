import { useState } from "react"
import type { ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"

type Entry = "none" | "forward" | "backward"

const ENTRY_CLASSES = {
  none: "",
  forward: "animate-enter-forward",
  backward: "animate-enter-backward",
} as const satisfies Record<Entry, string>

interface StepTransitionProps {
  step: number
  className?: string | undefined
  children: ReactNode
}

export function StepTransition({ step, className, children }: StepTransitionProps) {
  const [shown, setShown] = useState<{ step: number; entry: Entry }>({ step, entry: "none" })

  if (shown.step !== step) {
    setShown({ step, entry: step > shown.step ? "forward" : "backward" })
  }

  return (
    <div key={step} className={appendClassName(ENTRY_CLASSES[shown.entry], className)}>
      {children}
    </div>
  )
}
