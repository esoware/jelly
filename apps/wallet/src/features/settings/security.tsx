import { useSuspenseQuery } from "@tanstack/react-query"
import { useState } from "react"

import { MAX_U32, parseWhole } from "@/features/settings/layout.tsx"
import { hasCode } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { exportCommands } from "@/query/export.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { useActiveWallet, walletCommands } from "@/query/wallet.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import type { IconName } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { SecretRevealDialog } from "@/widgets/secret-reveal.tsx"

interface PasswordErrors {
  current: string | null
  next: string | null
  confirm: string | null
}

const NO_PASSWORD_ERRORS: PasswordErrors = { current: null, next: null, confirm: null }

interface PasswordChange {
  current: string
  next: string
}

function ChangePasswordForm() {
  const [current, setCurrent] = useState("")
  const [next, setNext] = useState("")
  const [confirm, setConfirm] = useState("")
  const [errors, setErrors] = useState<PasswordErrors>(NO_PASSWORD_ERRORS)

  const change = useIpcMutation<null, PasswordChange>({
    mutationFn: (variables) => walletCommands.changePassword(variables.current, variables.next),
    successToast: "Password changed",
    errorTitle: "Could not change the password",
    onDone: () => {
      setCurrent("")
      setNext("")
      setConfirm("")
      setErrors(NO_PASSWORD_ERRORS)
    },
  })

  const wrongPasswordError = hasCode(change.error, "wrong_password")
    ? describeFailure(change.error)
    : null

  return (
    <form
      className="space-y-3"
      onSubmit={(e) => {
        e.preventDefault()
        const found: PasswordErrors = {
          current: current === "" ? "Enter your current password." : null,
          next: next === "" ? "Enter a new password." : null,
          confirm: next === confirm ? null : "The passwords do not match.",
        }
        setErrors(found)
        if (found.current !== null || found.next !== null || found.confirm !== null) {
          return
        }
        change.mutate({ current, next })
      }}
    >
      <SectionHeader
        level="subsection"
        title="Change password"
        description="Enter your current password to set a new one."
      />

      <Field
        label="Current password"
        error={errors.current ?? wrongPasswordError}
        className="max-w-56"
      >
        <PasswordInput
          size="sm"
          autoComplete="current-password"
          value={current}
          onChange={(e) => {
            setCurrent(e.target.value)
            if (change.isError) {
              change.reset()
            }
          }}
        />
      </Field>

      <div className="grid max-w-56 gap-3">
        <Field label="New password" error={errors.next}>
          <PasswordInput
            size="sm"
            autoComplete="new-password"
            value={next}
            onChange={(e) => {
              setNext(e.target.value)
            }}
          />
        </Field>
        <Field label="Confirm password" error={errors.confirm}>
          <PasswordInput
            size="sm"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => {
              setConfirm(e.target.value)
            }}
          />
        </Field>
      </div>

      <Button type="submit" variant="secondary" size="sm" isLoading={change.isPending}>
        Change password
      </Button>
    </form>
  )
}

interface SaveAutoLock {
  draft: string
  minutes: number
}

function AutoLockForm() {
  const settings = useSuspenseQuery(settingQueries.all())
  const [draft, setDraft] = useState<string | null>(null)
  const value = draft ?? String(settings.data.autoLockMinutes)
  const parsed = parseWhole(value, MAX_U32, "Enter whole minutes.")

  const save = useIpcMutation<null, SaveAutoLock>({
    mutationFn: (variables) => settingCommands.setAutoLockMinutes(variables.minutes),
    invalidates: [settingKeys.root],
    optimistic: (client, variables) => {
      patchSettings(client, { autoLockMinutes: variables.minutes })
    },
    successToast: "Auto-lock saved",
    errorTitle: "Could not save the auto-lock timeout",
    onDone: (_data, variables) => {
      setDraft((current) => (current === variables.draft ? null : current))
    },
  })

  return (
    <form
      className="space-y-3 pt-3"
      onSubmit={(e) => {
        e.preventDefault()
        if (parsed.isOk && !save.isPending) {
          save.mutate({ draft: value, minutes: parsed.value })
        }
      }}
    >
      <SectionHeader
        level="subsection"
        title="Auto-lock"
        description="Lock after this many idle minutes. Use 0 to stay unlocked."
      />

      <Field label="Idle minutes" error={parsed.isOk ? null : parsed.error}>
        <div className="flex items-center gap-2">
          <Input
            size="sm"
            isMono
            className="w-24"
            inputMode="numeric"
            value={value}
            onChange={(e) => {
              setDraft(e.target.value)
            }}
          />
          <Button
            type="submit"
            variant="secondary"
            size="sm"
            isLoading={save.isPending}
            disabled={!parsed.isOk}
          >
            Save
          </Button>
        </div>
      </Field>
    </form>
  )
}

interface SecretExport {
  id: "phrase" | "passphrase" | "entropy" | "monero"
  label: string
  icon: IconName
  title: string
  warning: string
  isWordGrid: boolean
  fetchSecret: (password: string) => Promise<string>
}

const EXPORTS: readonly SecretExport[] = [
  {
    id: "phrase",
    label: "Seed phrase...",
    icon: "phrase",
    title: "Export seed phrase",
    warning:
      "Keep these words private. They restore your wallet. If you use a passphrase, you also need it to recover your funds.",
    isWordGrid: true,
    fetchSecret: exportCommands.seedPhrase,
  },
  {
    id: "passphrase",
    label: "View passphrase...",
    icon: "key",
    title: "View passphrase",
    warning:
      "Keep this passphrase private. You need it together with your seed phrase to recover your wallet.",
    isWordGrid: false,
    fetchSecret: exportCommands.seedPassphrase,
  },
  {
    id: "entropy",
    label: "Seed entropy...",
    icon: "entropy",
    title: "Export seed entropy",
    warning:
      "This hex value restores the same keys as your seed phrase. Keep it private and back up your passphrase too, if you use one.",
    isWordGrid: false,
    fetchSecret: exportCommands.seedEntropyHex,
  },
  {
    id: "monero",
    label: "Monero 25-word seed...",
    icon: "key",
    title: "Export Monero seed",
    warning:
      "These 25 words restore your Monero account and all its subaddresses in the official wallet or Feather. Anyone with them can spend your Monero.",
    isWordGrid: true,
    fetchSecret: (password) => exportCommands.accountSeed("monero", password),
  },
]

function ExportSecrets() {
  const { wallet } = useActiveWallet()
  const exportDialog = useDialogPayload<SecretExport>()
  const hasStoredPassphrase = wallet?.passphrasePolicy === "stored"
  const offeredExports = EXPORTS.filter(
    (secretExport) => secretExport.id !== "passphrase" || hasStoredPassphrase,
  )

  return (
    <div className="space-y-3 pt-3">
      <SectionHeader level="subsection" title="Export secrets" />

      <Callout tone="danger" heading="Keep your keys private">
        Anyone with an export can spend your funds.
      </Callout>

      <div className="flex flex-wrap gap-2">
        {offeredExports.map((secretExport) => (
          <Button
            key={secretExport.id}
            variant="secondary"
            size="sm"
            onClick={() => {
              exportDialog.show(secretExport)
            }}
          >
            <Icon name={secretExport.icon} />
            {secretExport.label}
          </Button>
        ))}
      </div>

      {exportDialog.payload === null ? null : (
        <SecretRevealDialog
          key={exportDialog.key}
          {...exportDialog.props}
          title={exportDialog.payload.title}
          warning={exportDialog.payload.warning}
          isWordGrid={exportDialog.payload.isWordGrid}
          fetchSecret={exportDialog.payload.fetchSecret}
        />
      )}
    </div>
  )
}

export function SecurityView() {
  return (
    <>
      <SectionHeader
        level="page"
        title="Security"
        description="Opening this wallet requires your password and this device's system keychain."
      />

      <div className="space-y-3">
        <ChangePasswordForm />
        <AutoLockForm />
        <ExportSecrets />
      </div>
    </>
  )
}
