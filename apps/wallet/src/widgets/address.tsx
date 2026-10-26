import type { AddressRecord } from "@/ipc/bindings.gen.ts"
import { copyToClipboard } from "@/ipc/client.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { notify } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import { MiddleTruncate } from "@/ui/middle-truncate.tsx"

interface AddressOptionProps {
  row: AddressRecord
  className?: string | undefined
}

export function AddressOption({ row, className }: AddressOptionProps) {
  return (
    <span className={appendClassName("flex min-w-0 gap-1", className)}>
      {row.label === null ? null : <span className="max-w-1/2 shrink-0 truncate">{row.label}</span>}
      <MiddleTruncate value={row.address} hasTooltip={false} />
    </span>
  )
}

const LISTED_ADDRESSES = 3

interface AddressListProps {
  addresses: readonly string[]
  hasTooltip?: boolean | undefined
  className?: string | undefined
}

export function AddressList({ addresses, hasTooltip, className }: AddressListProps) {
  const shown = addresses.slice(0, LISTED_ADDRESSES)
  const hiddenCount = addresses.length - shown.length
  return (
    <span className={appendClassName("flex min-w-0 gap-1", className)}>
      {shown.map((address, i) => (
        <span key={address} className="flex min-w-0">
          <MiddleTruncate value={address} hasTooltip={hasTooltip} />
          {i < shown.length - 1 ? <span className="shrink-0">,</span> : null}
        </span>
      ))}
      {hiddenCount > 0 ? (
        <span className="shrink-0">{`and ${String(hiddenCount)} more`}</span>
      ) : null}
    </span>
  )
}

interface ExplorerActionsProps {
  url: string
  onOpen: () => Promise<null>
}

export function ExplorerActions({ url, onOpen }: ExplorerActionsProps) {
  const openExplorer = useIpcMutation({
    mutationFn: onOpen,
    errorTitle: "Could not open the browser",
  })

  async function copy(): Promise<void> {
    if (await copyToClipboard(url)) {
      notify.success("Copied the explorer link")
    } else {
      notify.warning("Could not copy the explorer link")
    }
  }

  return (
    <>
      <Button
        size="sm"
        variant="secondary"
        onClick={() => {
          void copy()
        }}
      >
        <Icon name="copy" />
        Explorer link
      </Button>
      <Button
        size="sm"
        variant="secondary"
        isLoading={openExplorer.isPending}
        onClick={() => {
          openExplorer.mutate()
        }}
      >
        <Icon name="external" />
        Open in explorer
      </Button>
    </>
  )
}
