import { useSuspenseQuery } from "@tanstack/react-query"
import { Outlet, useMatch, useNavigate, useParams } from "@tanstack/react-router"
import { useState } from "react"
import type { ComponentProps, ReactNode } from "react"

import { AllocationMark } from "@/features/wallet/donut.tsx"
import type { AssetId, ChainId } from "@/ipc/bindings.gen.ts"
import { chainName, withShownAsset } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { allocate, holdingsFor } from "@/lib/portfolio.ts"
import type { Holdings } from "@/lib/portfolio.ts"
import { chainCommands, chainQueries } from "@/query/chain.ts"
import { moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys } from "@/query/setting.ts"
import { usePrices } from "@/store.ts"
import { Carousel } from "@/ui/carousel.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Panel } from "@/ui/panel.tsx"
import { SegmentedGroup, SegmentedList, SegmentedTab, SegmentedTabs } from "@/ui/segmented.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"
import { StepTransition } from "@/ui/step-transition.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { CHAIN_BRANDS, ChainLogo } from "@/widgets/logos.tsx"

const PORTFOLIO = "portfolio"

const CONTACTS = "contacts"

type WalletView = ChainId | typeof PORTFOLIO | typeof CONTACTS

function walletViews(holdings: readonly Holdings[]): WalletView[] {
  return [PORTFOLIO, CONTACTS, ...holdings.map((entry) => entry.chain)]
}

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
  const [isSpinning, setIsSpinning] = useState(false)
  const sync = useIpcMutation<ChainId[], ChainId | null>({
    mutationFn: chainCommands.syncNow,
    errorTitle: "Could not start a sync",
    successToast: (chains) => (chains.length === 0 ? "No chain to sync" : "Syncing now"),
  })

  return (
    <TooltipText
      content="Sync now"
      render={
        <button
          type="button"
          data-spinning={isSpinning}
          className="group/logo relative flex shrink-0 rounded-full"
          onClick={() => {
            if (!isSpinning) {
              setIsSpinning(true)
              sync.mutate(chain)
            }
          }}
        />
      }
    >
      {children}
      <span className="absolute inset-0 flex items-center justify-center rounded-full bg-logo-cover text-fg opacity-0 transition-opacity duration-base ease-standard group-hover/logo:opacity-100 group-focus-visible/logo:opacity-100 group-data-[spinning=true]/logo:opacity-100">
        <Icon
          name="sync"
          className="size-icon-lg group-data-[spinning=true]/logo:animate-spin"
          onAnimationIteration={() => {
            if (!sync.isPending) {
              setIsSpinning(false)
            }
          }}
        />
      </span>
    </TooltipText>
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
}

function WalletNav({ holdings, view }: WalletNavProps) {
  const navigate = useNavigate()
  const prices = usePrices()
  const allocation = allocate(holdings, prices)

  return (
    <SegmentedTabs
      value={view}
      values={walletViews(holdings)}
      onValueChange={(next) => {
        if (next === PORTFOLIO) {
          void navigate({ to: "/wallet/portfolio" })
        } else if (next === CONTACTS) {
          void navigate({ to: "/wallet/contacts" })
        } else {
          void navigate({ to: "/wallet/$chain", params: { chain: next } })
        }
      }}
    >
      <SegmentedList className="flex min-w-0 shrink-0 items-center gap-3">
        <SegmentedGroup variant="accent">
          <SegmentedTab
            item={{
              value: PORTFOLIO,
              label: "Portfolio",
              icon: <AllocationMark allocation={allocation} />,
            }}
          />
          <SegmentedTab
            item={{
              value: CONTACTS,
              label: "Address book",
              icon: <Icon name="contacts" className="size-icon-lg" />,
            }}
          />
        </SegmentedGroup>
        <Carousel
          activeKey={view}
          className="flex-1 px-2"
          frame={<SegmentedGroup variant="accent" />}
        >
          {holdings.map(({ chain }) => (
            <SegmentedTab
              key={chain}
              item={{
                value: chain,
                label: chainName(chain),
                icon: <ChainLogo chain={chain} />,
              }}
            />
          ))}
        </Carousel>
      </SegmentedList>
    </SegmentedTabs>
  )
}

export function WalletLayout() {
  const { chain } = useParams({ strict: false })
  const contactsMatch = useMatch({ from: "/_unlocked/wallet/contacts", shouldThrow: false })
  const view: WalletView = chain ?? (contactsMatch === undefined ? PORTFOLIO : CONTACTS)
  const holdings = useHoldings()
  const accentRef = useAccentColor<HTMLDivElement>(
    chain === undefined ? null : CHAIN_BRANDS[chain].color,
  )

  return (
    <div
      ref={accentRef}
      className="flex h-full flex-col gap-3 px-3 pt-1 pb-3 [transition:--color-accent_var(--transition-duration-base)_var(--ease-enter)]"
    >
      <WalletNav holdings={holdings} view={view} />
      <StepTransition
        step={walletViews(holdings).indexOf(view)}
        className="flex min-h-0 flex-1 gap-3"
      >
        <Outlet />
      </StepTransition>
    </div>
  )
}

export function WalletLayoutFallback() {
  return (
    <div className="flex flex-col gap-3 px-3 pt-1 pb-3">
      <Skeleton className="h-11 w-64" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
    </div>
  )
}
