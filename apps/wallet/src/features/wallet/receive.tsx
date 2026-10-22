import { useQuery } from "@tanstack/react-query"
import { QRCodeSVG } from "qrcode.react"
import { useState } from "react"

import type { AssetId, ChainId } from "@/ipc/bindings.gen.ts"
import { assetSymbol, chainName } from "@/lib/chain.ts"
import { addressQueries } from "@/query/address.ts"
import {
  Dialog,
  DialogBody,
  DialogDescription,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Select, SelectItem, SelectPopup, SelectTrigger } from "@/ui/select.tsx"
import { AddressOption } from "@/widgets/address.tsx"
import { CopyButton } from "@/widgets/copyable.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"

interface ReceiveBodyProps {
  chain: ChainId
}

function ReceiveBody({ chain }: ReceiveBodyProps) {
  const addresses = useQuery(addressQueries.list(chain))
  const [chosenId, setChosenId] = useState<number | null>(null)

  const addressRows = addresses.data ?? []
  const selected = addressRows.find((row) => row.id === chosenId) ?? addressRows.at(0) ?? null

  return (
    <ListStatus
      state={addresses}
      isEmpty={selected === null}
      isCompact
      errorTitle="Could not load the addresses"
      emptyTitle="No receiving address"
      emptyDescription="Add an address in the addresses panel to see its QR code."
      skeletonRowCount={1}
      skeletonClassName="h-40 w-full"
    >
      {selected === null ? null : (
        <div className="flex items-start gap-4">
          <div className="shrink-0 rounded-lg bg-qr-ground p-2">
            <QRCodeSVG
              value={selected.address}
              level="M"
              marginSize={0}
              fgColor="var(--color-qr-ink)"
              bgColor="var(--color-qr-ground)"
              className="size-37"
            />
          </div>

          <div className="min-w-0 flex-1 space-y-3">
            <Field
              label="Receiving address"
              hint={
                chain === "xrp"
                  ? "Send only XRP on the XRP Ledger mainnet. Jelly does not require a destination tag. A new account needs enough XRP to cover the network reserve."
                  : `Send only ${chainName(chain)} assets to this address.`
              }
            >
              <Select
                value={String(selected.id)}
                onValueChange={(next) => {
                  setChosenId(Number(next))
                }}
              >
                <SelectTrigger className="w-full">
                  <AddressOption row={selected} />
                </SelectTrigger>
                <SelectPopup shouldMatchAnchor>
                  {addressRows.map((row) => (
                    <SelectItem key={row.id} value={String(row.id)}>
                      <AddressOption row={row} />
                    </SelectItem>
                  ))}
                </SelectPopup>
              </Select>
            </Field>

            <div className="flex items-start gap-2 rounded-lg bg-surface p-2">
              <p className="min-w-0 flex-1 py-1 font-mono text-body break-all select-text">
                {selected.address}
              </p>
              <CopyButton value={selected.address} size="icon-sm" />
            </div>
          </div>
        </div>
      )}
    </ListStatus>
  )
}

interface ReceiveDialogProps {
  chain: ChainId
  asset: AssetId
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function ReceiveDialog({ chain, asset, open, onOpenChange }: ReceiveDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup size="md">
        <DialogHeader>
          <DialogTitle>{`Receive ${assetSymbol(asset)}`}</DialogTitle>
          <DialogDescription>
            {chain === "zcash"
              ? "Share this transparent Zcash address or QR code. Payments are public. Do not send mining rewards directly to this address."
              : `Share your address or QR code to receive ${chainName(chain)} assets.`}
          </DialogDescription>
        </DialogHeader>
        <DialogBody hasFooter={false}>
          <ReceiveBody chain={chain} />
        </DialogBody>
      </DialogPopup>
    </Dialog>
  )
}
