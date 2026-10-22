import type { Amount, AssetId, FiatCurrency, TxDirection } from "@/ipc/bindings.gen.ts"
import { formatAmount, formatAmountCompact, amountToBigInt } from "@/lib/amount.ts"
import { assetDecimals, assetSymbol } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { STALE_PRICE_NOTICE, formatFiat } from "@/lib/fiat.ts"
import { directionSign } from "@/lib/tx.ts"
import { useFiatCurrency, useFiatValue, useIsFiatStale } from "@/store.ts"
import { TooltipText } from "@/ui/tooltip.tsx"

interface FiatValueProps {
  value: bigint
  currency: FiatCurrency
  isStale?: boolean | undefined
  notice?: string | undefined
  isSigned?: boolean | undefined
  isPrice?: boolean | undefined
  className?: string | undefined
}

export function FiatValue({
  value,
  currency,
  isStale = false,
  notice,
  isSigned = false,
  isPrice = false,
  className,
}: FiatValueProps) {
  return (
    <TooltipText
      content={notice ?? (isStale ? STALE_PRICE_NOTICE : null)}
      render={
        <span
          data-stale={isStale}
          className={appendClassName(
            `whitespace-nowrap tabular-nums ${isStale ? "text-fg-faint italic" : ""}`,
            className,
          )}
        />
      }
    >
      {formatFiat(value, currency, { isSigned, isPrice })}
    </TooltipText>
  )
}

interface FiatAmountProps {
  amount: Amount | bigint | null
  asset: AssetId
  className?: string | undefined
}

export function FiatAmount({ amount, asset, className }: FiatAmountProps) {
  const value = useFiatValue(amount, asset)
  const currency = useFiatCurrency()
  const isStale = useIsFiatStale(asset)

  if (value === null) {
    return null
  }

  return <FiatValue value={value} currency={currency} isStale={isStale} className={className} />
}

const DIRECTION_CLASSES = {
  incoming: "text-ok",
  outgoing: "text-danger",
  self: "",
} as const satisfies Record<TxDirection, string>

interface CryptoAmountProps {
  amount: Amount | bigint | null
  asset: AssetId
  hasFiat?: boolean | undefined
  direction?: TxDirection | undefined
  isCompact?: boolean | undefined
  className?: string | undefined
}

export function CryptoAmount({
  amount,
  asset,
  hasFiat,
  direction,
  isCompact,
  className,
}: CryptoAmountProps) {
  const decimals = assetDecimals(asset)
  const raw = amountToBigInt(amount)
  const value = raw === null || direction === undefined ? raw : raw * directionSign(direction)
  const text =
    isCompact === true
      ? formatAmountCompact(value, decimals)
      : formatAmount(value, decimals, { isSigned: direction !== undefined })

  return (
    <span
      className={appendClassName(
        `inline-flex items-baseline gap-1 whitespace-nowrap tabular-nums ${direction === undefined ? "" : DIRECTION_CLASSES[direction]}`,
        className,
      )}
    >
      <span>{text}</span>
      <span>{assetSymbol(asset)}</span>
      {hasFiat === true ? (
        <FiatAmount amount={raw} asset={asset} className="not-data-[stale=true]:text-fg-muted" />
      ) : null}
    </span>
  )
}
