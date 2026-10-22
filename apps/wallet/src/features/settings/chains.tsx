import { useState } from "react"

import { MAX_U32, parseOptionalWhole } from "@/features/settings/layout.tsx"
import type { ChainId, WalletRecord } from "@/ipc/bindings.gen.ts"
import type { Parsed } from "@/lib/amount.ts"
import { allChains, chainName, chainTicker } from "@/lib/chain.ts"
import { chainCommands, chainKeys } from "@/query/chain.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { txKeys } from "@/query/tx.ts"
import { patchWallet, useActiveWallet, walletKeys } from "@/query/wallet.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Field } from "@/ui/field.tsx"
import { Input } from "@/ui/input.tsx"
import { Row } from "@/ui/row.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Switch } from "@/ui/switch.tsx"
import { ChainLogo } from "@/widgets/logos.tsx"

interface ChainRowProps {
  chain: ChainId
  isEnabled: boolean
  onEnabledChange: (isEnabled: boolean) => void
}

interface SavePollInterval {
  draft: string
  pollSecs: number | null
}

function ChainRow({ chain, isEnabled, onEnabledChange }: ChainRowProps) {
  const [pollDraft, setPollDraft] = useState("")
  const parsedDraft = parseOptionalWhole(pollDraft, MAX_U32, "Enter whole seconds.")
  const parsed: Parsed<number | null> =
    parsedDraft.isOk && parsedDraft.value === 0
      ? { isOk: false, error: "Enter at least one second." }
      : parsedDraft
  const savePoll = useIpcMutation<null, SavePollInterval>({
    mutationFn: (variables) => chainCommands.setPollInterval(chain, variables.pollSecs),
    successToast: (_data, variables) =>
      `${chainName(chain)} poll interval ${variables.pollSecs === null ? "reset" : "saved"}`,
    errorTitle: "Could not save the poll interval",
    onDone: (_data, variables) => {
      setPollDraft((current) => (current === variables.draft ? "" : current))
    },
  })

  return (
    <div>
      <Row size="sm">
        <Switch checked={isEnabled} onCheckedChange={onEnabledChange} />
        <ChainLogo chain={chain} />
        <span className="flex w-36 shrink-0 items-baseline gap-1">
          {chainName(chain)}
          <span className="text-caption text-fg-faint">{chainTicker(chain)}</span>
        </span>

        <form
          className="ml-auto flex shrink-0 items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            if (parsed.isOk && !savePoll.isPending) {
              savePoll.mutate({ draft: pollDraft, pollSecs: parsed.value })
            }
          }}
        >
          <span className="text-caption text-fg-muted">Poll every</span>
          <Input
            size="sm"
            isMono
            className="w-16"
            inputMode="numeric"
            placeholder="Default"
            value={pollDraft}
            onChange={(e) => {
              setPollDraft(e.target.value)
            }}
          />
          <span className="text-caption text-fg-faint">s</span>
          <Button
            type="submit"
            variant="secondary"
            size="sm"
            isLoading={savePoll.isPending}
            disabled={!parsed.isOk}
          >
            Save
          </Button>
        </form>
      </Row>
      {parsed.isOk ? null : (
        <p className="px-2 pb-1 text-right text-caption text-danger">{parsed.error}</p>
      )}
    </div>
  )
}

const CHAIN: ChainId = "monero"

interface RestoreHeightFormProps {
  wallet: WalletRecord | null
}

interface SaveRestoreHeight {
  draft: string
  height: number | null
}

function RestoreHeightForm({ wallet }: RestoreHeightFormProps) {
  const [draft, setDraft] = useState<string | null>(null)
  const storedHeight = wallet?.restoreHeights[CHAIN] ?? null
  const value = draft ?? (storedHeight === null ? "" : String(storedHeight))
  const parsed = parseOptionalWhole(value, Number.MAX_SAFE_INTEGER, "Enter a whole block height.")

  const save = useIpcMutation<null, SaveRestoreHeight>({
    mutationFn: (variables) => chainCommands.setRestoreHeight(CHAIN, variables.height),
    invalidates: [walletKeys.root],
    optimistic: (client, variables) => {
      patchWallet(client, wallet?.id ?? null, (row) => {
        const restoreHeights = { ...row.restoreHeights }
        if (variables.height === null) {
          delete restoreHeights[CHAIN]
        } else {
          restoreHeights[CHAIN] = variables.height
        }
        return { ...row, restoreHeights }
      })
    },
    successToast: "Restore height saved",
    errorTitle: "Could not save the restore height",
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
          save.mutate({ draft: value, height: parsed.value })
        }
      }}
    >
      <SectionHeader
        level="subsection"
        title={`${chainName(CHAIN)} restore height`}
        description="Start scanning at this block."
      />

      <Callout tone="warn" heading="Choose a block before your first payment">
        A later block can leave funds out of your balance. Lowering the height restarts the scan. A
        blank height or 0 scans the entire chain, which can take hours.
      </Callout>

      <Field label="Height" error={parsed.isOk ? null : parsed.error}>
        <div className="flex items-center gap-2">
          <Input
            size="sm"
            isMono
            className="w-32"
            inputMode="numeric"
            placeholder="Not set"
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

interface ChainToggle {
  chain: ChainId
  isEnabled: boolean
}

export function ChainsView() {
  const { wallet } = useActiveWallet()

  const toggle = useIpcMutation<null, ChainToggle>({
    mutationFn: (variables) => chainCommands.setEnabled(variables.chain, variables.isEnabled),
    invalidates: [chainKeys.root, walletKeys.root, txKeys.root],
    optimistic: (client, variables) => {
      patchWallet(client, wallet?.id ?? null, (row) => {
        const others = row.enabledChains.filter((entry) => entry !== variables.chain)
        return {
          ...row,
          enabledChains: variables.isEnabled ? [...others, variables.chain] : others,
        }
      })
    },
    errorTitle: "Could not change the chain",
  })

  const enabledChains = new Set(wallet?.enabledChains)

  return (
    <>
      <SectionHeader
        level="page"
        title="Chains"
        description="Disabling a chain stops updates but keeps its addresses and history."
      />

      <div className="space-y-3">
        <div className="-mx-2">
          {allChains().map((chain) => (
            <ChainRow
              key={chain}
              chain={chain}
              isEnabled={enabledChains.has(chain)}
              onEnabledChange={(isEnabled) => {
                toggle.mutate({ chain, isEnabled })
              }}
            />
          ))}
        </div>

        <RestoreHeightForm wallet={wallet} />
      </div>
    </>
  )
}
