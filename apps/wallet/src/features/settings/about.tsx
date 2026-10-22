import { useSuspenseQueries } from "@tanstack/react-query"

import { appQueries } from "@/query/app.ts"
import { SectionHeader } from "@/ui/section-header.tsx"
import { DetailList } from "@/widgets/copyable.tsx"

export function AboutView() {
  const [version, dataDir] = useSuspenseQueries({
    queries: [appQueries.version(), appQueries.dataDir()],
  })

  return (
    <>
      <SectionHeader
        level="page"
        title="About"
        description="Jelly stores your keys on this device. It supports mainnet only."
      />

      <div className="space-y-3">
        <DetailList
          items={[
            { label: "Version", value: version.data },
            { label: "Data directory", value: dataDir.data, copy: dataDir.data },
          ]}
        />

        <p className="text-caption text-fg-faint">Jelly stores wallet files and logs here.</p>
      </div>
    </>
  )
}
