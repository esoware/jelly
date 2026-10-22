import { useQuery, useSuspenseQuery } from "@tanstack/react-query"
import { Fragment, useState } from "react"

import type { ChainId, EndpointRecord, ProbeReport, ProxySetting } from "@/ipc/bindings.gen.ts"
import {
  ORIGIN_LABELS,
  TRANSPORT_LABELS,
  allChains,
  chainName,
  probeDetail,
  probeVerdict,
} from "@/lib/chain.ts"
import { endpointCommands, endpointQueries } from "@/query/endpoint.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Checkbox } from "@/ui/checkbox.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Input } from "@/ui/input.tsx"
import { Row } from "@/ui/row.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"
import { Switch } from "@/ui/switch.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import { ProxyEditor } from "@/widgets/proxy-editor.tsx"

function ProxyForm() {
  const settings = useSuspenseQuery(settingQueries.all())
  const save = useIpcMutation<null, ProxySetting | null>({
    mutationFn: settingCommands.setProxy,
    invalidates: [settingKeys.root],
    successToast: "Proxy settings saved",
    errorTitle: "Could not save the proxy settings",
  })

  return (
    <ProxyEditor
      key={JSON.stringify(settings.data.proxy)}
      proxy={settings.data.proxy}
      onSave={(proxy) => {
        save.mutate(proxy)
      }}
      isPending={save.isPending}
    />
  )
}

interface EndpointRowProps {
  endpoint: EndpointRecord
  isProbing: boolean
  onProbe: () => void
  onPinnedChange: (isPinned: boolean) => void
  onEnabledChange: (isEnabled: boolean) => void
  onRemove: () => void
}

function EndpointRow({
  endpoint,
  isProbing,
  onProbe,
  onPinnedChange,
  onEnabledChange,
  onRemove,
}: EndpointRowProps) {
  const isPinned = endpoint.origin === "pinned"

  return (
    <Row size="sm" data-enabled={endpoint.isEnabled} className="data-[enabled=false]:opacity-40">
      <TooltipText
        content={
          <>
            <p>{endpoint.url}</p>
            {endpoint.note === null ? null : <p className="text-fg-faint">{endpoint.note}</p>}
            <p className="text-fg-faint">
              {TRANSPORT_LABELS[endpoint.transport]}, {ORIGIN_LABELS[endpoint.origin]}
            </p>
          </>
        }
        render={<span className="min-w-0 flex-1 truncate font-mono text-caption" />}
      >
        {endpoint.url}
      </TooltipText>

      <Button variant="ghost" size="icon-sm" isLoading={isProbing} onClick={onProbe}>
        <Icon name="probe" />
      </Button>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => {
          onPinnedChange(!isPinned)
        }}
      >
        <Icon name={isPinned ? "pinOff" : "pin"} />
      </Button>
      {endpoint.origin === "default" ? null : (
        <Button variant="ghost-danger" size="icon-sm" onClick={onRemove}>
          <Icon name="remove" />
        </Button>
      )}
      <Switch checked={endpoint.isEnabled} onCheckedChange={onEnabledChange} />
    </Row>
  )
}

interface AddEndpoint {
  chain: ChainId
  url: string
  shouldAllowInsecure: boolean
}

interface AddEndpointFormProps {
  chain: ChainId
}

function AddEndpointForm({ chain }: AddEndpointFormProps) {
  const [url, setUrl] = useState("")
  const [shouldAllowInsecure, setShouldAllowInsecure] = useState(false)
  const trimmedUrl = url.trim()

  const add = useIpcMutation<null, AddEndpoint>({
    mutationFn: (variables) =>
      endpointCommands.add(variables.chain, variables.url, variables.shouldAllowInsecure),
    invalidates: (variables) => [endpointQueries.list(variables.chain).queryKey],
    successToast: "Endpoint added",
    errorTitle: "Could not add the endpoint",
    onDone: (_data, variables) => {
      if (variables.chain === chain) {
        setUrl("")
        setShouldAllowInsecure(false)
      }
    },
  })

  return (
    <form
      className="pt-2"
      onSubmit={(e) => {
        e.preventDefault()
        if (trimmedUrl !== "" && !add.isPending) {
          add.mutate({ chain, url: trimmedUrl, shouldAllowInsecure })
        }
      }}
    >
      <Field label="Add an endpoint" hint="Adding an existing endpoint enables it again.">
        <div className="flex items-center gap-3">
          <Input
            size="sm"
            isMono
            className="min-w-0 flex-1"
            placeholder="https://host or ssl://host:50002"
            autoComplete="off"
            value={url}
            onChange={(e) => {
              setUrl(e.target.value)
            }}
          />
          <label className="flex shrink-0 items-center gap-2 text-body">
            <Checkbox checked={shouldAllowInsecure} onCheckedChange={setShouldAllowInsecure} />
            Allow unencrypted connection
          </label>
          <Button
            type="submit"
            variant="secondary"
            size="sm"
            isLoading={add.isPending}
            disabled={trimmedUrl === ""}
          >
            Add
          </Button>
        </div>
      </Field>
    </form>
  )
}

const SKELETON_ROWS = 3

interface EndpointTarget {
  chain: ChainId
  url: string
}

interface EnableEndpoint extends EndpointTarget {
  isEnabled: boolean
}

interface PinEndpoint extends EndpointTarget {
  isPinned: boolean
}

function ChainEndpoints() {
  const [chain, setChain] = useState<ChainId>("ethereum")

  const endpoints = useQuery(endpointQueries.list(chain))

  const saveEnabled = useIpcMutation<null, EnableEndpoint>({
    mutationFn: (variables) =>
      endpointCommands.setEnabled(variables.chain, variables.url, variables.isEnabled),
    invalidates: (variables) => [endpointQueries.list(variables.chain).queryKey],
    optimistic: (client, variables) => {
      client.setQueryData(endpointQueries.list(variables.chain).queryKey, (rows) =>
        rows?.map((row) =>
          row.url === variables.url ? { ...row, isEnabled: variables.isEnabled } : row,
        ),
      )
    },
    errorTitle: "Could not update the endpoint",
  })

  const savePinned = useIpcMutation<null, PinEndpoint>({
    mutationFn: (variables) =>
      endpointCommands.setPinned(variables.chain, variables.url, variables.isPinned),
    invalidates: (variables) => [endpointQueries.list(variables.chain).queryKey],
    errorTitle: "Could not update the endpoint",
  })

  const remove = useIpcMutation<null, EndpointTarget>({
    mutationFn: (target) => endpointCommands.remove(target.chain, target.url),
    invalidates: (target) => [endpointQueries.list(target.chain).queryKey],
    successToast: "Endpoint removed",
    errorTitle: "Could not remove the endpoint",
  })

  const probe = useIpcMutation<ProbeReport, EndpointTarget>({
    mutationFn: (target) => endpointCommands.probe(target.chain, target.url),
    errorTitle: "Could not probe the endpoint",
  })

  const rows = endpoints.data ?? []
  const probed = probe.variables?.chain === chain ? probe.data : undefined
  const probingUrl = probe.isPending && probe.variables.chain === chain ? probe.variables.url : null

  return (
    <div className="space-y-3">
      <SectionHeader
        level="subsection"
        title="Endpoints"
        description="Jelly tries another endpoint when one fails. Pinned endpoints take priority."
        actions={
          <Select value={chain} onValueChange={setChain}>
            <SelectTrigger size="sm" className="w-36">
              {chainName(chain)}
            </SelectTrigger>
            <SelectPopup>
              {allChains().map((option) => (
                <SelectItem key={option} value={option}>
                  {chainName(option)}
                </SelectItem>
              ))}
            </SelectPopup>
          </Select>
        }
      />

      <div className="-mx-2">
        <ListStatus
          state={endpoints}
          isEmpty={rows.length === 0}
          errorTitle="Could not load the endpoints"
          emptyTitle="No endpoints configured"
          emptyDescription={`Add an endpoint below to use ${chainName(chain)}.`}
          skeletonRowCount={SKELETON_ROWS}
          skeletonClassName="h-6"
          isCompact
        >
          {rows.map((endpoint) => (
            <Fragment key={endpoint.url}>
              <EndpointRow
                endpoint={endpoint}
                isProbing={probingUrl === endpoint.url}
                onProbe={() => {
                  probe.mutate({ chain, url: endpoint.url })
                }}
                onPinnedChange={(isPinned) => {
                  savePinned.mutate({ chain, url: endpoint.url, isPinned })
                }}
                onEnabledChange={(isEnabled) => {
                  saveEnabled.mutate({ chain, url: endpoint.url, isEnabled })
                }}
                onRemove={() => {
                  remove.mutate({ chain, url: endpoint.url })
                }}
              />
              {probed !== undefined && probe.variables?.url === endpoint.url ? (
                <Callout {...probeVerdict(probed)}>{probeDetail(probed)}</Callout>
              ) : null}
            </Fragment>
          ))}
        </ListStatus>
      </div>

      <AddEndpointForm chain={chain} />
    </div>
  )
}

export function NetworkView() {
  return (
    <>
      <SectionHeader
        level="page"
        title="Network"
        description="Choose how Jelly connects and which endpoints it uses for each chain."
      />

      <ProxyForm />
      <ChainEndpoints />
    </>
  )
}
