import { useQuery } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { useState } from "react"

import type {
  PassphraseInput,
  PassphrasePolicy,
  PhraseWords,
  WalletColor,
} from "@/ipc/bindings.gen.ts"
import { describeFailure } from "@/lib/failure.ts"
import { phraseWords } from "@/lib/format.ts"
import { useSecretScreen } from "@/query/export.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { useActiveWallet, walletCommands, walletQueries } from "@/query/wallet.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Checkbox } from "@/ui/checkbox.tsx"
import { ColorPicker } from "@/ui/color-picker.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"
import { Textarea } from "@/ui/textarea.tsx"
import { CopyButton } from "@/widgets/copyable.tsx"
import { DEFAULT_WALLET_COLOR, WalletAvatar } from "@/widgets/logos.tsx"
import { WordGrid } from "@/widgets/secret-reveal.tsx"

// oxlint-disable-next-line eslint/no-magic-numbers -- BIP-39 phrase lengths
const PHRASE_LENGTHS: ReadonlySet<number> = new Set([12, 15, 18, 21, 24])

type SetupFlow = "create" | "restore"

interface WalletFormState {
  name: string
  color: WalletColor
  password: string
  confirm: string
  passphrasePolicy: PassphrasePolicy
  passphrase: string
  words: PhraseWords
  phrase: string
}

type WalletFormErrors = Partial<
  Record<"name" | "password" | "confirm" | "passphrase" | "phrase", string>
>

const EMPTY_WALLET_FORM: WalletFormState = {
  name: "",
  color: DEFAULT_WALLET_COLOR,
  password: "",
  confirm: "",
  passphrasePolicy: "none",
  passphrase: "",
  words: "twelve",
  phrase: "",
}

function toPassphraseInput(form: WalletFormState): PassphraseInput {
  return form.passphrasePolicy === "none"
    ? { kind: "none" }
    : { kind: form.passphrasePolicy, value: form.passphrase }
}

function validateWalletForm(form: WalletFormState, flow: SetupFlow): WalletFormErrors {
  const errors: WalletFormErrors = {}

  if (form.name.trim() === "") {
    errors.name = "Name the wallet."
  }
  if (form.password === "") {
    errors.password = "Enter a password."
  }
  if (form.confirm !== form.password) {
    errors.confirm = "The passwords do not match."
  }

  if (form.passphrasePolicy !== "none" && form.passphrase === "") {
    errors.passphrase = "Enter a passphrase or choose no passphrase."
  }

  if (flow === "restore") {
    const count = phraseWords(form.phrase).length
    if (count === 0) {
      errors.phrase = "Paste your seed phrase."
    } else if (!PHRASE_LENGTHS.has(count)) {
      errors.phrase = "Enter 12, 15, 18, 21 or 24 words."
    }
  }

  return errors
}

interface PasswordFieldsProps {
  form: WalletFormState
  errors: WalletFormErrors
  onChange: (patch: Partial<WalletFormState>) => void
}

function PasswordFields({ form, errors, onChange }: PasswordFieldsProps) {
  return (
    <div className="grid grid-cols-2 items-start gap-3">
      <Field
        label="Password"
        error={errors.password}
        hint="Use this password to unlock the wallet on this device."
      >
        <PasswordInput
          autoComplete="new-password"
          value={form.password}
          onChange={(e) => {
            onChange({ password: e.target.value })
          }}
        />
      </Field>
      <Field label="Confirm password" error={errors.confirm}>
        <PasswordInput
          autoComplete="new-password"
          value={form.confirm}
          onChange={(e) => {
            onChange({ confirm: e.target.value })
          }}
        />
      </Field>
    </div>
  )
}

const PASSPHRASE_POLICIES: readonly PassphrasePolicy[] = ["none", "stored", "prompt"]

const PASSPHRASE_POLICY_LABELS: Record<PassphrasePolicy, string> = {
  none: "No passphrase",
  stored: "Save in wallet",
  prompt: "Ask when unlocking",
}

const PASSPHRASE_HINTS: Record<PassphrasePolicy, string> = {
  none: "The seed phrase alone restores the wallet.",
  stored: "Save the passphrase in this wallet. Back it up separately for recovery.",
  prompt: "Enter it each time you unlock. Back it up or you will lose access to your funds.",
}

interface PassphraseFieldsProps {
  form: WalletFormState
  errors: WalletFormErrors
  onChange: (patch: Partial<WalletFormState>) => void
}

function PassphraseFields({ form, errors, onChange }: PassphraseFieldsProps) {
  return (
    <div className="grid grid-cols-2 items-start gap-3">
      <Field label="Passphrase" hint={PASSPHRASE_HINTS[form.passphrasePolicy]}>
        <Select
          value={form.passphrasePolicy}
          onValueChange={(policy) => {
            onChange({
              passphrasePolicy: policy,
              passphrase: policy === "none" ? "" : form.passphrase,
            })
          }}
        >
          <SelectTrigger className="w-full">
            {PASSPHRASE_POLICY_LABELS[form.passphrasePolicy]}
          </SelectTrigger>
          <SelectPopup>
            {PASSPHRASE_POLICIES.map((policy) => (
              <SelectItem key={policy} value={policy}>
                {PASSPHRASE_POLICY_LABELS[policy]}
              </SelectItem>
            ))}
          </SelectPopup>
        </Select>
      </Field>

      {form.passphrasePolicy === "none" ? null : (
        <Field label="Enter passphrase" error={errors.passphrase}>
          <PasswordInput
            autoComplete="off"
            value={form.passphrase}
            onChange={(e) => {
              onChange({ passphrase: e.target.value })
            }}
          />
        </Field>
      )}
    </div>
  )
}

const PHRASE_WORD_ITEMS: readonly { value: PhraseWords; label: string }[] = [
  { value: "twelve", label: "12 words" },
  { value: "twenty_four", label: "24 words" },
]

interface WalletFieldsProps {
  flow: SetupFlow
  form: WalletFormState
  errors: WalletFormErrors
  onChange: (patch: Partial<WalletFormState>) => void
}

function WalletFields({ flow, form, errors, onChange }: WalletFieldsProps) {
  return (
    <>
      <div className="flex items-start gap-3">
        <Field label="Wallet name" error={errors.name} className="min-w-0 flex-1">
          <Input
            autoComplete="off"
            value={form.name}
            onChange={(e) => {
              onChange({ name: e.target.value })
            }}
          />
        </Field>
        <Field label="Color">
          <ColorPicker
            value={form.color}
            attachment={<WalletAvatar color={form.color} />}
            onValueChange={(color) => {
              onChange({ color })
            }}
          />
        </Field>
      </div>

      {flow === "create" ? (
        <Field label="Phrase length">
          <Segmented
            value={form.words}
            items={PHRASE_WORD_ITEMS}
            onValueChange={(words) => {
              onChange({ words })
            }}
          />
        </Field>
      ) : null}

      {flow === "restore" ? (
        <Field label="Seed phrase" error={errors.phrase} hint="Separate the words with spaces.">
          <Textarea
            rows={3}
            autoComplete="off"
            placeholder="Enter your seed phrase."
            value={form.phrase}
            onChange={(e) => {
              onChange({ phrase: e.target.value })
            }}
          />
        </Field>
      ) : null}

      <PasswordFields form={form} errors={errors} onChange={onChange} />

      <PassphraseFields form={form} errors={errors} onChange={onChange} />
    </>
  )
}

interface PhraseBackupProps {
  phrase: string
  walletName: string
  onDone: () => void
}

function PhraseBackup({ phrase, walletName, onDone }: PhraseBackupProps) {
  const [isAcknowledged, setIsAcknowledged] = useState(false)
  const words = phraseWords(phrase)

  useSecretScreen()

  return (
    <div className="flex h-full flex-col gap-3 p-3">
      <SectionHeader
        title={`Back up ${walletName}`}
        description={`Write these ${String(words.length)} words on paper in this order.`}
        actions={<CopyButton value={phrase} size="icon-sm" isSensitive />}
      />

      <Callout tone="danger" heading="Keep your seed phrase private">
        These words can restore your wallet. Back up your passphrase too, if you use one. You can
        view the phrase again in Security settings with your password.
      </Callout>

      <div className="-mx-3 min-h-0 flex-1 scroll-y-3">
        <WordGrid words={words} />
      </div>

      <div className="flex shrink-0 items-center justify-between">
        <label className="flex items-center gap-2 text-body">
          <Checkbox checked={isAcknowledged} onCheckedChange={setIsAcknowledged} />
          I backed up the seed phrase
        </label>
        <Button disabled={!isAcknowledged} onClick={onDone}>
          Continue
        </Button>
      </div>
    </div>
  )
}

const NO_ERRORS: WalletFormErrors = {}

const RESTORE_FAILURE_TITLE = "Could not restore the wallet"
const CREATE_FAILURE_TITLE = "Could not create the wallet"

const FLOWS: readonly { value: SetupFlow; label: string; hint: string }[] = [
  { value: "create", label: "Create", hint: "Generate a new seed phrase." },
  { value: "restore", label: "Restore", hint: "Use an existing seed phrase." },
]

type Creation =
  | { kind: "idle" }
  | { kind: "creating" }
  | { kind: "failed"; message: string }
  | { kind: "created"; phrase: string; walletName: string }

const IDLE_CREATION: Creation = { kind: "idle" }

export function SetupView() {
  const navigate = useNavigate()
  const wallets = useQuery(walletQueries.all())
  const { isUnlocked } = useActiveWallet()

  const [flow, setFlow] = useState<SetupFlow>("create")
  const [form, setForm] = useState<WalletFormState>(EMPTY_WALLET_FORM)
  const [hasAttempted, setHasAttempted] = useState(false)
  const [creation, setCreation] = useState<Creation>(IDLE_CREATION)

  const restoreWallet = useIpcMutation({
    mutationFn: () =>
      walletCommands.importSeed(
        form.name.trim(),
        form.color,
        form.password,
        phraseWords(form.phrase).join(" "),
        toPassphraseInput(form),
      ),
    errorTitle: RESTORE_FAILURE_TITLE,
    onDone: () => {
      void navigate({ to: "/wallet", replace: true })
    },
  })

  async function createPhraseWallet(): Promise<void> {
    setCreation({ kind: "creating" })
    try {
      const created = await walletCommands.createSeed(
        form.name.trim(),
        form.color,
        form.password,
        form.words,
        toPassphraseInput(form),
      )
      setCreation({ kind: "created", phrase: created.phrase, walletName: created.wallet.name })
      setForm(EMPTY_WALLET_FORM)
      setHasAttempted(false)
    } catch (error) {
      setCreation({ kind: "failed", message: describeFailure(error) })
    }
  }

  if (creation.kind === "created") {
    return (
      <PhraseBackup
        phrase={creation.phrase}
        walletName={creation.walletName}
        onDone={() => {
          setCreation(IDLE_CREATION)
          void navigate({ to: "/wallet", replace: true })
        }}
      />
    )
  }

  const hasWallets = (wallets.data?.length ?? 0) > 0
  const errors = hasAttempted ? validateWalletForm(form, flow) : NO_ERRORS
  const isBusy = creation.kind === "creating" || restoreWallet.isPending
  const failures: Record<SetupFlow, { heading: string; message: string | null }> = {
    create: {
      heading: CREATE_FAILURE_TITLE,
      message: creation.kind === "failed" ? creation.message : null,
    },
    restore: {
      heading: RESTORE_FAILURE_TITLE,
      message: restoreWallet.error === null ? null : describeFailure(restoreWallet.error),
    },
  }
  const failure = failures[flow]

  function submit(): void {
    setHasAttempted(true)
    if (isBusy) {
      return
    }
    if (Object.keys(validateWalletForm(form, flow)).length > 0) {
      return
    }

    switch (flow) {
      case "create": {
        void createPhraseWallet()
        break
      }
      case "restore": {
        restoreWallet.mutate()
        break
      }
    }
  }

  return (
    <div className="flex h-full">
      <aside className="flex w-48 shrink-0 flex-col gap-2 p-2">
        <Segmented
          value={flow}
          variant="cards"
          orientation="vertical"
          onValueChange={(next) => {
            setFlow(next)
            setHasAttempted(false)
          }}
          items={FLOWS.map((entry) => ({
            value: entry.value,
            label: (
              <>
                <span className="text-body font-medium">{entry.label}</span>
                <span className="text-caption text-fg-faint">{entry.hint}</span>
              </>
            ),
          }))}
        />

        {hasWallets ? (
          <Button
            variant="ghost"
            size="sm"
            className="mt-auto justify-start"
            onClick={() => {
              void navigate({ to: "/unlock" })
            }}
          >
            <Icon name="back" />
            {isUnlocked ? "Back to the wallet" : "Back to unlock"}
          </Button>
        ) : (
          <p className="mt-auto px-1 pb-1 text-caption text-fg-faint">
            Jelly stores your keys on this device.
          </p>
        )}
      </aside>

      <main className="min-w-0 flex-1">
        <form
          className="flex h-full flex-col"
          onSubmit={(e) => {
            e.preventDefault()
            submit()
          }}
        >
          <div className="min-h-0 flex-1 scroll-y-4">
            <div className="mx-auto max-w-[calc(var(--container-xl)-var(--spacing)*8)] space-y-3 py-3">
              <WalletFields
                flow={flow}
                form={form}
                errors={errors}
                onChange={(patch) => {
                  setForm((current) => ({ ...current, ...patch }))
                }}
              />

              {failure.message === null ? null : (
                <Callout tone="danger" heading={failure.heading}>
                  {failure.message}
                </Callout>
              )}
            </div>
          </div>

          <div className="flex shrink-0 justify-end p-3">
            <Button type="submit" isLoading={isBusy}>
              {flow === "restore" ? "Restore wallet" : "Create wallet"}
            </Button>
          </div>
        </form>
      </main>
    </div>
  )
}
