export const NONE = "-"

export const ELLIPSIS = "..."

export function trimmedOrNull(text: string): string | null {
  const trimmed = text.trim()
  return trimmed === "" ? null : trimmed
}

export function truncateMiddle(value: string, head: number, tail: number): string {
  if (value.length <= head + tail + ELLIPSIS.length) {
    return value
  }
  return `${value.slice(0, head)}${ELLIPSIS}${value.slice(-tail)}`
}

export const MS_PER_SECOND = 1000

export const SECONDS_PER_DAY = 86_400

const DAY_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" })

const DAY_YEAR_FORMAT = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
})

export function unixSecondsOf(unixMillis: number): number {
  return Math.floor(unixMillis / MS_PER_SECOND)
}

function pad2(part: number): string {
  return String(part).padStart(2, "0")
}

function dateOf(unixSeconds: number | null): Date | null {
  if (unixSeconds === null) {
    return null
  }
  const at = new Date(unixSeconds * MS_PER_SECOND)
  return Number.isNaN(at.getTime()) ? null : at
}

function dateText(at: Date): string {
  return `${String(at.getFullYear())}-${pad2(at.getMonth() + 1)}-${pad2(at.getDate())}`
}

function clockText(at: Date): string {
  return `${pad2(at.getHours())}:${pad2(at.getMinutes())}`
}

function startOfDayMs(at: Date, offsetDays = 0): number {
  return new Date(at.getFullYear(), at.getMonth(), at.getDate() + offsetDays).getTime()
}

export function formatTimestamp(unixSeconds: number): string {
  const at = dateOf(unixSeconds)
  return at === null ? NONE : `${dateText(at)} ${clockText(at)}`
}

export function dayKey(unixSeconds: number | null): string {
  const at = dateOf(unixSeconds)
  return at === null ? NONE : dateText(at)
}

export function formatDay(unixSeconds: number | null, now = new Date()): string {
  const at = dateOf(unixSeconds)
  if (at === null) {
    return "Date unavailable"
  }
  const dayStartMs = startOfDayMs(at)
  if (dayStartMs === startOfDayMs(now)) {
    return "Today"
  }
  if (dayStartMs === startOfDayMs(now, -1)) {
    return "Yesterday"
  }
  return at.getFullYear() === now.getFullYear() ? DAY_FORMAT.format(at) : DAY_YEAR_FORMAT.format(at)
}

export function formatClock(unixSeconds: number | null): string {
  const at = dateOf(unixSeconds)
  return at === null ? NONE : clockText(at)
}

const WORD_SEPARATOR = /[\s,;]+/u
const LIST_NUMBER = /^[\d.):-]+/u

export function phraseWords(phrase: string): string[] {
  return phrase
    .split(WORD_SEPARATOR)
    .map((token) => token.replace(LIST_NUMBER, ""))
    .filter((word) => word !== "")
}
