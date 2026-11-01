import { Outlet, useLocation, useNavigate } from "@tanstack/react-router"

import { Segmented } from "@/ui/segmented.tsx"

const SECTIONS = [
  { value: "/settings/chains", label: "Chains" },
  { value: "/settings/network", label: "Network" },
  { value: "/settings/currency", label: "Currency" },
  { value: "/settings/api-keys", label: "API keys" },
  { value: "/settings/security", label: "Security" },
  { value: "/settings/wallets", label: "Wallets" },
  { value: "/settings/appearance", label: "Appearance" },
  { value: "/settings/about", label: "About" },
] as const

export function SettingsLayout() {
  const navigate = useNavigate()
  const pathname = useLocation({ select: (location) => location.pathname })
  const selected =
    SECTIONS.find((section) => section.value === pathname)?.value ?? SECTIONS[0].value

  return (
    <div className="flex h-full flex-col">
      <nav className="shrink-0 px-4 pt-3 pb-2">
        <Segmented
          value={selected}
          items={SECTIONS}
          className="mx-auto w-fit max-w-full flex-wrap justify-center"
          onValueChange={(to) => {
            void navigate({ to, replace: true })
          }}
        />
      </nav>

      <main className="min-h-0 flex-1 scroll-y-4">
        <div className="mx-auto flex w-full max-w-page flex-col gap-4 pt-5 pb-4">
          <Outlet />
        </div>
      </main>
    </div>
  )
}
