import { useQuery, useSuspenseQuery } from "@tanstack/react-query"
import { useDeferredValue, useRef, useState } from "react"
import type { ReactNode } from "react"

import { ContactPicker } from "@/features/wallet/contacts.tsx"
import { NORMAL_FEE, withEstimatedGas } from "@/features/wallet/fee-form.ts"
import { FeeSection, quoteOf, useFeeQuote } from "@/features/wallet/fee.tsx"
import { ReviewStep } from "@/features/wallet/send-review.tsx"
import { buildTxRequest, useSend } from "@/features/wallet/send-state.ts"
import type { SendFormErrors } from "@/features/wallet/send-state.ts"
import type {
  AddressRecord,
  Amount,
  AmountEntry,
  AssetId,
  ChainId,
  FeeSelection,
  TxRequest,
} from "@/ipc/bindings.gen.ts"
import { ZERO_AMOUNT, addAmounts, formatAmount, toBaseUnits } from "@/lib/amount.ts"
import type { Parsed } from "@/lib/amount.ts"
import {
  assetDecimals,
  assetSymbol,
  assetsForChain,
  chainMeta,
  formatAddressList,
  nativeAsset,
} from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { CURRENCIES, PRICE_DECIMALS, fiatToBaseUnits } from "@/lib/fiat.ts"
import { NONE } from "@/lib/format.ts"
import { addressBalance } from "@/lib/portfolio.ts"
import { addressQueries } from "@/query/address.ts"
import { moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { txCommands } from "@/query/tx.ts"
import type { QuoteTarget } from "@/query/tx.ts"
import { useFiatCurrency, useFiatValue, usePrice } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Collapsible, CollapsiblePanel, CollapsibleTrigger } from "@/ui/collapsible.tsx"
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
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { MiddleTruncate } from "@/ui/middle-truncate.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { MultiSelect, Select, SelectItem, SelectPopup, SelectTrigger } from "@/ui/select.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { Switch } from "@/ui/switch.tsx"
import { AddressOption } from "@/widgets/address.tsx"
import { CryptoAmount, FiatAmount } from "@/widgets/amounts.tsx"
import { AssetMenu } from "@/widgets/asset-menu.tsx"
import type { ListState } from "@/widgets/list-status.tsx"
import { AssetLogo } from "@/widgets/logos.tsx"

interface SourceOptionsProps {
  chain: ChainId
  addresses: readonly AddressRecord[]
  balanceFor: (addressId: number, asset: AssetId) => Amount | null
}

function SourceOptions({ chain, addresses, balanceFor }: SourceOptionsProps) {
  const native = nativeAsset(chain)
  return (
    <SelectPopup shouldMatchAnchor>
      {addresses.map((row) => {
        const rowBalance = balanceFor(row.id, native)
        return (
          <SelectItem
            key={row.id}
            value={String(row.id)}
            description={
              rowBalance === null ? undefined : (
                <CryptoAmount amount={rowBalance} asset={native} isCompact />
              )
            }
          >
            <AddressOption row={row} className="font-mono text-caption" />
          </SelectItem>
        )
      })}
    </SelectPopup>
  )
}

const CHOOSE_ADDRESS = "Choose an address"

interface SourceFieldsProps {
  chain: ChainId
  addresses: readonly AddressRecord[]
  sources: readonly AddressRecord[]
  fromError: string | undefined
  onSourcesChange: (addressIds: number[]) => void
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
  balanceFor: (addressId: number, asset: AssetId) => Amount | null
  totalFor: (asset: AssetId) => Amount | bigint | null
}

function SourceFields({
  chain,
  addresses,
  sources,
  fromError,
  onSourcesChange,
  asset,
  onAssetChange,
  balanceFor,
  totalFor,
}: SourceFieldsProps) {
  const assets = assetsForChain(chain)
  const first = sources.at(0) ?? null
  const soleSource = sources.length === 1 ? sources[0] : undefined
  const sourceLabel =
    soleSource === undefined ? (
      <span className="truncate font-mono text-caption">
        {sources.length === 0
          ? CHOOSE_ADDRESS
          : formatAddressList(sources.map((row) => row.address))}
      </span>
    ) : (
      <MiddleTruncate
        value={soleSource.address}
        hasTooltip={false}
        className="font-mono text-caption"
      />
    )

  return (
    <div className="flex gap-3">
      <Field label="From" error={fromError} className="min-w-0 flex-1">
        {chainMeta(chain).canPoolSources ? (
          <MultiSelect
            values={sources.map((row) => String(row.id))}
            onValuesChange={(next) => {
              onSourcesChange(next.map(Number))
            }}
          >
            <SelectTrigger className="w-full">{sourceLabel}</SelectTrigger>
            <SourceOptions chain={chain} addresses={addresses} balanceFor={balanceFor} />
          </MultiSelect>
        ) : (
          <Select
            value={first === null ? "" : String(first.id)}
            onValueChange={(value) => {
              onSourcesChange([Number(value)])
            }}
          >
            <SelectTrigger className="w-full">{sourceLabel}</SelectTrigger>
            <SourceOptions chain={chain} addresses={addresses} balanceFor={balanceFor} />
          </Select>
        )}
      </Field>

      {assets.length < 2 ? null : (
        <Field label="Asset" className="w-36 shrink-0">
          <AssetMenu
            assets={assets}
            asset={asset}
            onAssetChange={onAssetChange}
            amountFor={totalFor}
            trigger={
              <Button
                variant="secondary"
                className="w-full justify-between rounded-md border border-edge px-2"
              >
                <AssetLogo asset={asset} className="size-icon-md" />
                <span className="min-w-0 flex-1 truncate text-left">{assetSymbol(asset)}</span>
                <Icon name="caret" />
              </Button>
            }
          />
        </Field>
      )}
    </div>
  )
}

interface RecipientFieldProps {
  chain: ChainId
  value: string
  onValueChange: (value: string) => void
  isChecked: boolean
  error: string | undefined
}

function RecipientField({ chain, value, onValueChange, isChecked, error }: RecipientFieldProps) {
  const anchorRef = useRef<HTMLDivElement>(null)

  return (
    <Field label="Recipient" error={error}>
      <div ref={anchorRef}>
        <Input
          isMono
          autoComplete="off"
          spellCheck={false}
          value={value}
          attachment={
            <>
              {isChecked ? <Icon name="check" className="text-ok" /> : null}
              <ContactPicker
                chain={chain}
                value={value}
                anchor={anchorRef}
                onPick={onValueChange}
              />
            </>
          }
          onChange={(e) => {
            onValueChange(e.target.value)
          }}
        />
      </div>
    </Field>
  )
}

const ENTRY_ITEMS = [
  { value: "fiat", label: "Fiat" },
  { value: "units", label: "Units" },
] as const satisfies readonly { value: AmountEntry; label: string }[]

interface AmountFieldProps {
  asset: AssetId
  entry: AmountEntry
  canChooseEntry: boolean
  onEntryChange: (entry: AmountEntry) => void
  amount: string
  parsedAmount: Parsed<Amount>
  onAmountChange: (amount: string) => void
  shouldSendMax: boolean
  maxAmount: Amount | null
  onSendMax: () => void
  isLocked: boolean
  balance: Amount | bigint | null
  error: string | undefined
}

function AmountField({
  asset,
  entry,
  canChooseEntry,
  onEntryChange,
  amount,
  parsedAmount,
  onAmountChange,
  shouldSendMax,
  maxAmount,
  onSendMax,
  isLocked,
  balance,
  error,
}: AmountFieldProps) {
  const currency = useFiatCurrency()
  const maxFiat = useFiatValue(maxAmount, asset)
  const balanceFiat = useFiatValue(balance, asset)
  const typedAmount = parsedAmount.isOk ? parsedAmount.value : null
  const typedError =
    !shouldSendMax && !parsedAmount.isOk && amount !== "" ? parsedAmount.error : null
  const previewAmount = (shouldSendMax ? maxAmount : typedAmount) ?? ZERO_AMOUNT

  let shown = amount
  if (shouldSendMax && entry === "fiat") {
    shown =
      maxFiat === null
        ? ""
        : formatAmount(maxFiat, PRICE_DECIMALS, {
            maxFractionDigits: CURRENCIES[currency].fractionDigits,
            shouldGroup: false,
          })
  } else if (shouldSendMax) {
    shown =
      maxAmount === null
        ? ""
        : formatAmount(maxAmount, assetDecimals(asset), { shouldGroup: false })
  }

  return (
    <Field
      label="Amount"
      error={error ?? typedError}
      hint={
        <span className="flex items-baseline justify-between gap-2">
          <span>
            {entry === "fiat" ? (
              <CryptoAmount amount={previewAmount} asset={asset} />
            ) : (
              <FiatAmount amount={previewAmount} asset={asset} />
            )}
          </span>
          <span className="inline-flex items-baseline gap-1">
            Balance
            {balance === null ? (
              NONE
            ) : (
              <>
                <CryptoAmount amount={balance} asset={asset} />
                {balanceFiat === null ? null : (
                  <>
                    <span>·</span>
                    <FiatAmount amount={balance} asset={asset} />
                  </>
                )}
              </>
            )}
          </span>
        </span>
      }
    >
      <Input
        isMono
        inputMode="decimal"
        autoComplete="off"
        placeholder="0"
        readOnly={isLocked}
        value={shown}
        attachment={
          <>
            {canChooseEntry ? (
              <Segmented
                value={entry}
                items={ENTRY_ITEMS}
                className="**:[[role=tab]]:h-4 **:[[role=tab]]:px-2"
                onValueChange={onEntryChange}
              />
            ) : null}
            {isLocked ? null : (
              <Button variant="subtle" size="xs" onClick={onSendMax}>
                Max
              </Button>
            )}
          </>
        }
        onChange={(e) => {
          onAmountChange(e.target.value)
        }}
      />
    </Field>
  )
}

interface AdvancedFieldsProps {
  chain: ChainId
  nonce: string
  data: string
  nonceError: string | undefined
  dataError: string | undefined
  onNonceChange: (nonce: string) => void
  onDataChange: (data: string) => void
}

function AdvancedFields({
  chain,
  nonce,
  data,
  nonceError,
  dataError,
  onNonceChange,
  onDataChange,
}: AdvancedFieldsProps) {
  const { canPinNonce, canCarryData } = chainMeta(chain)
  if (!canPinNonce && !canCarryData) {
    return null
  }
  return (
    <Collapsible defaultOpen={nonce !== "" || data !== ""}>
      <CollapsibleTrigger>Advanced</CollapsibleTrigger>
      <CollapsiblePanel className="space-y-3 pt-2">
        {canPinNonce ? (
          <Field
            label="Nonce"
            error={nonceError}
            hint="Reuse a pending transaction's nonce to replace it."
          >
            <Input
              inputMode="numeric"
              autoComplete="off"
              placeholder="Next"
              value={nonce}
              onChange={(e) => {
                onNonceChange(e.target.value)
              }}
            />
          </Field>
        ) : null}
        {canCarryData ? (
          <Field
            label="Data (hex)"
            error={dataError}
            hint="Sent in place of the transfer, for a contract call."
          >
            <Input
              isMono
              autoComplete="off"
              spellCheck={false}
              placeholder="None"
              value={data}
              onChange={(e) => {
                onDataChange(e.target.value)
              }}
            />
          </Field>
        ) : null}
      </CollapsiblePanel>
    </Collapsible>
  )
}

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

function totalOf(amounts: readonly (Amount | null)[]): bigint | null {
  return amounts.reduce<bigint | null>((sum, amount) => addAmounts(sum, amount), ZERO_AMOUNT)
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
    return totalOf(sources.map((row) => balanceFor(row.id, wanted)))
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
            <DialogTitle>{`Send ${assetSymbol(asset)}`}</DialogTitle>
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
