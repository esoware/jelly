import { useState } from "react"

import type { ProxySetting } from "@/ipc/bindings.gen.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Field } from "@/ui/field.tsx"
import { Input } from "@/ui/input.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"

type ProxyRoute = "direct" | ProxySetting["kind"]

const ROUTES: readonly ProxyRoute[] = ["direct", "system", "socks5h", "http"]

const ROUTE_LABELS: Record<ProxyRoute, string> = {
  direct: "Direct",
  system: "System proxy",
  socks5h: "SOCKS5",
  http: "HTTP proxy",
}

const ROUTE_HINTS: Record<ProxyRoute, string> = {
  direct: "Connect without a proxy.",
  system: "Use your system's proxy settings.",
  socks5h: "The proxy resolves hostnames. For a local Tor proxy, try 127.0.0.1:9050.",
  http: "Use an HTTP CONNECT proxy, including for Electrum.",
}

const ROUTE_PLACEHOLDERS: Record<ProxyRoute, string> = {
  direct: "",
  system: "",
  socks5h: "socks5h://127.0.0.1:9050",
  http: "http://127.0.0.1:8080",
}

interface ProxyDraft {
  route: ProxyRoute
  url: string
}

function draftOf(proxy: ProxySetting | null): ProxyDraft {
  if (proxy === null) {
    return { route: "direct", url: "" }
  }
  if (proxy.kind === "system") {
    return { route: "system", url: "" }
  }
  return { route: proxy.kind, url: proxy.url }
}

function settingOf(draft: ProxyDraft): ProxySetting | null {
  switch (draft.route) {
    case "direct": {
      return null
    }
    case "system": {
      return { kind: "system" }
    }
    case "socks5h": {
      return { kind: "socks5h", url: draft.url.trim() }
    }
    case "http": {
      return { kind: "http", url: draft.url.trim() }
    }
  }
}

function hasUrl(route: ProxyRoute): boolean {
  return route === "socks5h" || route === "http"
}

function isSameDraft(a: ProxyDraft, b: ProxyDraft): boolean {
  return a.route === b.route && a.url.trim() === b.url.trim()
}

interface ProxyEditorProps {
  proxy: ProxySetting | null
  onSave: (proxy: ProxySetting | null) => void
  isPending: boolean
}

export function ProxyEditor({ proxy, onSave, isPending }: ProxyEditorProps) {
  const stored = draftOf(proxy)
  const [draft, setDraft] = useState(stored)
  const isMissing = hasUrl(draft.route) && draft.url.trim() === ""

  return (
    <form
      className="space-y-3"
      onSubmit={(e) => {
        e.preventDefault()
        if (!isMissing && !isPending) {
          onSave(settingOf(draft))
        }
      }}
    >
      <SectionHeader
        level="subsection"
        title="Connection"
        description="All network requests use this connection, including price updates."
      />

      <Field
        label="Connect through"
        hint={ROUTE_HINTS[draft.route]}
        error={isMissing ? "Enter the proxy address." : null}
      >
        <div className="flex items-center gap-2">
          <Select
            value={draft.route}
            onValueChange={(route: ProxyRoute) => {
              setDraft({ route, url: hasUrl(route) && hasUrl(draft.route) ? draft.url : "" })
            }}
          >
            <SelectTrigger size="sm" className="w-52">
              {ROUTE_LABELS[draft.route]}
            </SelectTrigger>
            <SelectPopup>
              {ROUTES.map((option) => (
                <SelectItem key={option} value={option}>
                  {ROUTE_LABELS[option]}
                </SelectItem>
              ))}
            </SelectPopup>
          </Select>

          {hasUrl(draft.route) ? (
            <Input
              size="sm"
              isMono
              className="min-w-0 flex-1"
              autoComplete="off"
              placeholder={ROUTE_PLACEHOLDERS[draft.route]}
              value={draft.url}
              onChange={(e) => {
                setDraft({ route: draft.route, url: e.target.value })
              }}
            />
          ) : null}

          <Button
            type="submit"
            variant="secondary"
            size="sm"
            isLoading={isPending}
            disabled={isPending || isMissing || isSameDraft(draft, stored)}
          >
            Save
          </Button>
        </div>
      </Field>

      <Callout tone="info" heading="Connection changes apply to every chain">
        Jelly uses these settings the next time it connects.
      </Callout>
    </form>
  )
}
