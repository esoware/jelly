import { getCurrentWindow } from "@tauri-apps/api/window"
import { writeText } from "@tauri-apps/plugin-clipboard-manager"

import type { IpcErrorCode, IpcErrorDetail } from "@/ipc/bindings.gen.ts"

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

function isIpcErrorCode(value: string): value is IpcErrorCode {
  return Object.hasOwn(IPC_ERROR_CODES, value)
}

export type IpcErrorKind = "backend" | "transport" | "unrecognized"

const TRANSPORT_DIAGNOSTIC = "IPC transport failed without a message"

const UNRECOGNIZED_DIAGNOSTIC = "IPC rejection was not an error payload"

export class IpcError extends Error {
  readonly kind: IpcErrorKind
  readonly code: IpcErrorCode | null
  readonly detail: IpcErrorDetail | null

  constructor(
    kind: IpcErrorKind,
    code: IpcErrorCode | null,
    message: string,
    detail: IpcErrorDetail | null,
    options?: ErrorOptions,
  ) {
    super(message, options)
    this.name = "IpcError"
    this.kind = kind
    this.code = code
    this.detail = detail
  }
}

interface WirePayload {
  code: string
  message: string
  detail?: IpcErrorDetail | null
}

function isWirePayload(value: unknown): value is WirePayload {
  if (typeof value !== "object" || value === null) {
    return false
  }
  return (
    "code" in value &&
    typeof value.code === "string" &&
    "message" in value &&
    typeof value.message === "string"
  )
}

export function errorFromPayload(payload: unknown): IpcError {
  if (!isWirePayload(payload)) {
    const message =
      typeof payload === "string" && payload.length > 0 ? payload : UNRECOGNIZED_DIAGNOSTIC
    return new IpcError("unrecognized", null, message, null, { cause: payload })
  }
  const code = isIpcErrorCode(payload.code) ? payload.code : null
  return new IpcError("backend", code, payload.message, payload.detail ?? null, { cause: payload })
}

function errorFromRejection(reason: unknown): IpcError {
  const message =
    reason instanceof Error && reason.message.length > 0 ? reason.message : TRANSPORT_DIAGNOSTIC
  return new IpcError("transport", null, message, null, { cause: reason })
}

export function hasCode(error: unknown, code: IpcErrorCode): boolean {
  return error instanceof IpcError && error.code === code
}

type IpcResult<T> = { status: "ok"; data: T } | { status: "error"; error: unknown }

const sessionLostListeners = new Set<() => void>()

export function onSessionLost(listener: () => void): void {
  sessionLostListeners.add(listener)
}

function announceSessionLost(): void {
  for (const listener of sessionLostListeners) {
    listener()
  }
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
    announceSessionLost()
  }
  throw error
}

export function fireAndForget<T>(call: Promise<IpcResult<T>>): void {
  async function swallow(): Promise<void> {
    try {
      await unwrap(call)
    } catch {}
  }
  void swallow()
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

export function watchMaximized(onChange: (isMaximized: boolean) => void): () => void {
  const handle = getCurrentWindow()
  let isDisposed = false

  function push(): void {
    attemptWindowAction("isMaximized", async () => {
      const isMaximized = await handle.isMaximized()
      if (!isDisposed) {
        onChange(isMaximized)
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
