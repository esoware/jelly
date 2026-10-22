import type {
  EndpointAttempt,
  FailureKind,
  IpcErrorCode,
  IpcErrorDetail,
  RejectionKind,
  RetryPolicy,
} from "@/ipc/bindings.gen.ts"
import { IpcError } from "@/ipc/client.ts"
import type { IpcErrorKind } from "@/ipc/client.ts"
import { chainName, formatAssetAmount } from "@/lib/chain.ts"

const MAX_LISTED_ATTEMPTS = 3

const INTERNAL_FAILURE = "Jelly encountered an internal error."

const CODE_MESSAGES: Record<IpcErrorCode, string> = {
  starting: "Jelly is still starting.",
  locked: "The wallet is locked.",
  wrong_password: "The password is incorrect.",
  wrong_passphrase: "The passphrase is incorrect.",
  passphrase_required: "Enter the wallet passphrase.",
  not_found: "That record no longer exists.",
  already_exists: "That already exists.",
  invalid_input: "Check the value and try again.",
  invalid_address: "Enter a valid address for this chain.",
  insufficient_funds: "The balance is too low for this transfer.",
  endpoints_down: "No endpoint responded. Check your endpoint settings.",
  rpc: "The chain returned an error.",
  no_adapter: "This chain is not enabled for this wallet.",
  unsupported: "This chain does not support that operation.",
  confirmation_required: "The confirmation text did not match.",
  unknown_pending_tx: "This transaction is no longer ready to send. Review it again.",
  rebuild_required: "The transaction expired. Review it again before sending.",
  rejected: "The network refused the transaction.",
  signing: "The transaction could not be signed.",
  protocol_too_new: "Update Jelly to use this endpoint's protocol.",
  address_mismatch: "The key does not match the saved address.",
  storage: "Jelly could not access the wallet database.",
  keychain: "Jelly could not access the system keychain.",
  prices_unavailable: "Prices are unavailable right now.",
  panic: INTERNAL_FAILURE,
  internal: INTERNAL_FAILURE,
}

const KIND_MESSAGES: Record<IpcErrorKind, string> = {
  backend: "Jelly could not complete the request.",
  transport: "Jelly could not reach the wallet engine.",
  unrecognized: INTERNAL_FAILURE,
}

const ATTEMPT_FAILURES: Record<FailureKind, string> = {
  connect: "could not be reached",
  tls: "could not establish a secure connection",
  timeout: "timed out",
  transport: "dropped the connection",
  protocol: "returned an unreadable response",
  rate_limited: "received too many requests",
  server: "reported a server error",
  client: "refused the request",
}

const REJECTION_MESSAGES: Record<RejectionKind, string> = {
  already_known: "The network already has this transaction.",
  below_relay_fee: "The fee is below the network minimum.",
  below_incremental_relay_fee: "Increase the fee further to replace the earlier transaction.",
  missing_or_spent: "A required coin or account is missing or already spent.",
  insufficient_funds: "The balance cannot cover the payment and fee.",
  double_spend: "These funds have already been spent.",
  dust: "A transaction output is below the network minimum.",
  too_long_mempool_chain: "This transaction depends on too many unconfirmed transactions.",
  nonce_too_low: "That nonce has already been used.",
  nonce_too_high: "An earlier transaction from this account has not reached the network.",
  replacement_underpriced: "Increase the fee further to replace the earlier transaction.",
  gas_too_low: "The gas limit is too low for this transaction.",
  stale_reference: "The transaction refers to an expired block.",
  reverted: "The contract rejected this transaction.",
  unclassified: "The network refused the transaction.",
}

const RETRY_MESSAGES: Record<RetryPolicy, string> = {
  did_not_land: "Nothing was sent. Review the transaction and try again.",
  final_everywhere: "The transaction must change before you retry.",
  try_another_endpoint: "Another endpoint may still accept it.",
  already_landed: "Do not send it again.",
}

function describeAttempts(attempts: readonly EndpointAttempt[]): string {
  const listed = attempts
    .slice(0, MAX_LISTED_ATTEMPTS)
    .map((attempt) => `${attempt.endpoint} ${ATTEMPT_FAILURES[attempt.kind]}`)
    .join(", ")
  const unlistedCount = attempts.length - MAX_LISTED_ATTEMPTS
  return unlistedCount > 0 ? `${listed}, and ${String(unlistedCount)} more` : listed
}

function describeDetail(detail: IpcErrorDetail): string {
  switch (detail.kind) {
    case "insufficient_funds": {
      return `You need at least ${formatAssetAmount(detail.needed, detail.asset)}. You have ${formatAssetAmount(detail.available, detail.asset)}.`
    }
    case "endpoints_down": {
      if (detail.attempts.length === 0) {
        return `No ${chainName(detail.chain)} endpoint is available. Check your endpoint settings.`
      }
      return `Jelly could not reach ${chainName(detail.chain)}. Endpoint failures: ${describeAttempts(detail.attempts)}.`
    }
    case "rejected": {
      return `${REJECTION_MESSAGES[detail.rejection]} ${RETRY_MESSAGES[detail.retry]}`
    }
    case "no_adapter": {
      return `${chainName(detail.chain)} is not enabled for this wallet.`
    }
    case "protocol_too_new": {
      return `This endpoint uses ${chainName(detail.chain)} protocol v${detail.version}. Update Jelly to use it.`
    }
    case "schema_too_new": {
      return `This database needs a newer version of Jelly. Its schema is v${String(detail.found)}. This version supports v${String(detail.supported)}.`
    }
  }
}

export function describeFailure(error: unknown): string {
  if (!(error instanceof IpcError)) {
    return INTERNAL_FAILURE
  }
  if (error.detail !== null) {
    return describeDetail(error.detail)
  }
  if (error.code === "invalid_input" && error.message.length > 0) {
    return error.message
  }
  return error.code === null ? KIND_MESSAGES[error.kind] : CODE_MESSAGES[error.code]
}
