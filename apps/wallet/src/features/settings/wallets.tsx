import { useQuery } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { useState } from "react"

import type { WalletRecord } from "@/ipc/bindings.gen.ts"
import { describeFailure } from "@/lib/failure.ts"
import { formatTimestamp } from "@/lib/format.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { useActiveWallet, walletCommands, walletKeys, walletQueries } from "@/query/wallet.ts"
import { Badge } from "@/ui/badge.tsx"
import { Button } from "@/ui/button.tsx"
import { ColorPicker } from "@/ui/color-picker.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Row } from "@/ui/row.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { DeleteWalletDialog, FormDialog } from "@/widgets/dialogs.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import { WalletAvatar } from "@/widgets/logos.tsx"

interface WalletRowProps {
  wallet: WalletRecord
  isActive: boolean
  onEdit: () => void
  onDelete: () => void
}

function WalletRow({ wallet, isActive, onEdit, onDelete }: WalletRowProps) {
  const chainCount = wallet.enabledChains.length

  return (
    <Row size="sm">
      <WalletAvatar color={wallet.color} />
      <div className="min-w-0 flex-1">
        <span className="inline-flex max-w-full items-center gap-2">
          <span className="truncate">{wallet.name}</span>
          {isActive ? <Badge>Open</Badge> : null}
          <Button variant="ghost" size="icon-xs" onClick={onEdit}>
            <Icon name="rename" />
          </Button>
        </span>
      </div>

      <span className="w-28 shrink-0 text-caption text-fg-faint tabular-nums">
        {formatTimestamp(wallet.createdAtUnixSecs)}
      </span>
      <span className="w-14 shrink-0 text-caption text-fg-faint">
        {`${String(chainCount)} ${chainCount === 1 ? "chain" : "chains"}`}
      </span>

      <Button variant="ghost-danger" size="icon-sm" onClick={onDelete}>
        <Icon name="remove" />
      </Button>
    </Row>
  )
}

interface EditWalletDialogProps extends DialogStateProps {
  wallet: WalletRecord
}

function EditWalletDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  wallet,
}: EditWalletDialogProps) {
  const [name, setName] = useState(wallet.name)
  const [color, setColor] = useState(wallet.color)

  const trimmedName = name.trim()
  const isUnchanged = trimmedName === wallet.name && color === wallet.color
  const canSave = trimmedName !== "" && !isUnchanged

  const save = useIpcMutation<null>({
    mutationFn: () => walletCommands.update(wallet.id, trimmedName, color),
    invalidates: [walletKeys.root],
    successToast: "Wallet updated",
    errorTitle: "Could not update the wallet",
    onDone: () => {
      onOpenChange(false)
    },
  })

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title="Edit wallet"
      size="sm"
      submitLabel="Save"
      isSubmitDisabled={!canSave}
      isBusy={save.isPending}
      error={save.error === null ? null : describeFailure(save.error)}
      onSubmit={() => {
        save.mutate()
      }}
    >
      <div className="flex items-start gap-3">
        <Field label="Name" className="min-w-0 flex-1">
          <Input
            autoFocus
            autoComplete="off"
            value={name}
            onChange={(e) => {
              setName(e.target.value)
            }}
          />
        </Field>
        <Field label="Color">
          <ColorPicker
            value={color}
            attachment={<WalletAvatar color={color} />}
            onValueChange={setColor}
          />
        </Field>
      </div>
    </FormDialog>
  )
}

const SKELETON_ROWS = 2

export function WalletsView() {
  const navigate = useNavigate()
  const wallets = useQuery(walletQueries.all())
  const { walletId } = useActiveWallet()
  const editDialog = useDialogPayload<WalletRecord>()
  const deleteDialog = useDialogPayload<WalletRecord>()

  const rows = wallets.data ?? []

  return (
    <>
      <SectionHeader
        level="page"
        title="Wallets"
        description="Manage wallets saved on this device."
      />

      <div className="space-y-3">
        <div className="-mx-2">
          <ListStatus
            state={wallets}
            isEmpty={rows.length === 0}
            errorTitle="Could not load the wallets"
            emptyTitle="No wallets on this device"
            skeletonRowCount={SKELETON_ROWS}
            skeletonClassName="h-6"
            isCompact
          >
            {rows.map((wallet) => (
              <WalletRow
                key={wallet.id}
                wallet={wallet}
                isActive={wallet.id === walletId}
                onEdit={() => {
                  editDialog.show(wallet)
                }}
                onDelete={() => {
                  deleteDialog.show(wallet)
                }}
              />
            ))}
          </ListStatus>
        </div>

        {editDialog.payload === null ? null : (
          <EditWalletDialog
            key={editDialog.key}
            {...editDialog.props}
            wallet={editDialog.payload}
          />
        )}

        {deleteDialog.payload === null ? null : (
          <DeleteWalletDialog
            key={deleteDialog.key}
            {...deleteDialog.props}
            wallet={deleteDialog.payload}
            onDeleted={(deletedId) => {
              if (deletedId === walletId) {
                void navigate({ to: "/", replace: true })
              }
            }}
          />
        )}
      </div>
    </>
  )
}
