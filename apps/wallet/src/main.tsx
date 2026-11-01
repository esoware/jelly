import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider } from "@tanstack/react-router"
import { StrictMode } from "react"
import type { ReactNode } from "react"
import { createRoot } from "react-dom/client"
import type { Root } from "react-dom/client"

import { hasCode } from "@/ipc/client.ts"
import type { IpcError } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { appCommands, loadCatalog } from "@/query/app.ts"
import { makeRouter } from "@/router.tsx"
import { startSession } from "@/session.ts"
import { BootFailed } from "@/shell/shell.tsx"
import { toastManager } from "@/store.ts"
import { ToastProvider } from "@/ui/toast.tsx"
import { TooltipProvider } from "@/ui/tooltip.tsx"

function isEditable(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  )
}

function refuseDrop(event: DragEvent): void {
  const transfer = event.dataTransfer
  const hasFiles = transfer?.types.includes("Files") ?? false
  if (isEditable(event.target) && !hasFiles) {
    return
  }
  event.preventDefault()
  if (transfer !== null) {
    transfer.dropEffect = "none"
  }
}

function refuseSelectAll(event: KeyboardEvent): void {
  const isSelectAll =
    (event.ctrlKey || event.metaKey) && (event.code === "KeyA" || event.key.toLowerCase() === "a")
  if (isSelectAll && !isEditable(event.target)) {
    event.preventDefault()
  }
}

function refuseBrowserInput(): void {
  const options = { capture: true } as const
  window.addEventListener("dragover", refuseDrop, options)
  window.addEventListener("drop", refuseDrop, options)
  window.addEventListener("keydown", refuseSelectAll, options)
}

declare module "@tanstack/react-query" {
  interface Register {
    defaultError: IpcError
  }
}

const RETRY_LIMIT = 2

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      networkMode: "always",
      refetchOnWindowFocus: false,
      staleTime: Number.POSITIVE_INFINITY,
      gcTime: Number.POSITIVE_INFINITY,
      retry: (failureCount, error) => !hasCode(error, "locked") && failureCount < RETRY_LIMIT,
    },
    mutations: { networkMode: "always", retry: false, gcTime: 0 },
  },
})

interface ProvidersProps {
  children: ReactNode
}

function Providers({ children }: ProvidersProps) {
  return (
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <TooltipProvider>
          <ToastProvider toastManager={toastManager}>{children}</ToastProvider>
        </TooltipProvider>
      </QueryClientProvider>
    </StrictMode>
  )
}

async function setRootData(key: string, value: Promise<string>): Promise<void> {
  try {
    document.documentElement.dataset[key] = await value
  } catch (error) {
    reportError(error)
  }
}

function logRenderFailure(
  scope: string,
  error: unknown,
  info: { componentStack?: string | undefined },
): void {
  reportError(
    new Error(`${scope} in ${info.componentStack ?? "an unknown subtree"}`, { cause: error }),
  )
}

async function boot(root: Root): Promise<void> {
  try {
    await loadCatalog()
  } catch (error) {
    root.render(
      <Providers>
        <BootFailed message={describeFailure(error)} />
      </Providers>,
    )
    return
  }

  const router = makeRouter(queryClient)
  startSession(queryClient, router)
  root.render(
    <Providers>
      <RouterProvider router={router} />
    </Providers>,
  )
}

refuseBrowserInput()
await Promise.all([
  setRootData("backdrop", appCommands.windowBackdrop()),
  setRootData("windowControls", appCommands.windowControls()),
  setRootData("platform", appCommands.platform()),
])

const container = document.querySelector("#root")

if (container === null) {
  reportError(new Error("index.html has no #root element"))
} else {
  await boot(
    createRoot(container, {
      onUncaughtError: (error, info) => {
        logRenderFailure("Uncaught render failure", error, info)
      },
      onCaughtError: (error, info) => {
        logRenderFailure("Caught render failure", error, info)
      },
    }),
  )
}
