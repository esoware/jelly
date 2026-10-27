import { useSuspenseQuery } from "@tanstack/react-query"
import { Outlet, useMatches, useNavigate, useParams } from "@tanstack/react-router"
import { useState } from "react"
import type { ComponentProps, ReactNode } from "react"

import { AllocationMark } from "@/features/wallet/donut.tsx"
import type { AssetId, ChainId } from "@/ipc/bindings.gen.ts"
import { assetChain, shownAsset, withShownAsset } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { allocate, holdingsFor } from "@/lib/portfolio.ts"
import type { Holdings } from "@/lib/portfolio.ts"
import { chainCommands, chainQueries } from "@/query/chain.ts"
import { moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { usePrices } from "@/store.ts"
import { Icon } from "@/ui/icon.tsx"
import { Panel } from "@/ui/panel.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { StepTransition } from "@/ui/step-transition.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { AssetLogo, assetBrand } from "@/widgets/logos.tsx"

const WALLET_VIEWS = ["portfolio", "wallet", "contacts", "settings"] as const

export type WalletView = (typeof WALLET_VIEWS)[number]

export function useHoldings(): readonly Holdings[] {
  const chains = useSuspenseQuery(chainQueries.enabled())
  const totals = useSuspenseQuery(moneyQueries.totals())
  return chains.data.map((chain) => holdingsFor(chain, totals.data))
}

export function useSetShownAsset() {
  return useIpcMutation<null, AssetId>({
    mutationFn: settingCommands.setShownAsset,
    invalidates: [settingKeys.root],
    optimistic: (client, next) => {
      patchSettings(client, (settings) => ({
        shownChain: assetChain(next),
        shownTokens: withShownAsset(settings.shownTokens, next),
      }))
    },
    errorTitle: "Could not switch the asset",
  })
}

export function HeroPanel({ className, ...props }: ComponentProps<"section">) {
  return (
    <Panel
      className={appendClassName("shrink-0 gap-4 bg-accent-surface p-4", className)}
      {...props}
    />
  )
}

interface HeroLogoProps {
  chain: ChainId | null
  children: ReactNode
}

export function HeroLogo({ chain, children }: HeroLogoProps) {
  const [isSpent, setIsSpent] = useState(false)
  const sync = useIpcMutation<ChainId[], ChainId | null>({
    mutationFn: chainCommands.syncNow,
    errorTitle: "Could not start a sync",
    successToast: (chains) => (chains.length === 0 ? "No chain to sync" : "Syncing now"),
  })

  return (
    <button
      type="button"
      data-spent={isSpent}
      className="group relative flex shrink-0 rounded-full"
      onClick={() => {
        if (!isSpent) {
          setIsSpent(true)
          sync.mutate(chain)
        }
      }}
      onPointerLeave={() => {
        setIsSpent(false)
      }}
      onBlur={() => {
        setIsSpent(false)
      }}
    >
      {children}
      <span className="absolute inset-0 flex items-center justify-center rounded-full bg-logo-cover text-fg opacity-0 transition-opacity duration-base ease-standard group-data-[spent=false]:group-hover:opacity-100">
        <Icon
          name="sync"
          className="size-icon-lg transition-[rotate] duration-base ease-enter group-data-[spent=true]:rotate-180"
        />
      </span>
    </button>
  )
}

interface HeroPanelTopProps {
  children: ReactNode
}

export function HeroPanelTop({ children }: HeroPanelTopProps) {
  return <div className="flex h-9 items-center gap-3">{children}</div>
}

interface HeroPanelFiguresProps {
  children: ReactNode
}

export function HeroPanelFigures({ children }: HeroPanelFiguresProps) {
  return <div className="flex flex-col gap-1">{children}</div>
}

interface HeroFigureProps {
  isLong: boolean
  children: ReactNode
}

export function HeroFigure({ isLong, children }: HeroFigureProps) {
  return (
    <span data-long={isLong} className="text-display tabular-nums data-[long=true]:text-headline">
      {children}
    </span>
  )
}

interface WalletNavProps {
  holdings: readonly Holdings[]
  view: WalletView
  walletAsset: AssetId | null
}

function WalletNav({ holdings, view, walletAsset }: WalletNavProps) {
  const navigate = useNavigate()
  const prices = usePrices()
  const allocation = allocate(holdings, prices)

  return (
    <Segmented
      variant="accent"
      value={view}
      items={[
        {
          value: "portfolio",
          label: "Portfolio",
          icon: <AllocationMark allocation={allocation} />,
        },
        {
          value: "wallet",
          label: "Wallet",
          icon: walletAsset === null ? undefined : <AssetLogo asset={walletAsset} />,
        },
        {
          value: "contacts",
          label: "Address book",
          icon: <Icon name="contacts" className="size-icon-lg" />,
        },
      ]}
      endItems={[
        {
          value: "settings",
          label: "Settings",
          icon: <Icon name="settings" className="size-icon-lg" />,
        },
      ]}
      onValueChange={(next) => {
        if (next === "portfolio") {
          void navigate({ to: "/wallet/portfolio" })
        } else if (next === "contacts") {
          void navigate({ to: "/wallet/contacts" })
        } else if (next === "settings") {
          void navigate({ to: "/wallet/settings" })
        } else if (walletAsset === null) {
          void navigate({ to: "/wallet/settings/chains" })
        } else {
          void navigate({ to: "/wallet/$chain", params: { chain: assetChain(walletAsset) } })
        }
      }}
    />
  )
}

export function WalletLayout() {
  const { chain } = useParams({ strict: false })
  const view = useMatches({
    select: (matches) =>
      matches.findLast((match) => match.staticData.walletView !== undefined)?.staticData
        .walletView ?? "portfolio",
  })
  const holdings = useHoldings()
  const { shownChain, shownTokens } = useSuspenseQuery(settingQueries.all()).data
  const walletChain =
    chain ??
    holdings.find((entry) => entry.chain === shownChain)?.chain ??
    holdings.at(0)?.chain ??
    null
  const walletAsset = walletChain === null ? null : shownAsset(walletChain, shownTokens)
  const accentRef = useAccentColor<HTMLDivElement>(
    chain === undefined || walletAsset === null ? null : assetBrand(walletAsset).color,
  )

  return (
    <div
      ref={accentRef}
      className="flex h-full flex-col gap-3 px-3 pt-1 pb-3 [transition:--color-accent_var(--transition-duration-base)_var(--ease-enter)]"
    >
      <WalletNav holdings={holdings} view={view} walletAsset={walletAsset} />
      <StepTransition step={WALLET_VIEWS.indexOf(view)} className="flex min-h-0 flex-1 gap-3">
        <Outlet />
      </StepTransition>
    </div>
  )
}

export function WalletLayoutFallback() {
  return (
    <div className="flex flex-col gap-3 px-3 pt-1 pb-3">
      <Skeleton className="h-10 w-full rounded-full" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
    </div>
  )
}
