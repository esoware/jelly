import { useState } from "react"
import type { ComponentProps } from "react"

import { isSubmitting, preparedOf } from "@/features/wallet/send-state.ts"
import type { SendState } from "@/features/wallet/send-state.ts"
import type { PendingTxId, TxPreview, TxWarningKind } from "@/ipc/bindings.gen.ts"
import { addAmounts } from "@/lib/amount.ts"
import { chainMeta, nativeAsset } from "@/lib/chain.ts"
import { useFiatCurrency, useFiatValue, useIsFiatStale } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Card } from "@/ui/card.tsx"
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
} from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Identifier } from "@/ui/identifier.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { AddressList } from "@/widgets/address.tsx"
import { CryptoAmount, FiatAmount, FiatValue } from "@/widgets/amounts.tsx"
import { CopyButton, DetailList, IdentifierWithCopy } from "@/widgets/copyable.tsx"
import type { DetailRow } from "@/widgets/copyable.tsx"

interface SourceListProps {
  sources: readonly string[]
}

function SourceList({ sources }: SourceListProps) {
  return (
    <span className="flex flex-col gap-1">
      {sources.map((source) => (
        <span key={source} className="flex items-baseline gap-2">
          <Identifier value={source} className="min-w-0 flex-1 break-all" />
          <CopyButton value={source} />
        </span>
      ))}
    </span>
  )
}

interface TotalAmountProps {
  preview: TxPreview
}

function TotalAmount({ preview }: TotalAmountProps) {
  if (preview.asset.kind === "native") {
    return <CryptoAmount amount={addAmounts(preview.amount, preview.fee)} asset={preview.asset} />
  }
  return (
    <>
      <CryptoAmount amount={preview.amount} asset={preview.asset} />
      <span>+</span>
      <CryptoAmount amount={preview.fee} asset={nativeAsset(preview.chain)} />
    </>
  )
}

interface TotalFiatProps {
  preview: TxPreview
}

function TotalFiat({ preview }: TotalFiatProps) {
  const native = nativeAsset(preview.chain)
  const amountValue = useFiatValue(preview.amount, preview.asset)
  const feeValue = useFiatValue(preview.fee, native)
  const currency = useFiatCurrency()
  const isAssetStale = useIsFiatStale(preview.asset)
  const isFeeStale = useIsFiatStale(native)
  if (amountValue === null || feeValue === null) {
    return null
  }
  return (
    <FiatValue
      value={amountValue + feeValue}
      currency={currency}
      isStale={isAssetStale || isFeeStale}
      className="text-caption text-fg-faint"
    />
  )
}

interface DetailsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  items: readonly DetailRow[]
}

function DetailsDialog({ open, onOpenChange, items }: DetailsDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup size="lg">
        <DialogHeader>
          <DialogTitle>Transaction details</DialogTitle>
        </DialogHeader>
        <DialogBody hasFooter={false}>
          <DetailList items={items} />
        </DialogBody>
      </DialogPopup>
    </Dialog>
  )
}

const WARNING_CALLOUTS = {
  dust_output: { tone: "danger", heading: "Payment below the network minimum" },
  high_fee: { tone: "warn", heading: "Check the transaction fee" },
  dropped_change: { tone: "warn", heading: "No change returned" },
  ambiguous_address: { tone: "danger", heading: "Check the recipient address" },
  remote_decoys: { tone: "warn", heading: "Remote node privacy" },
  account_activation: { tone: "warn", heading: "New recipient account" },
  account_deletion: { tone: "danger", heading: "Account deletion" },
  rent_exemption: { tone: "warn", heading: "Account rent requirement" },
  replacement: { tone: "warn", heading: "Transaction replacement" },
  child_pays: { tone: "warn", heading: "Speed-up transaction" },
  estimated: { tone: "warn", heading: "Estimated values" },
  other: { tone: "warn", heading: "Review before sending" },
} satisfies Record<TxWarningKind, Pick<ComponentProps<typeof Callout>, "tone" | "heading">>

interface ReviewStepProps {
  state: SendState
  onBack: () => void
  onConfirm: (password: string) => void
}

export function ReviewStep({ state, onBack, onConfirm }: ReviewStepProps) {
  const [password, setPassword] = useState("")
  const [isDetailsOpen, setIsDetailsOpen] = useState(false)
  const changedId = state.status === "preview" && state.hasChanged ? state.prepared.id : null
  const [clearedForId, setClearedForId] = useState<PendingTxId | null>(null)
  if (changedId !== null && changedId !== clearedForId) {
    setClearedForId(changedId)
    setPassword("")
  }
  let preview = preparedOf(state)?.preview ?? null
  if (state.status === "rebuilding") {
    preview = state.approved
  }
  const passwordError = state.status === "preview" ? state.passwordError : null
  const isSending = isSubmitting(state)
  const canConfirm = preview !== null && password !== "" && !isSending

  return (
    <DialogForm
      onSubmit={() => {
        if (canConfirm) {
          onConfirm(password)
        }
      }}
    >
      <DialogHeader>
        <DialogTitle>Review transaction</DialogTitle>
        <DialogDescription>Sending cannot be undone.</DialogDescription>
      </DialogHeader>

      <DialogBody>
        {preview === null ? null : (
          <>
            {preview.warnings.map((warning, i) => (
              <Callout key={`${String(i)}:${warning.kind}`} {...WARNING_CALLOUTS[warning.kind]}>
                {warning.message}
              </Callout>
            ))}

            <Card className="flex flex-col gap-1 p-3 text-body">
              <p className="flex flex-wrap items-baseline gap-x-2">
                <CryptoAmount
                  amount={preview.amount}
                  asset={preview.asset}
                  className="text-headline"
                />
                <FiatAmount
                  amount={preview.amount}
                  asset={preview.asset}
                  className="text-fg-muted"
                />
              </p>
              <div className="flex min-w-0 items-center gap-2">
                <span className="text-caption text-fg-faint">From</span>
                <AddressList addresses={preview.from} className="font-mono select-text" />
              </div>
              <div className="flex min-w-0 items-center gap-2">
                <span className="text-caption text-fg-faint">To</span>
                <IdentifierWithCopy value={preview.to} />
              </div>
              {chainMeta(preview.chain).hasDestinationTags ? (
                <div className="flex min-w-0 items-center gap-2">
                  <span className="text-caption text-fg-faint">Destination tag</span>
                  <span>
                    {preview.destinationTag === null ? "None" : String(preview.destinationTag)}
                  </span>
                </div>
              ) : null}
              <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
                <span className="flex items-baseline gap-1">
                  <span className="text-caption text-fg-faint">Fee</span>
                  <CryptoAmount amount={preview.fee} asset={nativeAsset(preview.chain)} />
                  <FiatAmount
                    amount={preview.fee}
                    asset={nativeAsset(preview.chain)}
                    className="text-caption text-fg-faint"
                  />
                </span>
                <span className="flex items-baseline gap-1">
                  <span className="text-caption text-fg-faint">Total</span>
                  <span className="flex flex-wrap items-baseline gap-1 font-semibold">
                    <TotalAmount preview={preview} />
                  </span>
                  <TotalFiat preview={preview} />
                </span>
              </div>
            </Card>
          </>
        )}

        <Field label="Password" error={passwordError}>
          <PasswordInput
            autoFocus
            autoComplete="current-password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value)
            }}
          />
        </Field>
      </DialogBody>

      <DialogFooter>
        <Button
          variant="ghost"
          className="mr-auto"
          onClick={() => {
            setIsDetailsOpen(true)
          }}
        >
          Details
        </Button>
        {changedId === null ? null : (
          <DialogFooterNote tone="warn">
            The transaction changed after it expired. Review it and confirm again.
          </DialogFooterNote>
        )}
        <Button variant="secondary" disabled={isSending} onClick={onBack}>
          Back
        </Button>
        <Button type="submit" isLoading={isSending} disabled={!canConfirm}>
          Sign and send
        </Button>
      </DialogFooter>

      <DetailsDialog
        open={isDetailsOpen}
        onOpenChange={setIsDetailsOpen}
        items={
          preview === null
            ? []
            : [
                { label: "From", value: <SourceList sources={preview.from} /> },
                ...preview.details.map((row) => ({ label: row.label, value: row.value })),
              ]
        }
      />
    </DialogForm>
  )
}
