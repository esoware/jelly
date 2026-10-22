import type { Amount, ChainId, TxDirection, TxEntry } from "@/ipc/bindings.gen.ts"
import { isZeroAmount } from "@/lib/amount.ts"
import {
  assetChain,
  assetDecimals,
  assetKey,
  assetSymbol,
  chainMeta,
  formatAssetAmount,
} from "@/lib/chain.ts"
import { fiatValue, formatFiat, priceForAsset } from "@/lib/fiat.ts"
import type { Prices } from "@/lib/fiat.ts"
import { dayKey, formatDay, truncateMiddle } from "@/lib/format.ts"

const TXID_HEAD_CHARS = 8
const TXID_TAIL_CHARS = 6

export const DIRECTION_TITLES: Record<TxDirection, string> = {
  incoming: "Received",
  outgoing: "Sent",
  self: "Self transfer",
}

export function txTitle(row: TxEntry, shouldNameNative: boolean): string {
  const title = row.paysFor === null ? DIRECTION_TITLES[row.direction] : "Speed-up"
  return row.asset.kind === "token" || shouldNameNative
    ? `${title} ${assetSymbol(row.asset)}`
    : title
}

export const DIRECTION_PARTIES: Record<TxDirection, string> = {
  incoming: "From",
  outgoing: "To",
  self: "Between",
}

export const TX_DIRECTIONS: readonly TxDirection[] = ["incoming", "outgoing", "self"]

export function isTxDirection(value: string): value is TxDirection {
  return Object.hasOwn(DIRECTION_TITLES, value)
}

export function directionSign(direction: TxDirection): bigint {
  return direction === "outgoing" ? -1n : 1n
}

function txKey(row: TxEntry): string {
  return `${row.txid}:${assetKey(row.asset)}`
}

type TxProgress = "pending" | "confirming" | "confirmed"

export const TX_PROGRESS_LABELS: Record<TxProgress, string> = {
  pending: "Pending",
  confirming: "Confirming",
  confirmed: "Confirmed",
}

export function txProgress(chain: ChainId, confirmations: number): TxProgress {
  if (confirmations <= 0) {
    return "pending"
  }
  return confirmations < chainMeta(chain).settledConfirmations ? "confirming" : "confirmed"
}

type TxListEntry =
  | { kind: "day"; key: string; label: string }
  | { kind: "tx"; key: string; row: TxEntry }

export function txListEntries(rows: readonly TxEntry[]): TxListEntry[] {
  const now = new Date()
  const entries: TxListEntry[] = []
  let currentDay: string | null = null
  for (const row of rows) {
    const day = dayKey(row.timestamp)
    if (day !== currentDay) {
      currentDay = day
      entries.push({
        kind: "day",
        key: `${day}:${txKey(row)}`,
        label: formatDay(row.timestamp, now),
      })
    }
    entries.push({ kind: "tx", key: txKey(row), row })
  }
  return entries
}

export function outcomeLabel(row: TxEntry): string | null {
  switch (row.outcome) {
    case "failed": {
      return "Failed"
    }
    case "replaced": {
      return "Replaced"
    }
    case "success":
    case "unknown": {
      return null
    }
  }
}

function hasNotRun(row: TxEntry): boolean {
  return outcomeLabel(row) !== null
}

export function isAmountUnknown(row: TxEntry): boolean {
  return (
    row.detection === "balance_delta" ||
    (row.paysFor === null && row.confirmations <= 0 && !hasNotRun(row) && isZeroAmount(row.amount))
  )
}

export function shownAmount(row: TxEntry): Amount | null {
  return row.paysFor === null ? row.amount : row.fee
}

export function shownDirection(row: TxEntry): TxDirection {
  return row.paysFor === null ? row.direction : "outgoing"
}

function isPendingSend(row: TxEntry): boolean {
  return (
    row.confirmations <= 0 &&
    !hasNotRun(row) &&
    (row.direction === "outgoing" || row.direction === "self")
  )
}

export function canBumpFee(row: TxEntry): boolean {
  return isPendingSend(row) && chainMeta(assetChain(row.asset)).canBumpFee
}

export function canCancel(row: TxEntry): boolean {
  return isPendingSend(row) && chainMeta(assetChain(row.asset)).canCancel
}

export function describeIncomingTx(row: TxEntry, prices: Prices): string {
  const price = priceForAsset(row.asset, prices)
  const value = price === null ? null : fiatValue(row.amount, assetDecimals(row.asset), price)
  const fiat = value === null ? "" : ` (${formatFiat(value, prices.currency)})`
  const txid = truncateMiddle(row.txid, TXID_HEAD_CHARS, TXID_TAIL_CHARS)
  return `${formatAssetAmount(row.amount, row.asset)}${fiat} - ${txid}`
}
