import { useState } from "react"
import type { ComponentProps, ReactNode } from "react"

import type { WalletRecord } from "@/ipc/bindings.gen.ts"
import type { IpcError } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { walletCommands, walletKeys, walletQueries } from "@/query/wallet.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import {
  Dialog,
  DialogBody,
  DialogClose,
  DialogDescription,
  DialogFooter,
  DialogForm,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Input } from "@/ui/input.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"

interface FormDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onOpenChangeComplete?: ((open: boolean) => void) | undefined
  title: string
  description?: ReactNode | undefined
  size?: ComponentProps<typeof DialogPopup>["size"]
  submitLabel: string
  isSubmitDisabled?: boolean | undefined
  isDestructive?: boolean | undefined
  isBusy: boolean
  error?: string | null | undefined
  onSubmit: () => void
  children: ReactNode
}

export function FormDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  title,
  description,
  size,
  submitLabel,
  isSubmitDisabled,
  isDestructive = false,
  isBusy,
  error,
  onSubmit,
  children,
}: FormDialogProps) {
  return (
    <Dialog
      open={open}
      disablePointerDismissal={isDestructive}
      isLocked={isBusy}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
    >
      <DialogPopup size={size} hasCloseButton={!isBusy && !isDestructive}>
        <DialogForm
          onSubmit={() => {
            if (!isBusy && isSubmitDisabled !== true) {
              onSubmit()
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description === undefined ? null : (
              <DialogDescription>{description}</DialogDescription>
            )}
          </DialogHeader>
          <DialogBody>
            {children}
            {error === undefined || error === null ? null : (
              <Callout tone="danger" heading="Could not complete the request">
                {error}
              </Callout>
            )}
          </DialogBody>
          <DialogFooter>
            <DialogClose render={<Button variant="secondary" disabled={isBusy} />}>
              Cancel
            </DialogClose>
            <Button
              type="submit"
              variant={isDestructive ? "danger" : "primary"}
              isLoading={isBusy}
              disabled={isSubmitDisabled}
            >
              {submitLabel}
            </Button>
          </DialogFooter>
        </DialogForm>
      </DialogPopup>
    </Dialog>
  )
}

interface TypedConfirmDialogProps extends DialogStateProps {
  title: string
  description: ReactNode
  confirmationText: string
  confirmLabel: string
  isBusy: boolean
  error: IpcError | null
  onConfirm: (confirmation: string, password: string) => void
}

export function TypedConfirmDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  title,
  description,
  confirmationText,
  confirmLabel,
  isBusy,
  error,
  onConfirm,
}: TypedConfirmDialogProps) {
  const [confirmation, setConfirmation] = useState("")
  const [password, setPassword] = useState("")
  const isReady = confirmation === confirmationText && password !== ""

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title={title}
      description={description}
      size="sm"
      submitLabel={confirmLabel}
      isSubmitDisabled={!isReady}
      isDestructive
      isBusy={isBusy}
      error={error === null ? null : describeFailure(error)}
      onSubmit={() => {
        onConfirm(confirmation, password)
      }}
    >
      <Callout tone="danger" heading="Permanent deletion">
        This cannot be undone.
      </Callout>

      <Field
        label="Confirmation"
        hint={
          <>
            Type{" "}
            <span className="font-mono font-semibold break-all text-fg">{confirmationText}</span> to
            confirm.
          </>
        }
      >
        <Input
          autoFocus
          autoComplete="off"
          isMono
          value={confirmation}
          onChange={(e) => {
            setConfirmation(e.target.value)
          }}
        />
      </Field>

      <Field label="Password">
        <PasswordInput
          autoComplete="current-password"
          value={password}
          onChange={(e) => {
            setPassword(e.target.value)
          }}
        />
      </Field>
    </FormDialog>
  )
}

interface DeleteWallet {
  id: number
  password: string
  confirmation: string
}

interface DeleteWalletDialogProps extends DialogStateProps {
  wallet: WalletRecord
  onDeleted: (walletId: number) => void
}

export function DeleteWalletDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  wallet,
  onDeleted,
}: DeleteWalletDialogProps) {
  const destroy = useIpcMutation<null, DeleteWallet>({
    mutationFn: (variables) =>
      walletCommands.destroy(variables.id, variables.password, variables.confirmation),
    invalidates: [walletKeys.root, walletQueries.lockState().queryKey],
    shouldRefreshOnError: true,
    successToast: "Wallet deleted",
    errorTitle: "Could not delete the wallet",
    onDone: (_data, variables) => {
      onOpenChange(false)
      onDeleted(variables.id)
    },
  })

  return (
    <TypedConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title={`Delete ${wallet.name}`}
      description="This deletes the wallet and its keys from this device. Back up your seed phrase, passphrase, and any standalone keys first, or you may lose access to your funds."
      confirmationText={wallet.name}
      confirmLabel="Delete wallet"
      isBusy={destroy.isPending}
      error={destroy.error}
      onConfirm={(confirmation, password) => {
        destroy.mutate({ id: wallet.id, password, confirmation })
      }}
    />
  )
}
