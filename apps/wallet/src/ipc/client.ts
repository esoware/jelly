import { getCurrentWindow } from "@tauri-apps/api/window"
import { writeText } from "@tauri-apps/plugin-clipboard-manager"

import type {
  IpcError as IpcErrorPayload,
  IpcErrorCode,
  IpcErrorDetail,
} from "@/ipc/bindings.gen.ts"

const IPC_ERROR_CODES: Readonly<Record<IpcErrorCode, true>> = {
  starting: true,
  locked: true,
  wrong_password: true,
  wrong_passphrase: true,
  passphrase_required: true,
  not_found: true,
  already_exists: true,
  invalid_input: true,
  invalid_address: true,
  insufficient_funds: true,
  endpoints_down: true,
  rpc: true,
  no_adapter: true,
  unsupported: true,
  confirmation_required: true,
  unknown_pending_tx: true,
  rebuild_required: true,
  rejected: true,
  signing: true,
  protocol_too_new: true,
  address_mismatch: true,
  storage: true,
  keychain: true,
  prices_unavailable: true,
  panic: true,
  internal: true,
}

const TRANSPORT_DIAGNOSTIC = "IPC transport failed without a message"

const UNRECOGNIZED_DIAGNOSTIC = "IPC rejection was not an error payload"

export class IpcError extends Error {
  readonly code: IpcErrorCode | "transport" | "unrecognized"
  readonly detail: IpcErrorDetail | null

  constructor(
    code: IpcError["code"],
    message: string,
    detail: IpcErrorDetail | null,
    options?: ErrorOptions,
  ) {
    super(message, options)
    this.name = "IpcError"
    this.code = code
    this.detail = detail
  }
}

function isIpcErrorPayload(value: unknown): value is IpcErrorPayload {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    typeof value.code === "string" &&
    Object.hasOwn(IPC_ERROR_CODES, value.code) &&
    "message" in value &&
    typeof value.message === "string" &&
    "detail" in value
  )
}

export function errorFromPayload(payload: unknown): IpcError {
  if (isIpcErrorPayload(payload)) {
    return new IpcError(payload.code, payload.message, payload.detail, { cause: payload })
  }
  const message =
    typeof payload === "string" && payload.length > 0 ? payload : UNRECOGNIZED_DIAGNOSTIC
  return new IpcError("unrecognized", message, null, { cause: payload })
}

function errorFromRejection(reason: unknown): IpcError {
  const message =
    reason instanceof Error && reason.message.length > 0 ? reason.message : TRANSPORT_DIAGNOSTIC
  return new IpcError("transport", message, null, { cause: reason })
}

export function hasCode(error: unknown, code: IpcErrorCode): boolean {
  return error instanceof IpcError && error.code === code
}

type IpcResult<T> = { status: "ok"; data: T } | { status: "error"; error: unknown }

const sessionLostListeners = new Set<() => void>()

export function onSessionLost(listener: () => void): void {
  sessionLostListeners.add(listener)
}

export async function unwrap<T>(call: Promise<IpcResult<T>>): Promise<T> {
  const result = await call.catch((error: unknown) => {
    throw errorFromRejection(error)
  })
  if (result.status === "ok") {
    return result.data
  }
  const error = errorFromPayload(result.error)
  if (error.code === "locked") {
    for (const listener of sessionLostListeners) {
      listener()
    }
  }
  throw error
}

export function attempt(action: string, call: () => Promise<unknown>): void {
  async function run(): Promise<void> {
    try {
      await call()
    } catch (error) {
      if (import.meta.env.DEV) {
        reportError(new Error(`${action} was refused`, { cause: error }))
      }
    }
  }
  void run()
}

interface EventChannel<T> {
  listen: (handler: (event: { payload: T }) => void) => Promise<unknown>
}

export function subscribe<T>(channel: EventChannel<T>, listener: (payload: T) => void): void {
  attempt("Event listener registration", () =>
    channel.listen((event) => {
      listener(event.payload)
    }),
  )
}

export async function copyToClipboard(value: string, isSensitive = false): Promise<boolean> {
  try {
    await (isSensitive ? navigator.clipboard.writeText(value) : writeText(value))
    return true
  } catch {
    return false
  }
}

function attemptWindowAction(action: string, call: () => Promise<unknown>): void {
  attempt(`Window action ${action}`, call)
}

export function setWindowTitle(title: string): void {
  attemptWindowAction("setTitle", () => getCurrentWindow().setTitle(title))
}

export function minimizeWindow(): void {
  attemptWindowAction("minimize", () => getCurrentWindow().minimize())
}

export function toggleMaximizeWindow(): void {
  attemptWindowAction("toggleMaximize", () => getCurrentWindow().toggleMaximize())
}

export function closeWindow(): void {
  attemptWindowAction("close", () => getCurrentWindow().close())
}

export function startWindowDrag(): void {
  attemptWindowAction("startDragging", () => getCurrentWindow().startDragging())
}

interface WindowState {
  isMaximized: boolean
  isFullscreen: boolean
}

export function watchWindowState(onChange: (state: WindowState) => void): () => void {
  const handle = getCurrentWindow()
  let isDisposed = false

  function push(): void {
    attemptWindowAction("isMaximized and isFullscreen", async () => {
      const [isMaximized, isFullscreen] = await Promise.all([
        handle.isMaximized(),
        handle.isFullscreen(),
      ])
      if (!isDisposed) {
        onChange({ isMaximized, isFullscreen })
      }
    })
  }

  push()
  const unlisten = handle.onResized(() => {
    push()
  })

  return () => {
    isDisposed = true
    attemptWindowAction("onResized", async () => {
      const stop = await unlisten
      stop()
    })
  }
}
