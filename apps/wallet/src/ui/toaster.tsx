import { Toast } from "@base-ui/react/toast"

import { Icon } from "@/ui/icon.tsx"
import type { IconName } from "@/ui/icon.tsx"

import "@/ui/toaster.css"

const GLYPHS = new Map<string | undefined, IconName>([
  ["success", "success"],
  ["warning", "warning"],
  ["error", "failure"],
])

export function Toaster() {
  const { toasts, close } = Toast.useToastManager()
  const frontmost = toasts.find(
    (toast) => toast.transitionStatus !== "ending" && toast.limited !== true,
  )
  return (
    <Toast.Portal>
      <Toast.Viewport className="fixed bottom-3 left-3 z-10 w-80 outline-none">
        {toasts.map((toast) => (
          <Toast.Root
            key={toast.id}
            toast={toast}
            className="toaster-toast absolute bottom-0 left-0 z-[calc(1000-var(--toast-index))] h-(--stack-height) w-full origin-bottom rounded-lg overlay-sm bg-clip-padding transition-[transform,opacity,height] duration-base ease-enter after:absolute after:inset-x-0 after:top-full after:h-[calc(var(--stack-gap)+1px)] data-ending-style:opacity-0 data-expanded:h-(--toast-height) data-limited:opacity-0 data-starting-style:opacity-0"
            onClick={(event) => {
              if (frontmost !== undefined && toast.transitionStatus !== "ending") {
                close(
                  Object.hasOwn(event.currentTarget.dataset, "expanded") ? toast.id : frontmost.id,
                )
              }
            }}
          >
            <Toast.Content className="flex gap-2 p-3 transition-opacity data-behind:not-data-expanded:pointer-events-none data-behind:not-data-expanded:opacity-0">
              <span className="flex h-lh shrink-0 items-center text-title text-fg-muted">
                <Icon name={GLYPHS.get(toast.type) ?? "info"} className="size-icon-md" />
              </span>
              <div className="flex min-w-0 flex-1 flex-col">
                <Toast.Title className="text-title font-medium" />
                <Toast.Description className="text-body text-fg-muted" />
              </div>
            </Toast.Content>
          </Toast.Root>
        ))}
      </Toast.Viewport>
    </Toast.Portal>
  )
}
