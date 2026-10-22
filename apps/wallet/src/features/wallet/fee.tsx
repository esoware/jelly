import { keepPreviousData, useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import type { ReactNode } from "react"

import {
  LEVEL_LABELS,
  buildCustomFee,
  feeForm,
  withEstimatedGas,
} from "@/features/wallet/fee-form.ts"
import type { FeeForm } from "@/features/wallet/fee-form.ts"
import type { ChainId, FeeLevel, FeeQuote, FeeSelection } from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import type { Parsed } from "@/lib/amount.ts"
import { chainMeta, nativeAsset } from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { txQueries } from "@/query/tx.ts"
import type { QuoteTarget } from "@/query/tx.ts"
import { Button } from "@/ui/button.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Select, SelectItem, SelectPopup, SelectTrigger } from "@/ui/select.tsx"
import { Spinner } from "@/ui/spinner.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { CryptoAmount, FiatAmount } from "@/widgets/amounts.tsx"
import { FormDialog } from "@/widgets/dialogs.tsx"

type FeeEstimate =
  | { status: "unquotable" }
  | { status: "estimating" }
  | { status: "quoted"; quote: FeeQuote }
  | { status: "failed"; message: string; isRefused: boolean }

interface FeeQuoteState {
  estimate: FeeEstimate
  isCurrent: boolean
  isFetching: boolean
}

export function quoteOf(state: FeeQuoteState): FeeQuote | null {
  return state.estimate.status === "quoted" ? state.estimate.quote : null
}

export function useFeeQuote(
  target: QuoteTarget,
  fee: FeeSelection | null,
  isEnabled = true,
): FeeQuoteState {
  const result = useQuery({
    ...txQueries.quote(target, fee),
    placeholderData: keepPreviousData,
    enabled: isEnabled,
  })
  const isQuotable = fee !== null && (target.kind !== "send" || target.request !== null)
  let estimate: FeeEstimate = { status: "unquotable" }
  if (isQuotable && result.isError) {
    estimate = {
      status: "failed",
      message: describeFailure(result.error),
      isRefused: hasCode(result.error, "invalid_input"),
    }
  } else if (isQuotable) {
    estimate =
      result.data === undefined
        ? { status: "estimating" }
        : { status: "quoted", quote: result.data }
  }
  return {
    estimate,
    isCurrent: !result.isPlaceholderData,
    isFetching: result.isFetching,
  }
}

interface FeeInputProps {
  label: string
  value: string
  placeholder?: string | undefined
  isWide?: boolean | undefined
  onValueChange: (value: string) => void
}

function FeeInput({ label, value, placeholder, isWide, onValueChange }: FeeInputProps) {
  return (
    <Field label={label} className={isWide === true ? "col-span-2" : undefined}>
      <Input
        size="sm"
        isMono
        autoComplete="off"
        placeholder={placeholder}
        value={value}
        onChange={(e) => {
          onValueChange(e.target.value)
        }}
      />
    </Field>
  )
}

interface FeeFieldsProps {
  chain: ChainId
  form: FeeForm
  onFormChange: (next: FeeForm) => void
}

function FeeFields({ chain, form, onFormChange }: FeeFieldsProps) {
  switch (form.variant) {
    case "rate": {
      const unit = chainMeta(chain).feeRateUnit
      return (
        <FeeInput
          label={unit === null ? "Fee rate" : `Fee rate (${unit})`}
          value={form.rate}
          isWide
          onValueChange={(rate) => {
            onFormChange({ ...form, rate })
          }}
        />
      )
    }
    case "fee_cap": {
      return (
        <>
          <FeeInput
            label="Max fee (gwei)"
            value={form.maxFee}
            placeholder="Normal"
            onValueChange={(maxFee) => {
              onFormChange({ ...form, maxFee })
            }}
          />
          <FeeInput
            label="Priority fee (gwei)"
            value={form.priorityFee}
            placeholder="Normal"
            onValueChange={(priorityFee) => {
              onFormChange({ ...form, priorityFee })
            }}
          />
          <FeeInput
            label="Gas limit"
            value={form.gasLimit}
            placeholder="Estimated"
            isWide
            onValueChange={(gasLimit) => {
              onFormChange({ ...form, gasLimit })
            }}
          />
        </>
      )
    }
    case "gas_price": {
      return (
        <>
          <FeeInput
            label="Gas price (gwei)"
            value={form.gasPrice}
            placeholder="Normal"
            onValueChange={(gasPrice) => {
              onFormChange({ ...form, gasPrice })
            }}
          />
          <FeeInput
            label="Gas limit"
            value={form.gasLimit}
            placeholder="Estimated"
            onValueChange={(gasLimit) => {
              onFormChange({ ...form, gasLimit })
            }}
          />
        </>
      )
    }
    case "priority_fee": {
      return (
        <FeeInput
          label={`Priority fee (${form.symbol})`}
          value={form.priorityFee}
          isWide
          onValueChange={(priorityFee) => {
            onFormChange({ ...form, priorityFee })
          }}
        />
      )
    }
    case "total": {
      return (
        <FeeInput
          label={`Fee (${form.symbol})`}
          value={form.fee}
          isWide
          onValueChange={(fee) => {
            onFormChange({ ...form, fee })
          }}
        />
      )
    }
  }
}

function rateCaption(chain: ChainId, quote: FeeQuote): string | null {
  const unit = chainMeta(chain).feeRateUnit
  return quote.custom?.kind === "rate" && unit !== null ? `${quote.custom.rate} ${unit}` : null
}

interface FeeReadoutBodyProps {
  chain: ChainId
  estimate: FeeEstimate
  problem: string | null
}

function FeeReadoutBody({ chain, estimate, problem }: FeeReadoutBodyProps) {
  if (problem !== null) {
    return <p className="line-clamp-2 self-center text-caption text-fg-muted">{problem}</p>
  }
  switch (estimate.status) {
    case "failed": {
      return (
        <div className="flex min-w-0 items-center gap-2 self-center">
          <Icon name="danger" className="size-icon-lg shrink-0 text-danger" />
          <TooltipText
            content={estimate.message}
            render={<p className="line-clamp-2 text-caption wrap-anywhere text-fg" />}
          >
            {estimate.message}
          </TooltipText>
        </div>
      )
    }
    case "unquotable":
    case "estimating": {
      return (
        <p className="line-clamp-2 self-center text-caption text-fg-muted">
          {estimate.status === "estimating"
            ? "Estimating the fee."
            : "Enter a recipient and amount to see the fee."}
        </p>
      )
    }
    case "quoted": {
      const { quote } = estimate
      const native = nativeAsset(chain)
      return (
        <div className="flex min-w-0 items-center justify-between gap-3 self-center">
          <span className="min-w-0 truncate text-caption text-fg-muted">
            {rateCaption(chain, quote)}
          </span>
          <span className="flex shrink-0 flex-col items-end">
            <CryptoAmount
              amount={quote.fee}
              asset={native}
              className="text-body font-medium text-fg"
            />
            <FiatAmount amount={quote.fee} asset={native} className="text-caption text-fg-muted" />
          </span>
        </div>
      )
    }
  }
}

interface FeeReadoutProps {
  title: string
  chain: ChainId
  state: FeeQuoteState
  problem?: string | null | undefined
  control?: ReactNode
}

function FeeReadout({ title, chain, state, problem = null, control }: FeeReadoutProps) {
  const isFailed = problem === null && state.estimate.status === "failed"

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-2 text-body font-medium text-fg-muted">
          {title}
          {state.isFetching ? <Spinner size="sm" /> : null}
        </span>
        {control}
      </div>
      <div
        data-tone={isFailed ? "danger" : "neutral"}
        className="grid grid-rows-[2lh] rounded-lg px-3 py-2 *:col-start-1 *:row-start-1 data-[tone=danger]:bg-danger-subtle data-[tone=neutral]:bg-surface"
      >
        <FeeReadoutBody chain={chain} estimate={state.estimate} problem={problem} />
      </div>
    </div>
  )
}

const QUOTE_DELAY_MS = 400

function customOf(form: FeeForm | null): Parsed<FeeSelection> | null {
  return form === null ? null : buildCustomFee(form)
}

interface FeeDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  chain: ChainId
  target: QuoteTarget
  fee: FeeSelection
  levelCustom: FeeSelection | null
  onApply: (fee: FeeSelection) => void
}

function FeeDialog({
  open,
  onOpenChange,
  chain,
  target,
  fee,
  levelCustom,
  onApply,
}: FeeDialogProps) {
  const [editedForm, setEditedForm] = useState(() =>
    fee.kind === "level" ? null : feeForm(chain, fee),
  )
  const [quotedForm, setQuotedForm] = useState(editedForm)
  const isSettled = quotedForm === editedForm
  const seedForm = feeForm(chain, levelCustom === null ? null : withEstimatedGas(levelCustom))
  const form = editedForm ?? seedForm
  const quoted = customOf(quotedForm ?? seedForm)
  const draftQuote = useFeeQuote(target, quoted?.isOk === true ? quoted.value : null, open)
  const draft = customOf(form)
  const draftFee = draft?.isOk === true ? draft.value : null
  const canQuote = target.kind !== "send" || target.request !== null
  const draftEstimate = draftQuote.estimate
  const isDraftChecked =
    isSettled &&
    draftQuote.isCurrent &&
    (draftEstimate.status === "quoted" ||
      (draftEstimate.status === "failed" && !draftEstimate.isRefused))

  useEffect(() => {
    if (isSettled) {
      return undefined
    }
    const handle = setTimeout(() => {
      setQuotedForm(editedForm)
    }, QUOTE_DELAY_MS)
    return () => {
      clearTimeout(handle)
    }
  }, [editedForm, isSettled])

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Custom fee"
      submitLabel="Apply"
      isSubmitDisabled={draftFee === null || (canQuote && !isDraftChecked)}
      isBusy={false}
      onSubmit={() => {
        if (draftFee !== null) {
          onApply(draftFee)
        }
      }}
    >
      {form === null ? null : (
        <div className="grid grid-cols-2 gap-x-3 gap-y-2">
          <FeeFields chain={chain} form={form} onFormChange={setEditedForm} />
        </div>
      )}
      <FeeReadout
        title="Total fee"
        chain={chain}
        state={isSettled ? draftQuote : { ...draftQuote, isFetching: true }}
        problem={draft === null || draft.isOk ? null : draft.error}
      />
    </FormDialog>
  )
}

type LevelChoice = FeeLevel | "custom"

interface LevelSelectProps {
  chain: ChainId
  fee: FeeSelection
  quote: FeeQuoteState
  onFeeChange: (fee: FeeSelection) => void
  onEdit: () => void
}

function LevelSelect({ chain, fee, quote, onFeeChange, onEdit }: LevelSelectProps) {
  const { feeLevels, customFee } = chainMeta(chain)
  const choice: LevelChoice = fee.kind === "level" ? fee.level : "custom"
  const native = nativeAsset(chain)
  return (
    <span className="flex items-center gap-1">
      {choice === "custom" ? (
        <Button variant="ghost" size="icon-sm" onClick={onEdit}>
          <Icon name="rename" />
        </Button>
      ) : null}
      <Select<LevelChoice>
        value={choice}
        onValueChange={(next) => {
          if (next === "custom") {
            onEdit()
          } else {
            onFeeChange({ kind: "level", level: next })
          }
        }}
      >
        <SelectTrigger size="sm" className="w-28">
          {choice === "custom" ? "Custom" : LEVEL_LABELS[choice]}
        </SelectTrigger>
        <SelectPopup>
          {feeLevels.map((level) => {
            const cost = quoteOf(quote)?.levels.find((row) => row.level === level)?.fee
            return (
              <SelectItem
                key={level}
                value={level}
                description={
                  cost === undefined ? undefined : <CryptoAmount amount={cost} asset={native} />
                }
              >
                {LEVEL_LABELS[level]}
              </SelectItem>
            )
          })}
          {customFee === null ? null : <SelectItem value="custom">Custom</SelectItem>}
        </SelectPopup>
      </Select>
    </span>
  )
}

interface FeeSectionProps {
  chain: ChainId
  target: QuoteTarget
  fee: FeeSelection
  quote: FeeQuoteState
  onFeeChange: (fee: FeeSelection) => void
}

export function FeeSection({ chain, target, fee, quote, onFeeChange }: FeeSectionProps) {
  const [openCount, setOpenCount] = useState(0)
  const [isEditing, setIsEditing] = useState(false)
  const { feeLevels, customFee } = chainMeta(chain)

  function openEditor(): void {
    setOpenCount((count) => count + 1)
    setIsEditing(true)
  }

  return (
    <>
      <FeeReadout
        title="Network fee"
        chain={chain}
        state={quote}
        control={
          feeLevels.length === 0 ? null : (
            <LevelSelect
              chain={chain}
              fee={fee}
              quote={quote}
              onFeeChange={onFeeChange}
              onEdit={openEditor}
            />
          )
        }
      />
      {customFee === null ? null : (
        <FeeDialog
          key={openCount}
          open={isEditing}
          onOpenChange={setIsEditing}
          chain={chain}
          target={target}
          fee={fee}
          levelCustom={fee.kind === "level" ? (quoteOf(quote)?.custom ?? null) : null}
          onApply={(next) => {
            onFeeChange(next)
            setIsEditing(false)
          }}
        />
      )}
    </>
  )
}
