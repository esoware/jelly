import { useQuery } from "@tanstack/react-query"
import { Navigate, useNavigate, useSearch } from "@tanstack/react-router"
import { useState } from "react"

import type { WalletRecord } from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { walletCommands, walletQueries } from "@/query/wallet.ts"
import { Splash } from "@/shell/startup.tsx"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"
import { DeleteWalletDialog } from "@/widgets/dialogs.tsx"
import { WalletAvatar, walletBadges } from "@/widgets/logos.tsx"

interface UnlockWallet {
  walletId: number
  password: string
  passphrase: string | null
}

export function UnlockView() {
  const preselectedId = useSearch({ from: "/unlock", select: (search) => search.walletId ?? null })
  const navigate = useNavigate()
  const wallets = useQuery(walletQueries.all())

  const [chosenId, setChosenId] = useState(preselectedId)
  const [password, setPassword] = useState("")
  const [passphrase, setPassphrase] = useState("")
  const deleteDialog = useDialogPayload<WalletRecord>()

  const unlock = useIpcMutation<null, UnlockWallet>({
    mutationFn: (variables) =>
      walletCommands.unlock(variables.walletId, variables.password, variables.passphrase),
    errorTitle: "Could not unlock the wallet",
    onDone: () => {
      void navigate({ to: "/wallet", replace: true })
    },
  })

  if (wallets.isPending) {
    return <Splash />
  }

  if (wallets.isError) {
    return (
      <div className="flex h-full scroll-y-4 items-center justify-center">
        <div className="w-72 space-y-3">
          <Callout tone="danger" heading="Could not list your wallets">
            {describeFailure(wallets.error)}
          </Callout>
          <Button
            variant="secondary"
            className="w-full"
            onClick={() => {
              void wallets.refetch()
            }}
          >
            Try again
          </Button>
        </div>
      </div>
    )
  }

  const rows = wallets.data
  const selected = rows.find((row) => row.id === chosenId) ?? rows.at(0)
  if (selected === undefined) {
    return <Navigate to="/setup" replace />
  }

  const isPassphraseRequired = selected.passphrasePolicy === "prompt"
  const isWrongPassword = hasCode(unlock.error, "wrong_password")
  const passwordError = isWrongPassword ? describeFailure(unlock.error) : null
  const formError = unlock.error === null || isWrongPassword ? null : describeFailure(unlock.error)
  const isBlocked = unlock.isPending || password.length === 0

  function clearFailure(): void {
    if (unlock.isError) {
      unlock.reset()
    }
  }

  return (
    <div className="flex h-full scroll-y-4 items-center justify-center">
      <form
        className="w-72 space-y-3 py-4"
        onSubmit={(e) => {
          e.preventDefault()
          if (isBlocked) {
            return
          }
          unlock.mutate({
            walletId: selected.id,
            password,
            passphrase: isPassphraseRequired ? passphrase : null,
          })
        }}
      >
        {rows.length > 1 ? (
          <Field label="Wallet">
            <Select
              value={String(selected.id)}
              onValueChange={(value) => {
                setChosenId(Number(value))
                setPassphrase("")
                clearFailure()
              }}
            >
              <SelectTrigger className="w-full">
                <WalletAvatar color={selected.color} />
                {selected.name}
              </SelectTrigger>
              <SelectPopup>
                {rows.map((row) => (
                  <SelectItem key={row.id} value={String(row.id)} badges={walletBadges(row)}>
                    <WalletAvatar color={row.color} />
                    {row.name}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          </Field>
        ) : null}

        <Field label="Password" error={passwordError}>
          <PasswordInput
            autoFocus
            autoComplete="current-password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value)
              clearFailure()
            }}
          />
        </Field>

        {isPassphraseRequired ? (
          <Field label="Passphrase" hint="Enter the passphrase you used to create this wallet.">
            <PasswordInput
              autoComplete="off"
              value={passphrase}
              onChange={(e) => {
                setPassphrase(e.target.value)
                clearFailure()
              }}
            />
          </Field>
        ) : null}

        {formError === null ? null : (
          <Callout tone="danger" heading="Could not unlock the wallet">
            {formError}
          </Callout>
        )}

        <Button type="submit" className="w-full" isLoading={unlock.isPending} disabled={isBlocked}>
          Unlock
        </Button>

        <div className="flex justify-center">
          <Button
            variant="ghost-danger"
            size="sm"
            disabled={unlock.isPending}
            onClick={() => {
              deleteDialog.show(selected)
            }}
          >
            <Icon name="remove" />
            Delete wallet
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              void navigate({ to: "/setup" })
            }}
          >
            <Icon name="add" />
            Add wallet
          </Button>
        </div>
      </form>
      {deleteDialog.payload === null ? null : (
        <DeleteWalletDialog
          {...deleteDialog.props}
          wallet={deleteDialog.payload}
          onDeleted={() => {
            setChosenId(null)
            setPassword("")
            setPassphrase("")
            unlock.reset()
          }}
        />
      )}
    </div>
  )
}
