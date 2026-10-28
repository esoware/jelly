import { useEffect, useReducer, useRef } from "react"

import type {
  AddressRecord,
  Amount,
  AssetId,
  FeeSelection,
  PendingTxId,
  PreparedTx,
  SendAmount,
  TxId,
  TxPreview,
  TxRequest,
} from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import { isZeroAmount } from "@/lib/amount.ts"
import type { Parsed } from "@/lib/amount.ts"
import { chainMeta } from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { moneyKeys } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { txCommands, txKeys } from "@/query/tx.ts"
import { notify } from "@/store.ts"

const MAX_DESTINATION_TAG = 4_294_967_295

const DIGITS = /^[0-9]+$/u
const HEX_BODY = /^[0-9a-fA-F]*$/u

interface SendFormValues {
  sources: readonly AddressRecord[]
  asset: AssetId
  to: string
  amount: Parsed<Amount>
  shouldSendMax: boolean
  shouldDeleteAccount: boolean
  destinationTag: string
  comment: string
  nonce: string
  data: string
}

type SendFieldKey = "from" | "to" | "amount" | "destinationTag" | "nonce" | "data"

export type SendFormErrors = Partial<Record<SendFieldKey, string>>

type SendRequestResult =
  | { isOk: true; request: TxRequest }
  | { isOk: false; errors: SendFormErrors }

function parseNonce(raw: string): Parsed<number | null> {
  const text = raw.trim()
  if (text === "") {
    return { isOk: true, value: null }
  }
  if (!DIGITS.test(text)) {
    return { isOk: false, error: "Enter the nonce as a whole number." }
  }
  const value = Number(text)
  if (!Number.isSafeInteger(value)) {
    return { isOk: false, error: "Enter a smaller nonce." }
  }
  return { isOk: true, value }
}

function parseData(raw: string): Parsed<string | null> {
  const text = raw.trim()
  if (text === "") {
    return { isOk: true, value: null }
  }
  const body = text.startsWith("0x") || text.startsWith("0X") ? text.slice(2) : text
  if (!HEX_BODY.test(body) || body.length % 2 !== 0) {
    return { isOk: false, error: "Enter the data as an even number of hex digits." }
  }
  return { isOk: true, value: `0x${body.toLowerCase()}` }
}

export function buildTxRequest(values: SendFormValues, fee: FeeSelection): SendRequestResult {
  const errors: SendFormErrors = {}
  const { sources } = values
  const to = values.to.trim()

  if (sources.length === 0) {
    errors.from = "Choose a source address."
  }
  if (to === "") {
    errors.to = "Enter a recipient address."
  }

  let amount: SendAmount = { kind: "max" }
  if (!values.shouldSendMax && !values.shouldDeleteAccount) {
    const parsed = values.amount
    if (!parsed.isOk) {
      errors.amount = parsed.error
    } else if (isZeroAmount(parsed.value)) {
      errors.amount = "Enter an amount above zero."
    } else {
      amount = { kind: "exact", value: parsed.value }
    }
  }

  let destinationTag: number | null = null
  const tag = values.destinationTag.trim()
  if (tag !== "") {
    if (!DIGITS.test(tag) || Number(tag) > MAX_DESTINATION_TAG) {
      errors.destinationTag = `Enter a whole number from 0 to ${String(MAX_DESTINATION_TAG)}.`
    } else {
      destinationTag = Number(tag)
    }
  }

  const nonce = parseNonce(values.nonce)
  if (!nonce.isOk) {
    errors.nonce = nonce.error
  }
  const data = parseData(values.data)
  if (!data.isOk) {
    errors.data = data.error
  }

  const lead = sources.at(0)
  if (lead === undefined || !nonce.isOk || !data.isOk || Object.keys(errors).length > 0) {
    return { isOk: false, errors }
  }

  const chainFacts = chainMeta(lead.chain)
  return {
    isOk: true,
    request: {
      fromAddressIds: sources.map((row) => row.id),
      destinationTag,
      comment: chainFacts.hasComments && values.comment !== "" ? values.comment : null,
      shouldDeleteAccount: values.shouldDeleteAccount,
      to,
      asset: values.asset,
      amount,
      fee,
      nonce: chainFacts.canPinNonce ? nonce.value : null,
      data: chainFacts.canCarryData ? data.value : null,
    },
  }
}

export type SendState =
  | { status: "idle" }
  | { status: "preparing" }
  | { status: "preview"; prepared: PreparedTx; hasChanged: boolean; passwordError: string | null }
  | { status: "submitting"; prepared: PreparedTx; approved: TxPreview }
  | { status: "rebuilding"; approved: TxPreview }
  | { status: "submitted" }
  | { status: "error"; prepared: PreparedTx | null; message: string }

type SendAction =
  | { type: "reset" }
  | { type: "prepare" }
  | { type: "prepared"; prepared: PreparedTx }
  | { type: "confirm" }
  | { type: "expired" }
  | { type: "rebuilt"; prepared: PreparedTx; shouldResubmit: boolean }
  | { type: "submitted" }
  | { type: "password-rejected"; message: string }
  | { type: "failed"; message: string }

const IDLE_SEND: SendState = { status: "idle" }

export function preparedOf(state: SendState): PreparedTx | null {
  switch (state.status) {
    case "preview":
    case "submitting":
    case "error": {
      return state.prepared
    }
    case "idle":
    case "preparing":
    case "rebuilding":
    case "submitted": {
      return null
    }
  }
}

export function isSubmitting(state: SendState): boolean {
  return state.status === "submitting" || state.status === "rebuilding"
}

function discardableOf(state: SendState): PreparedTx | null {
  return state.status === "preview" || state.status === "error" ? state.prepared : null
}

function previewsEquivalent(a: TxPreview, b: TxPreview): boolean {
  return (
    a.chain === b.chain &&
    a.asset.kind === b.asset.kind &&
    a.asset.value === b.asset.value &&
    a.from.length === b.from.length &&
    a.from.every((address, i) => address === b.from.at(i)) &&
    a.to === b.to &&
    a.amount === b.amount &&
    a.fee === b.fee &&
    a.rent === b.rent &&
    a.destinationTag === b.destinationTag &&
    a.warnings.length === b.warnings.length &&
    a.warnings.every((warning, i) => warning.kind === b.warnings.at(i)?.kind)
  )
}

function sendReducer(state: SendState, action: SendAction): SendState {
  switch (action.type) {
    case "reset": {
      return IDLE_SEND
    }

    case "prepare": {
      return { status: "preparing" }
    }

    case "prepared": {
      return {
        status: "preview",
        prepared: action.prepared,
        hasChanged: false,
        passwordError: null,
      }
    }

    case "confirm": {
      return state.status === "preview"
        ? {
            status: "submitting",
            prepared: state.prepared,
            approved: state.prepared.preview,
          }
        : state
    }

    case "expired": {
      return state.status === "submitting"
        ? { status: "rebuilding", approved: state.approved }
        : state
    }

    case "rebuilt": {
      if (state.status !== "rebuilding") {
        return state
      }
      if (action.shouldResubmit) {
        return { status: "submitting", prepared: action.prepared, approved: state.approved }
      }
      return { status: "preview", prepared: action.prepared, hasChanged: true, passwordError: null }
    }

    case "submitted": {
      return { status: "submitted" }
    }

    case "password-rejected": {
      return state.status === "submitting"
        ? {
            status: "preview",
            prepared: state.prepared,
            hasChanged: false,
            passwordError: action.message,
          }
        : state
    }

    case "failed": {
      return { status: "error", prepared: preparedOf(state), message: action.message }
    }
  }
}

const MAX_AUTO_REBUILDS = 2

const EXHAUSTED_ERROR =
  "The transaction kept expiring and automatic retries were exhausted. Nothing was sent. Review it and try again."

interface SubmitVariables<TIntent> {
  intent: TIntent
  prepared: PreparedTx
  password: string
}

async function discardPending(pendingId: PendingTxId): Promise<void> {
  try {
    await txCommands.discard(pendingId)
  } catch {}
}

interface SendController<TIntent> {
  state: SendState
  isBusy: boolean
  isSubmitting: boolean
  hasPreview: boolean
  error: string | null
  start: (intent: TIntent) => void
  reset: () => void
  confirm: (password: string) => void
}

interface UseSendOptions<TIntent> {
  prepare: (intent: TIntent) => Promise<PreparedTx>
  prepareErrorTitle: string
  onSubmitted: () => void
}

export function useSend<TIntent>(options: UseSendOptions<TIntent>): SendController<TIntent> {
  const [state, dispatch] = useReducer(sendReducer, IDLE_SEND)
  const intentRef = useRef<TIntent | null>(null)
  const discardableRef = useRef<PreparedTx | null>(null)
  const generationRef = useRef(0)

  useEffect(() => {
    discardableRef.current = discardableOf(state)
  })

  useEffect(
    () => () => {
      if (discardableRef.current !== null) {
        void discardPending(discardableRef.current.id)
      }
    },
    [],
  )

  const prepare = useIpcMutation<PreparedTx, TIntent>({
    mutationFn: options.prepare,
    errorTitle: options.prepareErrorTitle,
  })

  async function attemptSubmit(
    variables: SubmitVariables<TIntent>,
    prepared: PreparedTx,
    rebuildCount: number,
  ): Promise<TxId | null> {
    try {
      return await txCommands.submit(prepared.id, variables.password, prepared.previewDigest)
    } catch (error) {
      if (!hasCode(error, "rebuild_required")) {
        throw error
      }
      if (rebuildCount >= MAX_AUTO_REBUILDS) {
        dispatch({ type: "failed", message: EXHAUSTED_ERROR })
        return null
      }
      const attempt = rebuildCount + 1
      dispatch({ type: "expired" })
      notify.warning("The transaction expired", "Nothing was sent. Preparing it again.")
      const rebuilt = await options.prepare(variables.intent)
      const shouldResubmit = previewsEquivalent(variables.prepared.preview, rebuilt.preview)
      dispatch({ type: "rebuilt", prepared: rebuilt, shouldResubmit })
      if (!shouldResubmit) {
        notify.warning("The transaction changed", "Review it and confirm again.")
        return null
      }
      return attemptSubmit(variables, rebuilt, attempt)
    }
  }

  const submit = useIpcMutation<TxId | null, SubmitVariables<TIntent>>({
    mutationFn: (variables) => attemptSubmit(variables, variables.prepared, 0),
    errorTitle: "Could not send the transaction",
    invalidates: [txKeys.root, txKeys.quotes, moneyKeys.root],
    onDone: (txid) => {
      if (txid !== null) {
        notify.success("The transaction was sent to the network")
        options.onSubmitted()
      }
    },
  })

  async function runPrepare(next: TIntent, startedGeneration: number): Promise<void> {
    try {
      const prepared = await prepare.mutateAsync(next)
      if (startedGeneration !== generationRef.current) {
        void discardPending(prepared.id)
        return
      }
      dispatch({ type: "prepared", prepared })
    } catch (error) {
      if (startedGeneration === generationRef.current) {
        dispatch({ type: "failed", message: describeFailure(error) })
      }
    }
  }

  async function runSubmit(variables: SubmitVariables<TIntent>): Promise<void> {
    try {
      const txid = await submit.mutateAsync(variables)
      if (txid !== null) {
        dispatch({ type: "submitted" })
      }
    } catch (error) {
      if (hasCode(error, "wrong_password")) {
        dispatch({ type: "password-rejected", message: describeFailure(error) })
        return
      }
      dispatch({ type: "failed", message: describeFailure(error) })
    }
  }

  function discardHeld(): void {
    const held = discardableOf(state)
    if (held !== null) {
      void discardPending(held.id)
    }
  }

  function start(next: TIntent): void {
    discardHeld()
    intentRef.current = next
    generationRef.current += 1
    dispatch({ type: "prepare" })
    void runPrepare(next, generationRef.current)
  }

  function reset(): void {
    if (isSubmitting(state)) {
      return
    }
    discardHeld()
    intentRef.current = null
    generationRef.current += 1
    dispatch({ type: "reset" })
  }

  function confirm(password: string): void {
    if (state.status !== "preview") {
      return
    }
    const currentIntent = intentRef.current
    if (currentIntent === null) {
      return
    }
    const { prepared } = state
    dispatch({ type: "confirm" })
    void runSubmit({ intent: currentIntent, prepared, password })
  }

  return {
    state,
    isBusy: state.status === "preparing" || isSubmitting(state),
    isSubmitting: isSubmitting(state),
    hasPreview: state.status === "preview" || isSubmitting(state),
    error: state.status === "error" ? state.message : null,
    start,
    reset,
    confirm,
  }
}
