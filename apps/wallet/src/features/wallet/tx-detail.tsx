import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import type { ReactNode } from "react"

import { Donut } from "@/features/wallet/donut.tsx"
import type { DonutSlice } from "@/features/wallet/donut.tsx"
import { NORMAL_FEE } from "@/features/wallet/fee-form.ts"
import { FeeSection, useFeeQuote } from "@/features/wallet/fee.tsx"
import { ReviewStep } from "@/features/wallet/send-review.tsx"
import { useSend } from "@/features/wallet/send-state.ts"
import type {
  ChainId,
  FeeSelection,
  FiatCurrency,
  PreparedTx,
  TxEntry,
  TxId,
} from "@/ipc/bindings.gen.ts"
import { amountToBigInt } from "@/lib/amount.ts"
import { assetChain, assetDecimals, chainMeta, explorerTxUrl, nativeAsset } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { HISTORICAL_PRICE_NOTICE, fiatValue, isPriceStale, priceForAsset } from "@/lib/fiat.ts"
import { dayKey, formatClock, formatDay } from "@/lib/format.ts"
import {
  TX_PROGRESS_LABELS,
  canBumpFee,
  canCancel,
  directionSign,
  isAmountUnknown,
  outcomeLabel,
  shownAmount,
  shownDirection,
  txProgress,
  txTitle,
} from "@/lib/tx.ts"
import { addressQueries } from "@/query/address.ts"
import { txCommands } from "@/query/tx.ts"
import type { QuoteTarget } from "@/query/tx.ts"
import { useHistoricalPrice, usePrices } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import {
  Dialog,
  DialogBody,
  DialogDescription,
  DialogFooter,
  DialogFooterNote,
  DialogForm,
  DialogHeader,
  DialogPopup,
  DialogTitle,
  DialogTransition,
} from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Identifier } from "@/ui/identifier.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { ExplorerActions } from "@/widgets/address.tsx"
import { CryptoAmount, FiatValue } from "@/widgets/amounts.tsx"
import { CopyButton, DetailList } from "@/widgets/copyable.tsx"

interface TxFiatValues {
  currency: FiatCurrency
  currentValue: bigint | null
  historicalValue: bigint | null
  isCurrentStale: boolean
}

export function useTxFiat(row: TxEntry): TxFiatValues {
  const prices = usePrices()
  const isPreviousDay =
    row.confirmations > 0 &&
    row.timestamp !== null &&
    row.timestamp < prices.judgedAtUnixSecs &&
    dayKey(row.timestamp) !== dayKey(prices.judgedAtUnixSecs)
  const historicalPrice = useHistoricalPrice(
    row.asset,
    prices.currency,
    isPreviousDay ? row.timestamp : null,
  )
  const raw = isAmountUnknown(row) ? null : amountToBigInt(shownAmount(row))
  const signed = raw === null ? null : raw * directionSign(shownDirection(row))
  const decimals = assetDecimals(row.asset)
  const livePrice = priceForAsset(row.asset, prices)

  return {
    currency: prices.currency,
    currentValue: livePrice === null ? null : fiatValue(signed, decimals, livePrice),
    historicalValue: historicalPrice === null ? null : fiatValue(signed, decimals, historicalPrice),
    isCurrentStale: isPriceStale(row.asset, prices),
  }
}

interface TxAmountProps {
  row: TxEntry
  className?: string | undefined
}

export function TxAmount({ row, className }: TxAmountProps) {
  if (isAmountUnknown(row)) {
    return <span className={appendClassName("text-fg-faint", className)}>Amount pending</span>
  }
  return (
    <CryptoAmount
      amount={shownAmount(row)}
      asset={row.asset}
      direction={shownDirection(row)}
      className={className}
    />
  )
}

const MAX_STEP_SLICES = 6
const CONFIRMED_COLOR = "var(--color-ok)"
const REMAINING_COLOR = "var(--color-edge)"

function progressSlices(confirmations: number, settled: number): DonutSlice[] {
  if (settled > MAX_STEP_SLICES) {
    return [
      { id: "confirmed", weight: confirmations, color: CONFIRMED_COLOR },
      { id: "remaining", weight: settled - confirmations, color: REMAINING_COLOR },
    ]
  }
  return Array.from({ length: settled }, (_, step) => ({
    id: String(step),
    weight: 1,
    color: step < confirmations ? CONFIRMED_COLOR : REMAINING_COLOR,
  }))
}

interface ConfirmationMarkProps {
  chain: ChainId
  confirmations: number
}

export function ConfirmationMark({ chain, confirmations }: ConfirmationMarkProps) {
  const progress = txProgress(chain, confirmations)
  if (progress === "confirmed") {
    return null
  }
  const settled = chainMeta(chain).settledConfirmations
  return (
    <TooltipText
      content={
        progress === "pending"
          ? "Not in a block yet"
          : `${String(confirmations)} of ${String(settled)} confirmations`
      }
      render={<span className="flex shrink-0 self-center" />}
    >
      {progress === "pending" ? (
        <span className="size-icon-sm animate-pulse rounded-full border-2 border-warn motion-reduce:animate-none" />
      ) : (
        <Donut
          slices={progressSlices(confirmations, settled)}
          size="pie"
          className="size-icon-sm"
        />
      )}
    </TooltipText>
  )
}

interface TxFactProps {
  label: string
  copy?: string | undefined
  children: ReactNode
}

function TxFact({ label, copy, children }: TxFactProps) {
  return (
    <div className="flex min-w-0 items-baseline gap-2">
      <span className="w-24 shrink-0 text-fg-faint">{label}</span>
      <span className="flex min-w-0 flex-1 items-start gap-1 text-fg-muted">
        {copy === undefined ? (
          children
        ) : (
          <>
            <span className="min-w-0 font-mono break-all select-text">{children}</span>
            <CopyButton value={copy} />
          </>
        )}
      </span>
    </div>
  )
}

interface TxAddressesFactProps {
  label: string
  addresses: readonly string[]
}

function TxAddressesFact({ label, addresses }: TxAddressesFactProps) {
  const [first, ...rest] = addresses
  if (first === undefined) {
    return null
  }
  if (rest.length === 0) {
    return (
      <TxFact label={label} copy={first}>
        <Identifier value={first} />
      </TxFact>
    )
  }
  return (
    <TxFact label={label}>
      <span className="flex min-w-0 flex-1 flex-col gap-1">
        {addresses.map((address) => (
          <span key={address} className="flex min-w-0 items-start gap-1">
            <Identifier value={address} className="min-w-0 font-mono break-all select-text" />
            <CopyButton value={address} />
          </span>
        ))}
      </span>
    </TxFact>
  )
}

function statusText(row: TxEntry, chain: ChainId): string {
  return outcomeLabel(row) ?? TX_PROGRESS_LABELS[txProgress(chain, row.confirmations)]
}

function confirmationsText(row: TxEntry, chain: ChainId): string {
  const settled = chainMeta(chain).settledConfirmations
  return row.confirmations < settled
    ? `${String(row.confirmations)} of ${String(settled)}`
    : String(row.confirmations)
}

interface TxFactsProps {
  row: TxEntry
  onBumpFee: () => void
  onCancel: () => void
}

function TxFacts({ row, onBumpFee, onCancel }: TxFactsProps) {
  const chain = assetChain(row.asset)
  const fiat = useTxFiat(row)
  const ownAddresses = useQuery(addressQueries.list(chain))
  const ownRows = ownAddresses.data ?? []
  function addressesOf(ids: readonly number[]): string[] {
    return ids.flatMap((id) =>
      ownRows.filter((entry) => entry.id === id).map((entry) => entry.address),
    )
  }
  const counterparties = row.counterparty === null ? [] : [row.counterparty]
  const senders = addressesOf(row.senderIds)
  const receivers = addressesOf(row.receiverIds)
  const fromAddresses = row.direction === "incoming" ? counterparties : senders
  let toAddresses = counterparties
  if (row.direction === "incoming" || (row.direction === "self" && receivers.length > 0)) {
    toAddresses = receivers
  }
  const explorer = explorerTxUrl(chain, row.txid)
  const canBump = canBumpFee(row)
  const isCancellable = canCancel(row)
  const hasNotRun = outcomeLabel(row) !== null
  const time =
    row.timestamp === null
      ? "Time unavailable"
      : `${formatDay(row.timestamp)} at ${formatClock(row.timestamp)}`

  return (
    <div className="space-y-1 text-caption">
      <TxFact label="Transaction" copy={row.txid}>
        <Identifier value={row.txid} />
      </TxFact>
      {row.paysFor === null ? null : (
        <TxFact label="Speeds up" copy={row.paysFor}>
          <Identifier value={row.paysFor} />
        </TxFact>
      )}
      <TxAddressesFact label="From" addresses={fromAddresses} />
      <TxAddressesFact label="To" addresses={toAddresses} />
      <TxFact label="Time">{time}</TxFact>
      <TxFact label="Status">
        <span className="flex items-baseline gap-1.5">
          {hasNotRun ? null : <ConfirmationMark chain={chain} confirmations={row.confirmations} />}
          {statusText(row, chain)}
        </span>
      </TxFact>
      {row.confirmations > 0 ? (
        <TxFact label="Confirmations">{confirmationsText(row, chain)}</TxFact>
      ) : null}
      {row.height === null ? null : <TxFact label="Block">{String(row.height)}</TxFact>}
      {row.fee === null || row.outcome === "replaced" ? null : (
        <TxFact label="Network fee">
          <CryptoAmount amount={row.fee} asset={nativeAsset(chain)} hasFiat />
        </TxFact>
      )}
      {fiat.currentValue === null && fiat.historicalValue === null ? null : (
        <>
          <TxFact label="Value then">
            {fiat.historicalValue === null ? (
              "Not available"
            ) : (
              <FiatValue
                value={fiat.historicalValue}
                currency={fiat.currency}
                notice={HISTORICAL_PRICE_NOTICE}
                isSigned
              />
            )}
          </TxFact>
          <TxFact label="Value now">
            {fiat.currentValue === null ? (
              "Not available"
            ) : (
              <FiatValue
                value={fiat.currentValue}
                currency={fiat.currency}
                isStale={fiat.isCurrentStale}
                isSigned
              />
            )}
          </TxFact>
        </>
      )}
      {row.detection === "balance_delta" ? (
        <TxFact label="Detail">Only the balance change was seen.</TxFact>
      ) : null}

      {explorer === null && !canBump && !isCancellable ? null : (
        <div className="flex flex-wrap gap-2 pt-1">
          {explorer === null ? null : (
            <ExplorerActions
              url={explorer}
              onOpen={() => txCommands.openExplorer({ chain, value: row.txid })}
            />
          )}
          {canBump ? (
            <Button size="sm" variant="secondary" onClick={onBumpFee}>
              <Icon name="speed" />
              Bump fee
            </Button>
          ) : null}
          {isCancellable ? (
            <Button size="sm" variant="secondary" onClick={onCancel}>
              <Icon name="close" />
              Cancel
            </Button>
          ) : null}
        </div>
      )}
    </div>
  )
}

type Replacement = "bump" | "cancel"

const COPY = {
  bump: {
    title: "Bump fee",
    description:
      "Pay a higher fee so this transaction confirms sooner. The review shows how the fee is paid before you sign.",
    amountLabel: "Amount",
  },
  cancel: {
    title: "Cancel transaction",
    description:
      "Send nothing to yourself in this transaction's place, so it never confirms. If it confirms first, the cancel never runs and costs nothing.",
    amountLabel: "Amount not sent",
  },
} as const satisfies Record<
  Replacement,
  { title: string; description: string; amountLabel: string }
>

interface ReplacementStepProps {
  replacement: Replacement
  row: TxEntry
  isBusy: boolean
  error: string | null
  onBack: () => void
  onSubmit: () => void
  children: ReactNode
}

function ReplacementStep({
  replacement,
  row,
  isBusy,
  error,
  onBack,
  onSubmit,
  children,
}: ReplacementStepProps) {
  const copy = COPY[replacement]
  return (
    <DialogForm
      onSubmit={() => {
        if (!isBusy) {
          onSubmit()
        }
      }}
    >
      <DialogHeader>
        <DialogTitle>{copy.title}</DialogTitle>
        <DialogDescription>{copy.description}</DialogDescription>
      </DialogHeader>

      <DialogBody>
        <DetailList
          items={[
            {
              label: "Current fee",
              value: (
                <CryptoAmount amount={row.fee} asset={nativeAsset(assetChain(row.asset))} hasFiat />
              ),
            },
            {
              label: copy.amountLabel,
              value: <CryptoAmount amount={row.amount} asset={row.asset} hasFiat />,
            },
          ]}
        />
        {children}
      </DialogBody>

      <DialogFooter>
        {error === null ? null : <DialogFooterNote tone="danger">{error}</DialogFooterNote>}
        <Button variant="secondary" onClick={onBack}>
          Back
        </Button>
        <Button type="submit" isLoading={isBusy}>
          Review
        </Button>
      </DialogFooter>
    </DialogForm>
  )
}

const STEPS = ["details", "fee", "review"] as const

type Step = (typeof STEPS)[number]

interface ReplacementIntent {
  replacement: Replacement
  txid: TxId
  fee: FeeSelection
}

function prepareReplacement(intent: ReplacementIntent): Promise<PreparedTx> {
  switch (intent.replacement) {
    case "bump": {
      return txCommands.bumpFee(intent.txid, intent.fee)
    }
    case "cancel": {
      return txCommands.cancel(intent.txid, intent.fee)
    }
  }
}

interface TxDetailContentProps {
  row: TxEntry
  title: string
  onClose: () => void
}

function TxDetailContent({ row, title, onClose }: TxDetailContentProps) {
  const chain = assetChain(row.asset)
  const [replacement, setReplacement] = useState<Replacement | null>(null)

  const [fee, setFee] = useState<FeeSelection>(NORMAL_FEE)
  const txid: TxId = { chain, value: row.txid }
  const quoteTarget: QuoteTarget = { kind: replacement ?? "bump", txid }
  const feeQuote = useFeeQuote(quoteTarget, replacement === null ? null : fee)

  const flow = useSend<ReplacementIntent>({
    prepare: prepareReplacement,
    prepareErrorTitle:
      replacement === "cancel" ? "Could not build the cancel" : "Could not build the replacement",
    onSubmitted: onClose,
  })

  function begin(next: Replacement): void {
    setFee(NORMAL_FEE)
    setReplacement(next)
  }

  let step: Step = "details"
  let content: ReactNode = (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>
          <TxAmount row={row} />
        </DialogDescription>
      </DialogHeader>
      <DialogBody hasFooter={false}>
        <TxFacts
          row={row}
          onBumpFee={() => {
            begin("bump")
          }}
          onCancel={() => {
            begin("cancel")
          }}
        />
      </DialogBody>
    </>
  )
  if (flow.hasPreview) {
    step = "review"
    content = (
      <ReviewStep
        state={flow.state}
        onBack={() => {
          flow.reset()
        }}
        onConfirm={(password) => {
          flow.confirm(password)
        }}
      />
    )
  } else if (replacement !== null) {
    step = "fee"
    content = (
      <ReplacementStep
        replacement={replacement}
        row={row}
        isBusy={flow.isBusy}
        error={flow.error}
        onBack={() => {
          flow.reset()
          setReplacement(null)
        }}
        onSubmit={() => {
          flow.start({ replacement, txid, fee })
        }}
      >
        <FeeSection
          chain={chain}
          target={quoteTarget}
          fee={fee}
          quote={feeQuote}
          onFeeChange={setFee}
        />
      </ReplacementStep>
    )
  }

  return (
    <DialogTransition step={STEPS.indexOf(step)} isLocked={flow.isSubmitting}>
      {content}
    </DialogTransition>
  )
}

interface TxDetailDialogProps extends DialogStateProps {
  row: TxEntry | null
  shouldNameNative?: boolean | undefined
}

export function TxDetailDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  row,
  shouldNameNative = false,
}: TxDetailDialogProps) {
  if (row === null) {
    return null
  }
  return (
    <Dialog open={open} onOpenChange={onOpenChange} onOpenChangeComplete={onOpenChangeComplete}>
      <DialogPopup>
        <TxDetailContent
          row={row}
          title={txTitle(row, shouldNameNative)}
          onClose={() => {
            onOpenChange(false)
          }}
        />
      </DialogPopup>
    </Dialog>
  )
}
