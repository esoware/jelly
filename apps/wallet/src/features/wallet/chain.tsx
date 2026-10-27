import { useSuspenseQuery } from "@tanstack/react-query"
import { useNavigate, useParams } from "@tanstack/react-router"
import { useState } from "react"

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
import { PricePanel } from "@/features/wallet/price.tsx"
import { ReceiveDialog } from "@/features/wallet/receive.tsx"
import { SendDialog } from "@/features/wallet/send.tsx"
import type { AssetId } from "@/ipc/bindings.gen.ts"
import { formatAmount } from "@/lib/amount.ts"
import {
  assetChain,
  assetName,
  assetSymbol,
  assetsForChain,
  chainName,
  nativeAsset,
  shownAsset,
} from "@/lib/chain.ts"
import { positionOf } from "@/lib/portfolio.ts"
import type { Holdings } from "@/lib/portfolio.ts"
import { settingQueries } from "@/query/setting.ts"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import {
  Menu,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSubmenu,
  MenuTrigger,
} from "@/ui/menu.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { CryptoAmount, FiatAmount } from "@/widgets/amounts.tsx"
import { AssetMenuItems } from "@/widgets/asset-menu.tsx"
import { AssetLogo, ChainLogo, assetBrand } from "@/widgets/logos.tsx"

const LONG_FIGURE_CHARS = 11

interface CoinMenuProps {
  allHoldings: readonly Holdings[]
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
}

function CoinMenu({ allHoldings, asset, onAssetChange }: CoinMenuProps) {
  const chain = assetChain(asset)

  return (
    <Menu>
      <MenuTrigger
        render={
          <Button variant="ghost" size="icon-sm">
            <Icon name="switch" />
          </Button>
        }
      />
      <MenuPopup align="end" className="min-w-72">
        <MenuRadioGroup
          label="Chains"
          value={chain}
          onValueChange={(next) => {
            onAssetChange(nativeAsset(next))
          }}
        >
          {allHoldings.map((entry) => {
            const assets = assetsForChain(entry.chain)
            const coin = nativeAsset(entry.chain)
            const balance = (
              <CryptoAmount amount={positionOf(entry, coin).amount} asset={coin} isCompact />
            )
            const label = (
              <>
                <ChainLogo chain={entry.chain} />
                <span className="min-w-0 truncate">{chainName(entry.chain)}</span>
              </>
            )
            return assets.length > 1 ? (
              <MenuSubmenu
                key={entry.chain}
                label={label}
                description={balance}
                isChecked={entry.chain === chain}
                className="min-w-60"
              >
                <AssetMenuItems
                  assets={assets}
                  asset={entry.chain === chain ? asset : null}
                  onAssetChange={onAssetChange}
                  amountFor={(option) => positionOf(entry, option).amount}
                />
              </MenuSubmenu>
            ) : (
              <MenuRadioItem key={entry.chain} value={entry.chain} description={balance}>
                {label}
              </MenuRadioItem>
            )
          })}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  )
}

interface BalancePanelProps {
  holdings: Holdings
  allHoldings: readonly Holdings[]
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
  onSend: () => void
  onReceive: () => void
}

function BalancePanel({
  holdings,
  allHoldings,
  asset,
  onAssetChange,
  onSend,
  onReceive,
}: BalancePanelProps) {
  const accentRef = useAccentColor<HTMLElement>(assetBrand(asset).color)
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
        {allHoldings.length > 1 || assetsForChain(holdings.chain).length > 1 ? (
          <CoinMenu allHoldings={allHoldings} asset={asset} onAssetChange={onAssetChange} />
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
  const navigate = useNavigate()
  const allHoldings = useHoldings()
  const holdings = allHoldings.find((entry) => entry.chain === chain)
  const settings = useSuspenseQuery(settingQueries.all())
  const [action, setAction] = useState<Action>(null)

  const saveShownAsset = useSetShownAsset()

  if (holdings === undefined) {
    return null
  }

  const asset = shownAsset(chain, settings.data.shownTokens)

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
          allHoldings={allHoldings}
          asset={asset}
          onAssetChange={(next) => {
            saveShownAsset.mutate(next)
            const nextChain = assetChain(next)
            if (nextChain !== chain) {
              void navigate({ to: "/wallet/$chain", params: { chain: nextChain } })
            }
          }}
          onSend={() => {
            setAction("send")
          }}
          onReceive={openReceive}
        />
        <AddressesPanel key={chain} chain={chain} asset={asset} />
      </div>
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <ActivityPanel key={chain} chain={chain} onReceive={openReceive} />
        {settings.data.isPriceFeedEnabled && settings.data.isPriceChartEnabled ? (
          <PricePanel asset={asset} />
        ) : null}
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
