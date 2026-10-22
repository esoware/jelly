import { useQuery } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { useState } from "react"

import type { WalletRecord } from "@/ipc/bindings.gen.ts"
import { formatTimestamp } from "@/lib/format.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { useActiveWallet, walletCommands, walletKeys, walletQueries } from "@/query/wallet.ts"
import { Badge } from "@/ui/badge.tsx"
import { Button } from "@/ui/button.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Row } from "@/ui/row.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { DeleteWalletDialog } from "@/widgets/dialogs.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"

interface WalletRowProps {
  wallet: WalletRecord
  isActive: boolean
  isRenamePending: boolean
  draft: string | null
  onEdit: () => void
  onDraftChange: (value: string) => void
  onCommit: () => void
  onCancel: () => void
  onDelete: () => void
}

function WalletRow({
  wallet,
  isActive,
  isRenamePending,
  draft,
  onEdit,
  onDraftChange,
  onCommit,
  onCancel,
  onDelete,
}: WalletRowProps) {
  const chainCount = wallet.enabledChains.length

  return (
    <Row size="sm">
      <div className="min-w-0 flex-1">
        {draft === null ? (
          <span className="inline-flex max-w-full items-center gap-2">
            <span className="truncate">{wallet.name}</span>
            {isActive ? <Badge>Open</Badge> : null}
            <Button variant="ghost" size="icon-xs" onClick={onEdit} disabled={isRenamePending}>
              <Icon name="rename" />
            </Button>
          </span>
        ) : (
          <Input
            size="sm"
            autoFocus
            className="max-w-56"
            value={draft}
            disabled={isRenamePending}
            onChange={(e) => {
              onDraftChange(e.target.value)
            }}
            onBlur={onCommit}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.currentTarget.blur()
              }
              if (e.key === "Escape") {
                onCancel()
              }
            }}
          />
        )}
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

const SKELETON_ROWS = 2

interface RenameWallet {
  id: number
  name: string
}

export function WalletsView() {
  const navigate = useNavigate()
  const wallets = useQuery(walletQueries.all())
  const { walletId } = useActiveWallet()
  const [edit, setEdit] = useState<{ id: number; draft: string } | null>(null)
  const deleteDialog = useDialogPayload<WalletRecord>()

  const rename = useIpcMutation<null, RenameWallet>({
    mutationFn: (variables) => walletCommands.rename(variables.id, variables.name),
    invalidates: [walletKeys.root],
    successToast: "Wallet renamed",
    errorTitle: "Could not rename the wallet",
    onDone: () => {
      setEdit(null)
    },
  })

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
                isRenamePending={rename.isPending}
                draft={edit?.id === wallet.id ? edit.draft : null}
                onEdit={() => {
                  setEdit({ id: wallet.id, draft: wallet.name })
                }}
                onDraftChange={(value) => {
                  setEdit({ id: wallet.id, draft: value })
                }}
                onCommit={() => {
                  if (rename.isPending) {
                    return
                  }
                  const name = edit?.id === wallet.id ? edit.draft.trim() : ""
                  if (name === "" || name === wallet.name) {
                    setEdit(null)
                  } else {
                    rename.mutate({ id: wallet.id, name })
                  }
                }}
                onCancel={() => {
                  setEdit(null)
                }}
                onDelete={() => {
                  deleteDialog.show(wallet)
                }}
              />
            ))}
          </ListStatus>
        </div>

        {deleteDialog.payload === null ? null : (
          <DeleteWalletDialog
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
