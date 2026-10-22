import { useSuspenseQuery } from "@tanstack/react-query"

import type { BackdropSetting } from "@/ipc/bindings.gen.ts"
import { appQueries } from "@/query/app.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { Field } from "@/ui/field.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Switch } from "@/ui/switch.tsx"

export function AppearanceView() {
  const settings = useSuspenseQuery(settingQueries.all())
  const windowBackdrop = useSuspenseQuery(appQueries.windowBackdrop())

  const saveBackdrop = useIpcMutation<null, BackdropSetting>({
    mutationFn: settingCommands.setBackdrop,
    invalidates: [settingKeys.root],
    optimistic: (client, backdrop) => {
      patchSettings(client, { backdrop })
    },
    errorTitle: "Could not change the window background",
  })

  return (
    <>
      <SectionHeader level="page" title="Appearance" description="Choose the window background." />

      {windowBackdrop.data === "composed" ? (
        <Field label="Window background" hint="Shows a blur of whatever is behind the window.">
          <label className="flex items-center gap-2 text-body">
            <Switch
              checked={settings.data.backdrop === "blur"}
              onCheckedChange={(isBlurred) => {
                saveBackdrop.mutate(isBlurred ? "blur" : "solid")
              }}
            />
            Blur behind the window
          </label>
        </Field>
      ) : (
        <p className="text-fg-muted">Window blur is not available on this device.</p>
      )}
    </>
  )
}
