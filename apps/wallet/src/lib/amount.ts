import type { Amount } from "@/ipc/bindings.gen.ts"
import { ELLIPSIS, NONE } from "@/lib/format.ts"

export type Parsed<T> = { isOk: true; value: T } | { isOk: false; error: string }

export const ZERO_AMOUNT = 0n

export const U128_MAX = (1n << 128n) - 1n

const DECIMAL_SHAPE = /^(?<whole>[0-9_,]*)(?:\.(?<fraction>[0-9]*))?$/u

const INTEGER_SHAPE = /^[+-]?[0-9]+$/u

const TRAILING_ZEROS = /0+$/u

const DIGIT_GROUP_SIZE = 3

const COMPACT_FRACTION_DIGITS = 8

export function pow10(exponent: number): bigint {
  return 10n ** BigInt(Math.max(0, Math.trunc(exponent)))
}

function invalid(error: string): { isOk: false; error: string } {
  return { isOk: false, error }
}

function tooManyDecimals(decimals: number): string {
  if (decimals === 0) {
    return "Enter a whole number."
  }
  return `Use at most ${String(decimals)} decimal place${decimals === 1 ? "" : "s"}.`
}

export function toBaseUnits(input: string, decimals: number): Parsed<Amount> {
  const text = input.trim()
  if (text === "") {
    return invalid("Enter an amount.")
  }
  if (text.startsWith("-")) {
    return invalid("Enter a positive amount.")
  }
  if (text.includes("e") || text.includes("E")) {
    return invalid("Enter the full number without exponent notation.")
  }

  const match = DECIMAL_SHAPE.exec(text)
  if (match === null) {
    return invalid("Enter a valid number.")
  }

  const whole = (match.groups?.["whole"] ?? "").replaceAll("_", "").replaceAll(",", "")
  const fraction = match.groups?.["fraction"] ?? ""
  if (whole === "" && fraction === "") {
    return invalid("Enter a valid number.")
  }
  if (fraction.length > decimals) {
    return invalid(tooManyDecimals(decimals))
  }

  const padded = fraction.padEnd(decimals, "0")
  const value =
    BigInt(whole === "" ? "0" : whole) * pow10(decimals) + (padded === "" ? 0n : BigInt(padded))
  if (value > U128_MAX) {
    return invalid("Enter a smaller amount.")
  }
  return { isOk: true, value: value.toString() }
}

export function amountToBigInt(amount: Amount | bigint | null): bigint | null {
  if (amount === null) {
    return null
  }
  if (typeof amount === "bigint") {
    return amount
  }
  const text = amount.trim()
  if (!INTEGER_SHAPE.test(text)) {
    return null
  }
  return BigInt(text)
}

export function signPrefix(value: bigint, isSigned: boolean): string {
  if (value < 0n) {
    return "-"
  }
  return isSigned && value > 0n ? "+" : ""
}

interface FormatAmountOptions {
  shouldKeepTrailingZeros?: boolean
  maxFractionDigits?: number
  shouldGroup?: boolean
  isSigned?: boolean
}

function groupDigits(digits: string): string {
  let grouped = ""
  let i = 0
  for (const digit of digits) {
    if (i > 0 && (digits.length - i) % DIGIT_GROUP_SIZE === 0) {
      grouped += ","
    }
    grouped += digit
    i += 1
  }
  return grouped
}

export function formatAmount(
  amount: Amount | bigint | null,
  decimals: number,
  options: FormatAmountOptions = {},
): string {
  const raw = amountToBigInt(amount)
  if (raw === null) {
    return NONE
  }

  const magnitude = raw < 0n ? -raw : raw
  const scale = pow10(decimals)
  const whole = (magnitude / scale).toString()

  let fraction = decimals > 0 ? (magnitude % scale).toString().padStart(decimals, "0") : ""
  const { maxFractionDigits } = options
  if (maxFractionDigits !== undefined && maxFractionDigits < fraction.length) {
    fraction = fraction.slice(0, maxFractionDigits)
  }
  if (options.shouldKeepTrailingZeros !== true) {
    fraction = fraction.replace(TRAILING_ZEROS, "")
  }

  const head = options.shouldGroup === false ? whole : groupDigits(whole)
  const body = fraction === "" ? head : `${head}.${fraction}`
  return `${signPrefix(raw, options.isSigned === true)}${body}`
}

export function formatAmountCompact(amount: Amount | bigint | null, decimals: number): string {
  const full = formatAmount(amount, decimals)
  const short = formatAmount(amount, decimals, { maxFractionDigits: COMPACT_FRACTION_DIGITS })
  return full === short ? full : `${short}${ELLIPSIS}`
}

export function addAmounts(a: Amount | bigint | null, b: Amount | bigint | null): bigint | null {
  const left = amountToBigInt(a)
  const right = amountToBigInt(b)
  if (left === null || right === null) {
    return null
  }
  return left + right
}

export function isZeroAmount(amount: Amount | bigint | null): boolean {
  return amountToBigInt(amount) === 0n
}
