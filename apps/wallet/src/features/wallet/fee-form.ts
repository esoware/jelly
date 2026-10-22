import type { ChainId, FeeLevel, FeeSelection } from "@/ipc/bindings.gen.ts"
import { formatAmount, isZeroAmount, toBaseUnits } from "@/lib/amount.ts"
import type { Parsed } from "@/lib/amount.ts"
import { assetDecimals, assetSymbol, chainMeta, nativeAsset } from "@/lib/chain.ts"

const GWEI_DECIMALS = 9

const MIN_GAS_LIMIT = 21_000

export const NORMAL_FEE: FeeSelection = { kind: "level", level: "normal" }

export const LEVEL_LABELS: Record<FeeLevel, string> = {
  slow: "Slow",
  normal: "Normal",
  fast: "Fast",
}

export type FeeForm =
  | { variant: "rate"; rate: string }
  | { variant: "fee_cap"; maxFee: string; priorityFee: string; gasLimit: string }
  | { variant: "gas_price"; gasPrice: string; gasLimit: string }
  | { variant: "priority_fee"; priorityFee: string; symbol: string; decimals: number }
  | { variant: "total"; fee: string; symbol: string; decimals: number }

function formatGwei(wei: string | null | undefined): string {
  return wei === null || wei === undefined
    ? ""
    : formatAmount(wei, GWEI_DECIMALS, { shouldGroup: false })
}

export function withEstimatedGas(fee: FeeSelection): FeeSelection {
  return fee.kind === "evm" && fee.gasLimit !== null ? { ...fee, gasLimit: null } : fee
}

export function feeForm(chain: ChainId, seed: FeeSelection | null): FeeForm | null {
  const evm = seed?.kind === "evm" ? seed : null
  const gasLimit = evm?.gasLimit ?? ""
  const native = nativeAsset(chain)
  const symbol = assetSymbol(native)
  const decimals = assetDecimals(native)
  const { customFee } = chainMeta(chain)
  switch (customFee) {
    case null: {
      return null
    }
    case "rate": {
      return { variant: "rate", rate: seed?.kind === "rate" ? seed.rate : "" }
    }
    case "fee_cap": {
      return {
        variant: "fee_cap",
        maxFee: formatGwei(evm?.maxFeePerGas),
        priorityFee: formatGwei(evm?.maxPriorityFeePerGas),
        gasLimit,
      }
    }
    case "gas_price": {
      return { variant: "gas_price", gasPrice: formatGwei(evm?.maxPriorityFeePerGas), gasLimit }
    }
    case "priority_fee": {
      return {
        variant: "priority_fee",
        priorityFee:
          seed?.kind === "solana"
            ? formatAmount(seed.priorityFeeLamports, decimals, { shouldGroup: false })
            : "",
        symbol,
        decimals,
      }
    }
    case "total": {
      return {
        variant: "total",
        fee: seed?.kind === "total" ? formatAmount(seed.fee, decimals, { shouldGroup: false }) : "",
        symbol,
        decimals,
      }
    }
  }
}

const DIGITS = /^[0-9]+$/u
const DECIMAL = /^[0-9]+(?:\.[0-9]+)?$/u
const LEADING_ZEROS = /^0+(?=[0-9])/u
const ZERO_SHAPE = /^0+(?:\.0*)?$/u

function parseFeeRate(raw: string): Parsed<string> {
  const text = raw.trim()
  if (text === "") {
    return { isOk: false, error: "Enter a fee rate." }
  }
  if (!DECIMAL.test(text)) {
    return { isOk: false, error: "Enter the fee rate as a number." }
  }
  if (ZERO_SHAPE.test(text)) {
    return { isOk: false, error: "Enter a fee rate above zero." }
  }
  return { isOk: true, value: text.replace(LEADING_ZEROS, "") }
}

function parseGwei(raw: string, name: string, canBeZero: boolean): Parsed<string | null> {
  if (raw.trim() === "") {
    return { isOk: true, value: null }
  }
  const parsed = toBaseUnits(raw, GWEI_DECIMALS)
  if (!parsed.isOk) {
    return {
      isOk: false,
      error: `Enter the ${name} in gwei, with at most ${String(GWEI_DECIMALS)} decimal places.`,
    }
  }
  if (!canBeZero && isZeroAmount(parsed.value)) {
    return { isOk: false, error: `Enter a ${name} above zero.` }
  }
  return parsed
}

function parseGasLimit(raw: string): Parsed<string | null> {
  const text = raw.trim()
  if (text === "") {
    return { isOk: true, value: null }
  }
  if (!DIGITS.test(text)) {
    return { isOk: false, error: "Enter the gas limit as a whole number." }
  }
  if (Number(text) < MIN_GAS_LIMIT) {
    return { isOk: false, error: `Enter a gas limit of at least ${String(MIN_GAS_LIMIT)}.` }
  }
  return { isOk: true, value: text.replace(LEADING_ZEROS, "") }
}

function parseCoinFee(raw: string, name: string, symbol: string, decimals: number): Parsed<string> {
  if (raw.trim() === "") {
    return { isOk: false, error: `Enter a ${name}.` }
  }
  const parsed = toBaseUnits(raw, decimals)
  return parsed.isOk
    ? parsed
    : {
        isOk: false,
        error: `Enter the ${name} in ${symbol}, with at most ${String(decimals)} decimal places.`,
      }
}

export function buildCustomFee(form: FeeForm): Parsed<FeeSelection> {
  switch (form.variant) {
    case "rate": {
      const rate = parseFeeRate(form.rate)
      return rate.isOk ? { isOk: true, value: { kind: "rate", rate: rate.value } } : rate
    }

    case "fee_cap": {
      const maxFee = parseGwei(form.maxFee, "max fee", false)
      const priorityFee = parseGwei(form.priorityFee, "priority fee", true)
      const gasLimit = parseGasLimit(form.gasLimit)
      if (!maxFee.isOk) {
        return maxFee
      }
      if (!priorityFee.isOk) {
        return priorityFee
      }
      if (
        maxFee.value !== null &&
        priorityFee.value !== null &&
        BigInt(priorityFee.value) > BigInt(maxFee.value)
      ) {
        return { isOk: false, error: "Enter a priority fee no higher than the max fee." }
      }
      if (!gasLimit.isOk) {
        return gasLimit
      }
      return {
        isOk: true,
        value: {
          kind: "evm",
          gasLimit: gasLimit.value,
          maxFeePerGas: maxFee.value,
          maxPriorityFeePerGas: priorityFee.value,
        },
      }
    }

    case "gas_price": {
      const gasPrice = parseGwei(form.gasPrice, "gas price", false)
      const gasLimit = parseGasLimit(form.gasLimit)
      if (!gasPrice.isOk) {
        return gasPrice
      }
      if (!gasLimit.isOk) {
        return gasLimit
      }
      return {
        isOk: true,
        value: {
          kind: "evm",
          gasLimit: gasLimit.value,
          maxFeePerGas: gasPrice.value,
          maxPriorityFeePerGas: gasPrice.value,
        },
      }
    }

    case "priority_fee": {
      const priorityFee = parseCoinFee(form.priorityFee, "priority fee", form.symbol, form.decimals)
      return priorityFee.isOk
        ? { isOk: true, value: { kind: "solana", priorityFeeLamports: priorityFee.value } }
        : priorityFee
    }

    case "total": {
      const fee = parseCoinFee(form.fee, "fee", form.symbol, form.decimals)
      return fee.isOk ? { isOk: true, value: { kind: "total", fee: fee.value } } : fee
    }
  }
}
