import { useQuery, useSuspenseQuery } from "@tanstack/react-query"
import { useDeferredValue, useState } from "react"
import type { ReactNode } from "react"

import { NORMAL_FEE, withEstimatedGas } from "@/features/wallet/fee-form.ts"
import { FeeSection, quoteOf, useFeeQuote } from "@/features/wallet/fee.tsx"
import {
  AdvancedFields,
  AmountField,
  RecipientField,
  SourceFields,
} from "@/features/wallet/send-fields.tsx"
import { ReviewStep } from "@/features/wallet/send-review.tsx"
import { buildTxRequest, useSend } from "@/features/wallet/send-state.ts"
import type { SendFormErrors } from "@/features/wallet/send-state.ts"
import type {
  Amount,
  AmountEntry,
  AssetId,
  ChainId,
  FeeSelection,
  TxRequest,
} from "@/ipc/bindings.gen.ts"
import { ZERO_AMOUNT, addAmounts, toBaseUnits } from "@/lib/amount.ts"
import { assetDecimals, assetLabel, chainMeta } from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { fiatToBaseUnits } from "@/lib/fiat.ts"
import type { Parsed } from "@/lib/format.ts"
import { addressBalance } from "@/lib/portfolio.ts"
import { addressQueries } from "@/query/address.ts"
import { moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { txCommands } from "@/query/tx.ts"
import type { QuoteTarget } from "@/query/tx.ts"
import { useFiatCurrency, usePrice } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import {
  Dialog,
  DialogBody,
  DialogFooter,
  DialogFooterNote,
  DialogForm,
  DialogHeader,
  DialogPopup,
  DialogTitle,
  DialogTransition,
} from "@/ui/dialog.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Field } from "@/ui/field.tsx"
import { Input } from "@/ui/input.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { Switch } from "@/ui/switch.tsx"
import type { ListState } from "@/widgets/list-status.tsx"

interface SourceGateProps {
  state: ListState
  hasSource: boolean
  children: ReactNode
}

function SourceGate({ state, hasSource, children }: SourceGateProps) {
  if (state.isPending) {
    return (
      <>
        <Skeleton className="h-control-md" />
        <Skeleton className="h-control-md" />
        <Skeleton className="h-control-md" />
      </>
    )
  }
  if (state.error !== null) {
    return (
      <>
        <Callout tone="danger" heading="Could not load your addresses">
          {describeFailure(state.error)}
        </Callout>
        <Button
          variant="secondary"
          onClick={() => {
            void state.refetch()
          }}
        >
          Try again
        </Button>
      </>
    )
  }
  if (!hasSource) {
    return (
      <EmptyState
        isCompact
        title="No address to send from"
        description="Add an address in the addresses panel first."
      />
    )
  }
  return children
}

interface SendContentProps {
  chain: ChainId
  defaultAsset: AssetId
  onSent: () => void
}

function SendContent({ chain, defaultAsset, onSent }: SendContentProps) {
  const addresses = useQuery(addressQueries.list(chain))
  const balances = useQuery(moneyQueries.balances())
  const settings = useSuspenseQuery(settingQueries.all())

  const canChooseEntry = settings.data.isPriceFeedEnabled
  const entry: AmountEntry = canChooseEntry ? settings.data.amountEntry : "units"

  const [chosenFromIds, setChosenFromIds] = useState<readonly number[] | null>(null)
  const [chosenAsset, setChosenAsset] = useState<AssetId | null>(null)
  const [to, setTo] = useState("")
  const [destinationTag, setDestinationTag] = useState("")
  const [comment, setComment] = useState("")
  const [amountInput, setAmountInput] = useState({ entry, text: "" })
  const [shouldSendMax, setShouldSendMax] = useState(false)
  const [shouldDeleteAccount, setShouldDeleteAccount] = useState(false)
  const [nonce, setNonce] = useState("")
  const [data, setData] = useState("")
  const [fee, setFee] = useState<FeeSelection>(NORMAL_FEE)
  const [formErrors, setFormErrors] = useState<SendFormErrors>({})

  const saveEntry = useIpcMutation<null, AmountEntry>({
    mutationFn: settingCommands.setAmountEntry,
    invalidates: [settingKeys.root],
    optimistic: (client, amountEntry) => {
      patchSettings(client, { amountEntry })
    },
    errorTitle: "Could not change how amounts are entered",
  })

  const asset = chosenAsset ?? defaultAsset
  const decimals = assetDecimals(asset)
  const price = usePrice(asset)
  const currency = useFiatCurrency()

  function parseAmount(text: string): Parsed<Amount> {
    return entry === "fiat"
      ? fiatToBaseUnits(text, currency, decimals, price)
      : toBaseUnits(text, decimals)
  }

  const sourceAddresses = addresses.data ?? []
  const fromIds = chosenFromIds ?? sourceAddresses.slice(0, 1).map((row) => row.id)
  const sources = fromIds.flatMap((id) => sourceAddresses.filter((row) => row.id === id))

  const balanceRows = balances.data ?? []
  function balanceFor(addressId: number, wanted: AssetId): Amount | null {
    return addressBalance(balanceRows, addressId, wanted)?.amount ?? null
  }

  function totalFor(wanted: AssetId): bigint | null {
    return sources.reduce<bigint | null>(
      (sum, row) => addAmounts(sum, balanceFor(row.id, wanted)),
      ZERO_AMOUNT,
    )
  }

  const amount = amountInput.entry === entry ? amountInput.text : ""
  const deferredInput = useDeferredValue(amountInput)
  const deferredAmount = deferredInput.entry === entry ? deferredInput.text : ""

  const trimmedTo = to.trim()
  const deferredTo = useDeferredValue(trimmedTo)
  const validation = useQuery(addressQueries.validate(chain, deferredTo))
  const canonicalTo = validation.data ?? null
  const recipientError = validation.isError ? describeFailure(validation.error) : null
  const isRecipientChecked = canonicalTo !== null && deferredTo === trimmedTo

  function requestFor(recipient: string, typed: Parsed<Amount>, requestFee: FeeSelection) {
    return buildTxRequest(
      {
        sources,
        asset,
        to: recipient,
        amount: typed,
        shouldSendMax,
        shouldDeleteAccount,
        destinationTag,
        comment,
        nonce,
        data,
      },
      requestFee,
    )
  }

  const quoteBuilt =
    canonicalTo === null ? null : requestFor(deferredTo, parseAmount(deferredAmount), NORMAL_FEE)
  const quoteTarget: QuoteTarget = {
    kind: "send",
    request: quoteBuilt?.isOk === true ? quoteBuilt.request : null,
  }
  const feeQuote = useFeeQuote(quoteTarget, fee)
  const parsedAmount = parseAmount(amount)
  const sendBuilt = requestFor(trimmedTo, parsedAmount, fee)

  const flow = useSend<TxRequest>({
    prepare: txCommands.prepare,
    prepareErrorTitle: "Could not build the transaction",
    onSubmitted: onSent,
  })

  const isReady = addresses.isSuccess && sourceAddresses.length > 0

  return (
    <DialogTransition step={flow.hasPreview ? 1 : 0} isLocked={flow.isSubmitting}>
      {flow.hasPreview ? (
        <ReviewStep
          state={flow.state}
          onBack={() => {
            flow.reset()
          }}
          onConfirm={(password) => {
            flow.confirm(password)
          }}
        />
      ) : (
        <DialogForm
          onSubmit={() => {
            if (isReady && !flow.isBusy) {
              setFormErrors(sendBuilt.isOk ? {} : sendBuilt.errors)
              if (sendBuilt.isOk) {
                flow.start(sendBuilt.request)
              }
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{`Send ${assetLabel(asset)}`}</DialogTitle>
          </DialogHeader>
          <DialogBody>
            <SourceGate state={addresses} hasSource={sourceAddresses.length > 0}>
              <SourceFields
                chain={chain}
                addresses={sourceAddresses}
                sources={sources}
                fromError={formErrors.from}
                onSourcesChange={(next) => {
                  setChosenFromIds(next)
                  setNonce("")
                  setFee(withEstimatedGas(fee))
                }}
                asset={asset}
                onAssetChange={(next) => {
                  setChosenAsset(next)
                  setAmountInput({ entry, text: "" })
                  setFee(withEstimatedGas(fee))
                }}
                balanceFor={balanceFor}
                totalFor={totalFor}
              />

              <RecipientField
                chain={chain}
                value={to}
                onValueChange={(next) => {
                  setTo(next)
                  setDestinationTag("")
                  setComment("")
                  setFee(withEstimatedGas(fee))
                }}
                isChecked={isRecipientChecked}
                error={recipientError ?? formErrors.to}
              />

              {chainMeta(chain).hasDestinationTags ? (
                <Field
                  label="Destination tag"
                  error={formErrors.destinationTag}
                  hint="Use the tag supplied by the recipient. Leave blank if none is required or the X-address already includes it."
                >
                  <Input
                    inputMode="numeric"
                    autoComplete="off"
                    value={destinationTag}
                    onChange={(e) => {
                      setDestinationTag(e.target.value)
                    }}
                  />
                </Field>
              ) : null}

              {chainMeta(chain).canDeleteAccount ? (
                <Field
                  label="Account deletion"
                  hint="Sends the whole balance, reserve included, and removes this account from the ledger. The deletion fee is the network's owner reserve."
                >
                  <label className="flex items-center gap-2 text-body">
                    <Switch
                      checked={shouldDeleteAccount}
                      onCheckedChange={(isDeleting) => {
                        setShouldDeleteAccount(isDeleting)
                        setShouldSendMax(isDeleting)
                        setAmountInput({ entry, text: "" })
                      }}
                    />
                    Delete account
                  </label>
                </Field>
              ) : null}

              {chainMeta(chain).hasComments ? (
                <Field
                  label="Comment"
                  hint="Use the comment supplied by the recipient. Leave blank if none is required or the payment request already includes it."
                >
                  <Input
                    autoComplete="off"
                    value={comment}
                    onChange={(e) => {
                      setComment(e.target.value)
                    }}
                  />
                </Field>
              ) : null}

              <AmountField
                asset={asset}
                entry={entry}
                canChooseEntry={canChooseEntry}
                onEntryChange={(next) => {
                  setAmountInput({ entry: next, text: "" })
                  setShouldSendMax(shouldDeleteAccount)
                  saveEntry.mutate(next)
                }}
                amount={amount}
                parsedAmount={parsedAmount}
                onAmountChange={(next) => {
                  setAmountInput({ entry, text: next })
                  setShouldSendMax(false)
                }}
                shouldSendMax={shouldSendMax}
                maxAmount={feeQuote.isCurrent ? (quoteOf(feeQuote)?.maxAmount ?? null) : null}
                onSendMax={() => {
                  setShouldSendMax(true)
                }}
                isLocked={shouldDeleteAccount}
                balance={totalFor(asset)}
                error={formErrors.amount}
              />

              <AdvancedFields
                chain={chain}
                nonce={nonce}
                data={data}
                nonceError={formErrors.nonce}
                dataError={formErrors.data}
                onNonceChange={setNonce}
                onDataChange={(next) => {
                  setData(next)
                  setFee(withEstimatedGas(fee))
                }}
              />

              <FeeSection
                chain={chain}
                target={quoteTarget}
                fee={fee}
                quote={feeQuote}
                onFeeChange={setFee}
              />
            </SourceGate>
          </DialogBody>
          <DialogFooter>
            {flow.error === null ? null : (
              <DialogFooterNote tone="danger">{flow.error}</DialogFooterNote>
            )}
            <Button type="submit" isLoading={flow.isBusy} disabled={!isReady}>
              Review
            </Button>
          </DialogFooter>
        </DialogForm>
      )}
    </DialogTransition>
  )
}

interface SendDialogProps {
  chain: ChainId
  asset: AssetId
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function SendDialog({ chain, asset, open, onOpenChange }: SendDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup>
        <SendContent
          chain={chain}
          defaultAsset={asset}
          onSent={() => {
            onOpenChange(false)
          }}
        />
      </DialogPopup>
    </Dialog>
  )
}
