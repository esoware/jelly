import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import type { ReactNode } from "react"

import type { DatabaseRebuilt, ProxySetting } from "@/ipc/bindings.gen.ts"
import { closeWindow, errorFromPayload } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { appCommands, appQueries } from "@/query/app.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { useStartupStatus } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import {
  Dialog,
  DialogBody,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "@/ui/dialog.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Spinner } from "@/ui/spinner.tsx"
import { MonoWithCopy } from "@/widgets/copyable.tsx"
import { ProxyEditor } from "@/widgets/proxy-editor.tsx"

export function Splash() {
  return (
    <div className="flex h-full items-center justify-center gap-2 text-body text-fg-muted">
      <Spinner />
      Opening Jelly...
    </div>
  )
}

interface FileLocationProps {
  label: string
  path: string
}

function FileLocation({ label, path }: FileLocationProps) {
  return (
    <div className="space-y-1">
      <p className="text-caption text-fg-muted">{label}</p>
      <MonoWithCopy value={path} className="text-caption text-fg-faint" />
    </div>
  )
}

interface StartupNoticeProps {
  title: string
  description: string
  isQuitDisabled?: boolean | undefined
  callout: ReactNode
  children: ReactNode
}

function StartupNotice({
  title,
  description,
  isQuitDisabled,
  callout,
  children,
}: StartupNoticeProps) {
  return (
    <div className="flex h-full items-center justify-center px-6">
      <div className="w-full max-w-md space-y-3">
        <EmptyState
          title={title}
          description={description}
          action={
            <Button variant="secondary" size="sm" onClick={closeWindow} disabled={isQuitDisabled}>
              Quit
            </Button>
          }
        />

        {callout}

        {children}
      </div>
    </div>
  )
}

interface StartupFailedProps {
  message: string
}

export function StartupFailed({ message }: StartupFailedProps) {
  const dataDir = useQuery(appQueries.dataDir())

  return (
    <StartupNotice
      title="Jelly could not start"
      description={message}
      callout={
        <Callout tone="danger" heading="No wallet was opened">
          Your wallet files are unchanged. Check the log for details, then restart Jelly.
        </Callout>
      }
    >
      {dataDir.data === undefined ? null : (
        <FileLocation label="Wallet files and logs" path={dataDir.data} />
      )}
    </StartupNotice>
  )
}

const INITIAL_PROXY: ProxySetting = { kind: "socks5h", url: "" }

function ProxyRepair() {
  const [isSaved, setIsSaved] = useState(false)
  const save = useIpcMutation<null, ProxySetting | null>({
    mutationFn: appCommands.repairProxy,
    errorTitle: "Could not save the proxy settings",
    onDone: () => {
      setIsSaved(true)
    },
  })

  return (
    <StartupNotice
      title={isSaved ? "Proxy settings saved" : "Fix your proxy settings"}
      description={
        isSaved
          ? "Close and reopen Jelly to use the new connection settings."
          : "Your saved proxy settings could not be used. Choose a connection to use when you reopen Jelly."
      }
      isQuitDisabled={save.isPending}
      callout={
        <Callout tone="warn" heading="Network access is blocked">
          No wallet was opened. Saving these settings does not connect to the network.
        </Callout>
      }
    >
      {isSaved ? null : (
        <ProxyEditor
          proxy={INITIAL_PROXY}
          onSave={(proxy) => {
            save.mutate(proxy)
          }}
          isPending={save.isPending}
        />
      )}
    </StartupNotice>
  )
}

interface DatabaseRebuiltDialogProps {
  open: boolean
  rebuilt: DatabaseRebuilt
  onAcknowledge: () => void
}

function DatabaseRebuiltDialog({ open, rebuilt, onAcknowledge }: DatabaseRebuiltDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          onAcknowledge()
        }
      }}
    >
      <DialogPopup>
        <DialogHeader>
          <DialogTitle>Jelly reset the database</DialogTitle>
          <DialogDescription>
            Jelly could not read the database and replaced it with an empty one.
          </DialogDescription>
        </DialogHeader>

        <DialogBody>
          <Callout tone="warn" heading="Restore your wallets">
            Your key files are still on this device. Restore each wallet from its seed phrase and
            import its standalone keys again. Jelly will then reload its balances and history.
          </Callout>

          <FileLocation label="Old database" path={rebuilt.movedTo} />
        </DialogBody>

        <DialogFooter>
          <Button onClick={onAcknowledge}>Continue</Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  )
}

interface StartupGateProps {
  children: ReactNode
}

export function StartupGate({ children }: StartupGateProps) {
  const status = useStartupStatus()
  const [isAcknowledged, setIsAcknowledged] = useState(false)

  if (status?.kind === "starting") {
    return <Splash />
  }

  const rebuilt = status?.rebuilt ?? null
  const rebuiltNotice =
    rebuilt === null ? null : (
      <DatabaseRebuiltDialog
        open={!isAcknowledged}
        rebuilt={rebuilt}
        onAcknowledge={() => {
          setIsAcknowledged(true)
        }}
      />
    )

  if (status?.kind === "failed") {
    return (
      <>
        {status.canRepairProxy ? (
          <ProxyRepair />
        ) : (
          <StartupFailed message={describeFailure(errorFromPayload(status.error))} />
        )}
        {rebuiltNotice}
      </>
    )
  }

  return (
    <>
      {children}
      {rebuiltNotice}
    </>
  )
}
