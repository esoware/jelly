import { Dialog as BaseDialog } from "@base-ui/react/dialog"
import { createContext, use, useEffect, useLayoutEffect, useRef, useState } from "react"
import type { ComponentProps, ReactElement, ReactNode, RefObject, TransitionEvent } from "react"

import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import { StepTransition } from "@/ui/step-transition.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"

export interface DialogStateProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onOpenChangeComplete: (open: boolean) => void
}

export interface DialogPayload<T> {
  payload: T | null
  show: (payload: T) => void
  props: DialogStateProps
}

export function useDialogPayload<T>(): DialogPayload<T> {
  const [payload, setPayload] = useState<T | null>(null)
  const [isOpen, setIsOpen] = useState(false)

  return {
    payload,
    show: (next) => {
      setPayload(next)
      setIsOpen(true)
    },
    props: {
      open: isOpen,
      onOpenChange: (open) => {
        setIsOpen(open)
      },
      onOpenChangeComplete: (open) => {
        if (!open && !isOpen) {
          setPayload(null)
        }
      },
    },
  }
}

const POPUP_SIZE_CLASSES = { sm: "max-w-sm", md: "max-w-md", lg: "max-w-lg" } as const

const DialogOpenContext = createContext(false)

const DialogLockContext = createContext<RefObject<boolean> | null>(null)

type DialogProps = Omit<ComponentProps<typeof BaseDialog.Root>, "open"> & {
  open: boolean
  isLocked?: boolean | undefined
}

export function Dialog({ open, isLocked, onOpenChange, ...props }: DialogProps) {
  const [canOpen, setCanOpen] = useState(!open)
  const isTransitionLockedRef = useRef(false)
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      setCanOpen(true)
    })
    return () => {
      cancelAnimationFrame(frame)
    }
  }, [])
  const isOpen = canOpen && open
  return (
    <DialogOpenContext value={isOpen}>
      <DialogLockContext value={isTransitionLockedRef}>
        <BaseDialog.Root
          open={isOpen}
          onOpenChange={(next, details) => {
            if (!next && (isLocked === true || isTransitionLockedRef.current)) {
              details.cancel()
              return
            }
            onOpenChange?.(next, details)
          }}
          {...props}
        />
      </DialogLockContext>
    </DialogOpenContext>
  )
}

interface RetainedChildrenProps {
  isOpen: boolean
  children: ReactNode
}

function RetainedChildren({ isOpen, children }: RetainedChildrenProps) {
  const [retained, setRetained] = useState(children)
  if (isOpen && retained !== children) {
    setRetained(children)
  }
  return isOpen ? children : retained
}

export const DialogClose = BaseDialog.Close

interface DialogPopupProps {
  size?: keyof typeof POPUP_SIZE_CLASSES | undefined
  hasCloseButton?: boolean | undefined
  children: ReactNode
}

export function DialogPopup({ size = "md", hasCloseButton = true, children }: DialogPopupProps) {
  const isOpen = use(DialogOpenContext)
  return (
    <BaseDialog.Portal>
      <BaseDialog.Backdrop className="fixed inset-0 bg-scrim transition-opacity duration-base ease-enter data-ending-style:opacity-0 data-ending-style:duration-fast data-ending-style:ease-exit data-starting-style:opacity-0" />
      <BaseDialog.Viewport className="fixed inset-0 flex items-center justify-center p-6">
        <BaseDialog.Popup
          inert={!isOpen}
          className={`relative flex max-h-full min-h-0 w-full motion-pop flex-col overflow-hidden rounded-xl overlay-lg outline-none app-region-no-drag ${POPUP_SIZE_CLASSES[size]}`}
        >
          <RetainedChildren isOpen={isOpen}>{children}</RetainedChildren>
          {hasCloseButton ? (
            <BaseDialog.Close
              render={<Button variant="ghost" size="icon-sm" className="absolute top-3 right-3" />}
            >
              <Icon name="close" />
            </BaseDialog.Close>
          ) : null}
        </BaseDialog.Popup>
      </BaseDialog.Viewport>
    </BaseDialog.Portal>
  )
}

const RESIZING = "data-resizing"

function handleTransitionEnd(event: TransitionEvent<HTMLDivElement>): void {
  const node = event.currentTarget
  if (event.target === node && event.propertyName === "height") {
    node.style.removeProperty("height")
    node.removeAttribute(RESIZING)
  }
}

interface DialogTransitionProps {
  step: number
  isLocked?: boolean | undefined
  children: ReactNode
}

export function DialogTransition({ step, isLocked = false, children }: DialogTransitionProps) {
  const isTransitionLockedRef = use(DialogLockContext)
  const rootRef = useRef<HTMLDivElement>(null)
  const lastHeightRef = useRef<number | null>(null)
  const resizedStepRef = useRef(step)

  useEffect(() => {
    if (isTransitionLockedRef === null) {
      return undefined
    }
    isTransitionLockedRef.current = isLocked
    return () => {
      isTransitionLockedRef.current = false
    }
  }, [isTransitionLockedRef, isLocked])

  useEffect(() => {
    const node = rootRef.current
    if (node === null) {
      return undefined
    }
    const observer = new ResizeObserver(() => {
      lastHeightRef.current = node.getBoundingClientRect().height
    })
    observer.observe(node)
    return () => {
      observer.disconnect()
    }
  }, [])

  useLayoutEffect(() => {
    const node = rootRef.current
    const from = lastHeightRef.current
    if (node === null || from === null || resizedStepRef.current === step) {
      return
    }
    resizedStepRef.current = step
    node.style.removeProperty("height")
    const to = node.getBoundingClientRect().height
    if (to === from) {
      node.removeAttribute(RESIZING)
      return
    }
    node.setAttribute(RESIZING, "")
    node.style.height = `${String(from)}px`
    node.getBoundingClientRect()
    node.style.height = `${String(to)}px`
  })

  return (
    <div
      ref={rootRef}
      className="flex min-h-0 flex-col transition-[height] duration-base ease-enter"
      onTransitionEnd={handleTransitionEnd}
    >
      <StepTransition step={step} className="flex min-h-0 flex-1 flex-col">
        {children}
      </StepTransition>
    </div>
  )
}

interface DialogFormProps {
  onSubmit: () => void
  children: ReactNode
}

export function DialogForm({ onSubmit, children }: DialogFormProps) {
  return (
    <form
      className="flex min-h-0 flex-1 flex-col"
      onSubmit={(e) => {
        e.preventDefault()
        e.stopPropagation()
        onSubmit()
      }}
    >
      {children}
    </form>
  )
}

interface DialogHeaderProps {
  children: ReactNode
}

export function DialogHeader({ children }: DialogHeaderProps) {
  return <div className="flex shrink-0 flex-col gap-1 px-5 pt-4 pb-3">{children}</div>
}

interface DialogBodyProps {
  hasFooter?: boolean | undefined
  children: ReactNode
}

export function DialogBody({ hasFooter = true, children }: DialogBodyProps) {
  return (
    <div
      className={`min-h-0 flex-1 scroll-y-5 space-y-3 pt-1 in-data-resizing:overflow-y-hidden ${hasFooter ? "pb-1" : "pb-5"}`}
    >
      {children}
    </div>
  )
}

interface DialogFooterProps {
  children: ReactNode
}

export function DialogFooter({ children }: DialogFooterProps) {
  return <div className="flex shrink-0 justify-end gap-2 px-5 pt-3 pb-4">{children}</div>
}

const NOTE_TONE_CLASSES = { warn: "text-warn", danger: "text-danger" } as const

interface DialogFooterNoteProps {
  tone: keyof typeof NOTE_TONE_CLASSES
  children: string
}

export function DialogFooterNote({ tone, children }: DialogFooterNoteProps) {
  return (
    <TooltipText
      content={children}
      render={
        <p
          className={`min-w-0 flex-1 self-center truncate text-caption ${NOTE_TONE_CLASSES[tone]}`}
        />
      }
    >
      {children}
    </TooltipText>
  )
}

interface DialogTitleProps {
  children: ReactNode
}

export function DialogTitle({ children }: DialogTitleProps) {
  return <BaseDialog.Title className="text-title">{children}</BaseDialog.Title>
}

interface DialogDescriptionProps {
  render?: ReactElement | undefined
  children: ReactNode
}

export function DialogDescription({ render, children }: DialogDescriptionProps) {
  return (
    <BaseDialog.Description
      className="text-body text-fg-muted"
      {...(render === undefined ? {} : { render })}
    >
      {children}
    </BaseDialog.Description>
  )
}
