import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query"
import { useParams } from "@tanstack/react-router"
import { useEffect, useRef, useState } from "react"

import { ActivityPanel } from "@/features/wallet/activity.tsx"
import { AddressesPanel } from "@/features/wallet/addresses.tsx"
import {
  HeroFigure,
  HeroLogo,
  HeroPanel,
  HeroPanelFigures,
  HeroPanelTop,
  useHoldings,
  useSetShownAsset,
} from "@/features/wallet/layout.tsx"
import { CHART_RANGES } from "@/features/wallet/price-scale.ts"
import type { ChartRange } from "@/features/wallet/price-scale.ts"
import { PricePanel } from "@/features/wallet/price.tsx"
import { ReceiveDialog } from "@/features/wallet/receive.tsx"
import { SendDialog } from "@/features/wallet/send.tsx"
import type { AssetId } from "@/ipc/bindings.gen.ts"
import { formatAmount } from "@/lib/amount.ts"
import { assetName, assetSymbol, assetsForChain, chainName, shownAsset } from "@/lib/chain.ts"
import { positionOf } from "@/lib/portfolio.ts"
import type { Holdings } from "@/lib/portfolio.ts"
import { PRICE_CHART_REFRESH_MS, moneyQueries } from "@/query/money.ts"
import { settingQueries } from "@/query/setting.ts"
import { useFiatCurrency } from "@/store.ts"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { FiatAmount } from "@/widgets/amounts.tsx"
import { AssetMenu } from "@/widgets/asset-menu.tsx"
import { AssetLogo, ChainLogo, assetBrand } from "@/widgets/logos.tsx"

const LONG_FIGURE_CHARS = 11
const CHART_PREFETCH_CONCURRENCY = 4

interface BalancePanelProps {
  holdings: Holdings
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
  onAssetMenuOpenChange: (isOpen: boolean) => void
  onSend: () => void
  onReceive: () => void
}

function BalancePanel({
  holdings,
  asset,
  onAssetChange,
  onAssetMenuOpenChange,
  onSend,
  onReceive,
}: BalancePanelProps) {
  const accentRef = useAccentColor<HTMLElement>(assetBrand(asset).color)
  const assets = assetsForChain(holdings.chain)
  const position = positionOf(holdings, asset)
  const text = formatAmount(position.amount, position.decimals)

  return (
    <HeroPanel ref={accentRef} className="transition-colors">
      <HeroPanelTop>
        <HeroLogo chain={holdings.chain}>
          <AssetLogo asset={asset} className="size-8" />
        </HeroLogo>
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-title">{assetName(asset)}</span>
          <span className="flex min-w-0 items-center gap-1 text-caption text-fg-muted">
            On
            <ChainLogo chain={holdings.chain} className="size-icon-sm" />
            <span className="truncate">{chainName(holdings.chain)}</span>
          </span>
        </div>
        {assets.length > 1 ? (
          <AssetMenu
            assets={assets}
            asset={asset}
            onAssetChange={onAssetChange}
            onOpenChange={onAssetMenuOpenChange}
            amountFor={(option) => positionOf(holdings, option).amount}
            trigger={
              <Button variant="ghost" size="icon-sm">
                <Icon name="switch" />
              </Button>
            }
          />
        ) : null}
      </HeroPanelTop>

      <HeroPanelFigures>
        <p className="flex min-w-0 items-baseline gap-2">
          <HeroFigure isLong={text.length > LONG_FIGURE_CHARS}>{text}</HeroFigure>
          <span className="text-body text-fg-muted">{assetSymbol(asset)}</span>
        </p>
        <FiatAmount amount={position.amount} asset={asset} className="text-body text-fg-muted" />
      </HeroPanelFigures>

      <div className="flex gap-2">
        <Button variant="secondary" className="flex-1" onClick={onReceive}>
          <Icon name="receive" />
          Receive
        </Button>
        <Button className="flex-1" onClick={onSend}>
          <Icon name="send" />
          Send
        </Button>
      </div>
    </HeroPanel>
  )
}

type Action = "send" | "receive" | null

export function ChainView() {
  const { chain } = useParams({ from: "/_unlocked/wallet/$chain" })
  const holdings = useHoldings().find((entry) => entry.chain === chain)
  const settings = useSuspenseQuery(settingQueries.all())
  const client = useQueryClient()
  const currency = useFiatCurrency()
  const [action, setAction] = useState<Action>(null)
  const [range, setRange] = useState<ChartRange>("1d")
  const prefetchQueue = useRef<(() => Promise<unknown>)[]>([])
  const prefetchWorkers = useRef(0)

  useEffect(
    () => () => {
      prefetchQueue.current = []
    },
    [chain],
  )

  const saveShownAsset = useSetShownAsset()

  if (holdings === undefined) {
    return null
  }

  const asset = shownAsset(chain, settings.data.shownTokens)
  const isChartShown = settings.data.isPriceFeedEnabled && settings.data.isPriceChartEnabled

  async function drainPrefetch(): Promise<void> {
    const next = prefetchQueue.current.shift()
    if (next === undefined) {
      prefetchWorkers.current -= 1
      return
    }
    try {
      await next()
    } catch {}
    await drainPrefetch()
  }

  function prefetchCharts(isMenuOpen: boolean): void {
    if (!isMenuOpen || !isChartShown) {
      return
    }
    const { interval } = CHART_RANGES[range]
    prefetchQueue.current = assetsForChain(chain).map(
      (option) => () =>
        client.query({
          ...moneyQueries.priceChart(option, interval, currency),
          gcTime: PRICE_CHART_REFRESH_MS,
          retry: false,
        }),
    )
    while (prefetchWorkers.current < CHART_PREFETCH_CONCURRENCY) {
      prefetchWorkers.current += 1
      void drainPrefetch()
    }
  }

  function openReceive(): void {
    setAction("receive")
  }

  function handleOpenChange(isOpen: boolean): void {
    if (!isOpen) {
      setAction(null)
    }
  }

  return (
    <>
      <div className="flex w-72 shrink-0 flex-col gap-3">
        <BalancePanel
          holdings={holdings}
          asset={asset}
          onAssetChange={(next) => {
            saveShownAsset.mutate(next)
          }}
          onAssetMenuOpenChange={prefetchCharts}
          onSend={() => {
            setAction("send")
          }}
          onReceive={openReceive}
        />
        <AddressesPanel chain={chain} asset={asset} />
      </div>
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <ActivityPanel chain={chain} onReceive={openReceive} />
        {isChartShown ? <PricePanel asset={asset} range={range} onRangeChange={setRange} /> : null}
      </div>

      <SendDialog
        chain={chain}
        asset={asset}
        open={action === "send"}
        onOpenChange={handleOpenChange}
      />
      <ReceiveDialog
        chain={chain}
        asset={asset}
        open={action === "receive"}
        onOpenChange={handleOpenChange}
      />
    </>
  )
}
