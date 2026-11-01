import { useRef } from "react"

import { ContactPicker } from "@/features/wallet/contacts.tsx"
import type { AddressRecord, Amount, AmountEntry, AssetId, ChainId } from "@/ipc/bindings.gen.ts"
import { ZERO_AMOUNT, formatAmount } from "@/lib/amount.ts"
import { assetDecimals, assetSymbol, assetsForChain, chainMeta, nativeAsset } from "@/lib/chain.ts"
import { CURRENCIES, PRICE_DECIMALS } from "@/lib/fiat.ts"
import { NONE } from "@/lib/format.ts"
import type { Parsed } from "@/lib/format.ts"
import { useFiatCurrency, useFiatValue } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Collapsible, CollapsiblePanel, CollapsibleTrigger } from "@/ui/collapsible.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { MultiSelect, Select, SelectItem, SelectPopup, SelectTrigger } from "@/ui/select.tsx"
import { AddressList, AddressOption } from "@/widgets/address.tsx"
import { CryptoAmount, FiatAmount } from "@/widgets/amounts.tsx"
import { AssetMenu } from "@/widgets/asset-menu.tsx"
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

export function SourceFields({
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
  const sourceLabel =
    first === null ? (
      <span className="truncate font-mono text-caption">{CHOOSE_ADDRESS}</span>
    ) : (
      <AddressList
        addresses={sources.map((row) => row.address)}
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

export function RecipientField({
  chain,
  value,
  onValueChange,
  isChecked,
  error,
}: RecipientFieldProps) {
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

export function AmountField({
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

export function AdvancedFields({
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
