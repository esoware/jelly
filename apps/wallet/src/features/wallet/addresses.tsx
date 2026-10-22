import { useQuery } from "@tanstack/react-query"
import { useState } from "react"

import {
  CustomPathDialog,
  DeleteKeyDialog,
  GenerateKeyDialog,
  ImportKeyDialog,
  NewAddressDialog,
  RemoveAddressDialog,
} from "@/features/wallet/address-dialogs.tsx"
import type { AddressRecord, AssetId, BalanceRecord, ChainId } from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import {
  SCRIPT_LABELS,
  chainMeta,
  chainName,
  describeKeySource,
  explorerAddressUrl,
  hasScriptChoice,
  isStandaloneKey,
  keyHolders,
} from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { NONE, trimmedOrNull } from "@/lib/format.ts"
import { addressBalance } from "@/lib/portfolio.ts"
import { addressCommands, addressKeys, addressQueries } from "@/query/address.ts"
import { exportCommands } from "@/query/export.ts"
import { moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { Badge } from "@/ui/badge.tsx"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Card } from "@/ui/card.tsx"
import { Collapsible, CollapsiblePanel, CollapsibleTrigger } from "@/ui/collapsible.tsx"
import {
  Dialog,
  DialogBody,
  DialogClose,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
  useDialogPayload,
} from "@/ui/dialog.tsx"
import type { DialogPayload, DialogStateProps } from "@/ui/dialog.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Menu, MenuGroup, MenuItem, MenuPopup, MenuTrigger } from "@/ui/menu.tsx"
import { MiddleTruncate } from "@/ui/middle-truncate.tsx"
import { Panel, PanelBody, PanelHeader } from "@/ui/panel.tsx"
import { Row, RowActions } from "@/ui/row.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { ExplorerActions } from "@/widgets/address.tsx"
import { CryptoAmount } from "@/widgets/amounts.tsx"
import { CopyButton, DetailList, MonoWithCopy } from "@/widgets/copyable.tsx"
import type { DetailRow } from "@/widgets/copyable.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import type { ListState } from "@/widgets/list-status.tsx"
import { SecretRevealDialog } from "@/widgets/secret-reveal.tsx"

type AddressDialog =
  | { kind: "new" }
  | { kind: "path" }
  | { kind: "generate" }
  | { kind: "import" }
  | { kind: "details"; addressId: number }
  | { kind: "export"; addressId: number }
  | { kind: "remove"; addressId: number }
  | { kind: "delete-key"; addressId: number }

interface LabelChange {
  addressId: number
  label: string | null
}

interface AddressList {
  state: ListState
  rows: readonly AddressRecord[]
  balanceOf: (addressId: number, asset: AssetId) => BalanceRecord | undefined
  rename: (row: AddressRecord, draft: string) => void
}

function useAddressList(chain: ChainId): AddressList {
  const addresses = useQuery(addressQueries.list(chain))
  const balances = useQuery(moneyQueries.balances())

  const setLabel = useIpcMutation<null, LabelChange>({
    mutationFn: (variables) => addressCommands.setLabel(variables.addressId, variables.label),
    errorTitle: "Could not save the label",
    invalidates: [addressKeys.root],
    optimistic: (client, variables) => {
      client.setQueryData(addressQueries.list(chain).queryKey, (current) =>
        current?.map((row) =>
          row.id === variables.addressId ? { ...row, label: variables.label } : row,
        ),
      )
    },
  })

  return {
    state: addresses,
    rows: addresses.data ?? [],
    balanceOf: (addressId, asset) => addressBalance(balances.data ?? [], addressId, asset),
    rename: (row, draft) => {
      const label = trimmedOrNull(draft)
      if (label === row.label) {
        return
      }
      setLabel.mutate({ addressId: row.id, label })
    },
  }
}

interface LabelFieldProps {
  label: string | null
  onCommit: (draft: string) => void
}

function LabelField({ label, onCommit }: LabelFieldProps) {
  const [draft, setDraft] = useState<string | null>(null)

  if (draft !== null) {
    return (
      <Input
        autoFocus
        size="sm"
        autoComplete="off"
        placeholder="Label"
        className="w-full"
        value={draft}
        onChange={(e) => {
          setDraft(e.target.value)
        }}
        onClick={(e) => {
          e.stopPropagation()
        }}
        onBlur={() => {
          setDraft(null)
          onCommit(draft)
        }}
        onKeyDown={(e) => {
          e.stopPropagation()
          if (e.key === "Enter") {
            e.currentTarget.blur()
          }
          if (e.key === "Escape") {
            setDraft(null)
          }
        }}
      />
    )
  }

  return (
    <button
      type="button"
      className="max-w-full truncate rounded-sm text-left transition-colors hover:text-fg"
      onClick={(e) => {
        e.stopPropagation()
        setDraft(label ?? "")
      }}
    >
      {label ?? <span className="text-fg-faint">Unnamed</span>}
    </button>
  )
}

interface AddressMenuProps {
  row: AddressRecord
  isKeyShared: boolean
  onOpen: (dialog: AddressDialog) => void
}

function AddressMenu({ row, isKeyShared, onOpen }: AddressMenuProps) {
  const isStandalone = isStandaloneKey(row.source)
  return (
    <Menu>
      <MenuTrigger render={<Button variant="ghost" size="icon-xs" />}>
        <Icon name="more" />
      </MenuTrigger>
      <MenuPopup align="end">
        <MenuGroup>
          <MenuItem
            onClick={() => {
              onOpen({ kind: "details", addressId: row.id })
            }}
          >
            <Icon name="info" />
            Details
          </MenuItem>
          <MenuItem
            onClick={() => {
              onOpen({ kind: "export", addressId: row.id })
            }}
          >
            <Icon name="key" />
            Export private key...
          </MenuItem>
        </MenuGroup>

        <MenuGroup>
          {!isStandalone || isKeyShared ? (
            <MenuItem
              tone="danger"
              onClick={() => {
                onOpen({ kind: "remove", addressId: row.id })
              }}
            >
              <Icon name="remove" />
              Remove...
            </MenuItem>
          ) : null}
          {isStandalone ? (
            <MenuItem
              tone="danger"
              onClick={() => {
                onOpen({ kind: "delete-key", addressId: row.id })
              }}
            >
              <Icon name="destroy" />
              Delete key...
            </MenuItem>
          ) : null}
        </MenuGroup>
      </MenuPopup>
    </Menu>
  )
}

interface AddressRowProps {
  list: AddressList
  row: AddressRecord
  asset: AssetId
  onOpen: (dialog: AddressDialog) => void
}

function AddressRow({ list, row, asset, onOpen }: AddressRowProps) {
  const balance = list.balanceOf(row.id, asset)

  return (
    <Row
      onActivate={() => {
        onOpen({ kind: "details", addressId: row.id })
      }}
    >
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="flex min-w-0 items-center gap-2 text-body">
          <LabelField
            label={row.label}
            onCommit={(draft) => {
              list.rename(row, draft)
            }}
          />
          {isStandaloneKey(row.source) ? <Badge>Standalone</Badge> : null}
          <RowActions>
            <CopyButton value={row.address} />
            <AddressMenu
              row={row}
              isKeyShared={keyHolders(list.rows, row).length > 1}
              onOpen={onOpen}
            />
          </RowActions>
        </span>
        <MiddleTruncate value={row.address} className="font-mono text-caption text-fg-muted" />
      </div>
      <div className="shrink-0 text-caption text-fg-muted">
        {balance === undefined ? null : (
          <CryptoAmount amount={balance.amount} asset={balance.asset} isCompact />
        )}
      </div>
    </Row>
  )
}

interface AddAddressMenuProps {
  chain: ChainId
  onOpen: (dialog: AddressDialog) => void
}

function AddAddressMenu({ chain, onOpen }: AddAddressMenuProps) {
  const { hasSubaddresses, canHoldStandaloneKeys } = chainMeta(chain)
  return (
    <Menu>
      <MenuTrigger render={<Button variant="ghost" size="icon-sm" />}>
        <Icon name="add" />
      </MenuTrigger>
      <MenuPopup align="end">
        <MenuGroup label="From seed phrase">
          <MenuItem
            onClick={() => {
              onOpen({ kind: "new" })
            }}
          >
            <Icon name="add" />
            {hasSubaddresses ? "New subaddress..." : "New address..."}
          </MenuItem>
          {hasSubaddresses ? null : (
            <MenuItem
              onClick={() => {
                onOpen({ kind: "path" })
              }}
            >
              <Icon name="phrase" />
              Address at a custom path...
            </MenuItem>
          )}
        </MenuGroup>

        {canHoldStandaloneKeys ? (
          <MenuGroup label="Standalone key">
            <MenuItem
              onClick={() => {
                onOpen({ kind: "generate" })
              }}
            >
              <Icon name="key" />
              Generate key...
            </MenuItem>
            <MenuItem
              onClick={() => {
                onOpen({ kind: "import" })
              }}
            >
              <Icon name="entropy" />
              Import key...
            </MenuItem>
          </MenuGroup>
        ) : null}
      </MenuPopup>
    </Menu>
  )
}

const UNSUPPORTED_XPUB =
  "This address has no account xpub. Xpubs are unavailable for Solana, TON, Monero, standalone keys, and paths without a hardened account prefix."

const XPUB_CAVEAT =
  "An xpub reveals all addresses derived from it. Share it only with people or services you trust."

interface XpubCardProps {
  addressId: number
  derivedPath: string | null
}

function XpubCard({ addressId, derivedPath }: XpubCardProps) {
  const [shouldFetchXpub, setShouldFetchXpub] = useState(false)
  const [path, setPath] = useState("")
  const [lookupPath, setLookupPath] = useState<string | null>(null)

  const xpub = useQuery(addressQueries.xpubFor(shouldFetchXpub ? addressId : null))
  const lookup = useQuery(addressQueries.xpubAt(lookupPath))

  const xpubValue = xpub.data ?? null
  const isUnsupported = hasCode(xpub.error, "unsupported") || (xpub.isSuccess && xpubValue === null)

  return (
    <Card className="space-y-2 p-3">
      <SectionHeader
        level="subsection"
        title="Account xpub"
        description="Use this extended public key to watch the account without its private keys."
        actions={
          <Button
            size="sm"
            variant="secondary"
            isLoading={xpub.isFetching}
            onClick={() => {
              setShouldFetchXpub(true)
            }}
          >
            Fetch
          </Button>
        }
      />

      {xpubValue === null ? null : (
        <div className="rounded-md bg-surface p-2">
          <MonoWithCopy value={xpubValue} />
        </div>
      )}

      {isUnsupported ? <p className="text-caption text-fg-muted">{UNSUPPORTED_XPUB}</p> : null}
      {!isUnsupported && xpub.error !== null ? (
        <Callout tone="danger" heading="Could not fetch the public key">
          {describeFailure(xpub.error)}
        </Callout>
      ) : null}

      {derivedPath === null ? null : (
        <Collapsible>
          <CollapsibleTrigger>Fetch an xpub at any path</CollapsibleTrigger>
          <CollapsiblePanel className="space-y-2 pt-2">
            <div className="flex gap-2">
              <Input
                isMono
                size="sm"
                autoComplete="off"
                placeholder={derivedPath}
                value={path}
                onChange={(e) => {
                  setPath(e.target.value)
                }}
              />
              <Button
                size="sm"
                variant="secondary"
                isLoading={lookup.isFetching}
                onClick={() => {
                  setLookupPath(trimmedOrNull(path))
                }}
              >
                Fetch
              </Button>
            </div>

            {lookup.data === undefined ? null : (
              <div className="rounded-md bg-surface p-2">
                <MonoWithCopy value={lookup.data} />
              </div>
            )}
            {lookup.error === null ? null : (
              <Callout tone="danger" heading="Could not fetch the public key">
                {describeFailure(lookup.error)}
              </Callout>
            )}
          </CollapsiblePanel>
        </Collapsible>
      )}

      <p className="text-caption text-fg-faint">{XPUB_CAVEAT}</p>
    </Card>
  )
}

interface AddressDetailsProps {
  address: AddressRecord
}

function AddressDetails({ address }: AddressDetailsProps) {
  const explorer = explorerAddressUrl(address.chain, address.address)
  const derivedPath = address.source.kind === "derived" ? address.source.path : null
  const items: readonly DetailRow[] = [
    { label: "Chain", value: chainName(address.chain) },
    { label: "Key source", value: describeKeySource(address.source) },
    ...(address.scriptKind === null || !hasScriptChoice(address.chain)
      ? []
      : [{ label: "Address type", value: SCRIPT_LABELS[address.scriptKind] }]),
    {
      label: "Derivation index",
      value: address.derivationIndex === null ? NONE : String(address.derivationIndex),
    },
    { label: "Label", value: address.label ?? NONE },
  ]

  return (
    <>
      <DetailList items={items} />
      {explorer === null ? null : (
        <div className="flex gap-2">
          <ExplorerActions
            url={explorer}
            onOpen={() => addressCommands.openExplorer(address.chain, address.address)}
          />
        </div>
      )}
      <XpubCard addressId={address.id} derivedPath={derivedPath} />
    </>
  )
}

interface AddressDetailsDialogProps extends DialogStateProps {
  chain: ChainId
  addressId: number
}

function AddressDetailsDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  chain,
  addressId,
}: AddressDetailsDialogProps) {
  const match = useQuery({
    ...addressQueries.list(chain),
    select: (rows) => rows.find((row) => row.id === addressId) ?? null,
  })
  const address = match.data ?? null

  return (
    <Dialog open={open} onOpenChange={onOpenChange} onOpenChangeComplete={onOpenChangeComplete}>
      <DialogPopup size="lg">
        <DialogHeader>
          <DialogTitle>Address details</DialogTitle>
          {address === null ? null : (
            <DialogDescription render={<div />}>
              <MonoWithCopy value={address.address} />
            </DialogDescription>
          )}
        </DialogHeader>

        <DialogBody>
          {address === null ? (
            <EmptyState isCompact title="This address is no longer in the list" />
          ) : (
            <AddressDetails address={address} />
          )}
        </DialogBody>

        <DialogFooter>
          <DialogClose render={<Button variant="secondary" />}>Close</DialogClose>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  )
}

const EXPORT_WARNING =
  "Anyone with this private key can spend the funds at this address. Keep it private."

interface AddressDialogsProps {
  chain: ChainId
  rows: readonly AddressRecord[]
  dialog: DialogPayload<AddressDialog>
}

function AddressDialogs({ chain, rows, dialog }: AddressDialogsProps) {
  switch (dialog.payload?.kind) {
    case "new": {
      return <NewAddressDialog {...dialog.props} chain={chain} />
    }
    case "path": {
      return <CustomPathDialog {...dialog.props} chain={chain} />
    }
    case "generate": {
      return <GenerateKeyDialog {...dialog.props} chain={chain} />
    }
    case "import": {
      return <ImportKeyDialog {...dialog.props} chain={chain} />
    }
    case "details": {
      return (
        <AddressDetailsDialog
          {...dialog.props}
          chain={chain}
          addressId={dialog.payload.addressId}
        />
      )
    }
    case "export": {
      const { addressId } = dialog.payload
      return (
        <SecretRevealDialog
          {...dialog.props}
          title="Export private key"
          warning={EXPORT_WARNING}
          fetchSecret={(password) => exportCommands.privateKey(addressId, password)}
        />
      )
    }
    case "remove": {
      const { addressId } = dialog.payload
      const row = rows.find((entry) => entry.id === addressId)
      return row === undefined ? null : <RemoveAddressDialog {...dialog.props} row={row} />
    }
    case "delete-key": {
      return <DeleteKeyDialog {...dialog.props} rows={rows} addressId={dialog.payload.addressId} />
    }
    case undefined: {
      return null
    }
  }
}

const SKELETON_ROWS = 4

interface AddressesPanelProps {
  chain: ChainId
  asset: AssetId
}

export function AddressesPanel({ chain, asset }: AddressesPanelProps) {
  const list = useAddressList(chain)
  const dialog = useDialogPayload<AddressDialog>()

  return (
    <Panel className="flex-1">
      <PanelHeader
        title="Addresses"
        count={list.rows.length}
        actions={
          <AddAddressMenu
            chain={chain}
            onOpen={(next) => {
              dialog.show(next)
            }}
          />
        }
      />

      <PanelBody>
        <ListStatus
          state={list.state}
          isEmpty={list.rows.length === 0}
          errorTitle="Could not load the addresses"
          emptyTitle="No addresses yet"
          emptyDescription={
            chainMeta(chain).canHoldStandaloneKeys
              ? "Derive one from your seed phrase, or add a standalone key."
              : "Derive one from your seed phrase."
          }
          skeletonRowCount={SKELETON_ROWS}
          isCompact
        >
          {list.rows.map((row) => (
            <AddressRow
              key={row.id}
              list={list}
              row={row}
              asset={asset}
              onOpen={(next) => {
                dialog.show(next)
              }}
            />
          ))}
        </ListStatus>
      </PanelBody>

      <AddressDialogs chain={chain} rows={list.rows} dialog={dialog} />
    </Panel>
  )
}
