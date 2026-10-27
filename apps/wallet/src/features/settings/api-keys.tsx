import { useSuspenseQuery } from "@tanstack/react-query"
import { useState } from "react"

import type { AppSettings } from "@/ipc/bindings.gen.ts"
import { trimmedOrNull } from "@/lib/format.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { Button } from "@/ui/button.tsx"
import { Field } from "@/ui/field.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"

interface ApiKey {
  id: "etherscan" | "trongrid"
  label: string
  hint: string
  isStored: (settings: AppSettings) => boolean
  save: (key: string | null) => Promise<null>
}

interface SaveApiKey {
  draft: string
  key: string | null
}

interface ApiKeyFormProps {
  apiKey: ApiKey
  isStored: boolean
}

function ApiKeyForm({ apiKey, isStored }: ApiKeyFormProps) {
  const [draft, setDraft] = useState<string | null>(null)
  const save = useIpcMutation<null, SaveApiKey>({
    mutationFn: (variables) => apiKey.save(variables.key),
    invalidates: [settingKeys.root],
    successToast: (_data, variables) =>
      `${apiKey.label} key ${variables.key === null ? "cleared" : "saved"}`,
    errorTitle: "Could not save the key",
    onDone: (_data, variables) => {
      setDraft((current) => (current === variables.draft ? null : current))
    },
  })

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        if (draft !== null && !save.isPending) {
          save.mutate({ draft, key: trimmedOrNull(draft) })
        }
      }}
    >
      <Field label={apiKey.label} hint={apiKey.hint}>
        <div className="flex items-center gap-2">
          <PasswordInput
            size="sm"
            isMono
            className="min-w-0 flex-1"
            autoComplete="off"
            placeholder={isStored ? "Enter a replacement key." : "Enter an API key."}
            value={draft ?? ""}
            onChange={(e) => {
              setDraft(e.target.value)
            }}
          />
          <Button
            type="submit"
            variant="secondary"
            size="sm"
            isLoading={save.isPending}
            disabled={draft === null}
          >
            Save
          </Button>
        </div>
      </Field>
    </form>
  )
}

const API_KEYS: readonly ApiKey[] = [
  {
    id: "etherscan",
    label: "Etherscan",
    hint: "Arbitrum One transaction history needs a free Etherscan key, which also loads Polygon history faster. BNB Smart Chain, Base, and OP Mainnet history need a paid Etherscan plan.",
    isStored: (settings) => settings.hasEtherscanApiKey,
    save: settingCommands.setEtherscanApiKey,
  },
  {
    id: "trongrid",
    label: "TronGrid",
    hint: "An API key allows more requests to TronGrid.",
    isStored: (settings) => settings.hasTrongridApiKey,
    save: settingCommands.setTrongridApiKey,
  },
]

export function ApiKeysView() {
  const settings = useSuspenseQuery(settingQueries.all())

  return (
    <>
      <SectionHeader level="page" title="API keys" description="These keys are optional." />

      <div className="space-y-3">
        {API_KEYS.map((apiKey) => (
          <ApiKeyForm key={apiKey.id} apiKey={apiKey} isStored={apiKey.isStored(settings.data)} />
        ))}
      </div>
    </>
  )
}
