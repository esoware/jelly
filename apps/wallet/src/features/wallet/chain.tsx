import { useSuspenseQuery } from "@tanstack/react-query"
import { useNavigate, useParams } from "@tanstack/react-router"
import { useState } from "react"
import type { ComponentProps, ReactElement } from "react"

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
import type { AssetId, ChainId } from "@/ipc/bindings.gen.ts"
import { formatAmount } from "@/lib/amount.ts"
import {
  assetName,
  assetSymbol,
  assetsForChain,
  chainName,
  chainTicker,
  shownAsset,
} from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { positionOf } from "@/lib/portfolio.ts"
import type { Holdings } from "@/lib/portfolio.ts"
import { settingQueries } from "@/query/setting.ts"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Menu, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "@/ui/menu.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { FiatAmount } from "@/widgets/amounts.tsx"
import { AssetMenu } from "@/widgets/asset-menu.tsx"
import { AssetLogo, ChainLogo, assetBrand } from "@/widgets/logos.tsx"

const LONG_FIGURE_CHARS = 11

function SwitchRow({ className, children, ...props }: ComponentProps<"button">) {
  return (
    <button
      type="button"
      {...props}
      className={appendClassName(
        "group/switch flex min-w-0 items-center gap-1 text-left transition-colors",
        className,
      )}
    >
      {children}
      <span className="-my-px ml-auto flex size-4.5 shrink-0 items-center justify-center rounded-md text-fg-muted transition-colors group-hover/switch:bg-hover group-hover/switch:text-fg group-data-popup-open/switch:bg-active group-data-popup-open/switch:text-fg">
        <Icon name="switch" className="size-icon-sm" />
      </span>
    </button>
  )
}

interface ChainMenuProps {
  chains: readonly ChainId[]
  chain: ChainId
  onChainChange: (chain: ChainId) => void
  trigger: ReactElement
}

function ChainMenu({ chains, chain, onChainChange, trigger }: ChainMenuProps) {
  return (
    <Menu>
      <MenuTrigger render={trigger} />
      <MenuPopup align="start" className="min-w-60">
        <MenuRadioGroup label="Chains" value={chain} onValueChange={onChainChange}>
          {chains.map((option) => (
            <MenuRadioItem key={option} value={option} description={chainTicker(option)}>
              <ChainLogo chain={option} />
              <span className="min-w-0 truncate">{chainName(option)}</span>
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  )
}

interface BalancePanelProps {
  holdings: Holdings
  chains: readonly ChainId[]
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
  onChainChange: (chain: ChainId) => void
  onSend: () => void
  onReceive: () => void
}

function BalancePanel({
  holdings,
  chains,
  asset,
  onAssetChange,
  onChainChange,
  onSend,
  onReceive,
}: BalancePanelProps) {
  const accentRef = useAccentColor<HTMLElement>(assetBrand(asset).color)
  const assets = assetsForChain(holdings.chain)
  const position = positionOf(holdings, asset)
  const text = formatAmount(position.amount, position.decimals)
  const chainLine = (
    <>
      On
      <ChainLogo chain={holdings.chain} className="size-icon-sm" />
      <span className="truncate">{chainName(holdings.chain)}</span>
    </>
  )

  return (
    <HeroPanel ref={accentRef} className="transition-colors">
      <HeroPanelTop>
        <HeroLogo chain={holdings.chain}>
          <AssetLogo asset={asset} className="size-8" />
        </HeroLogo>
        <div className="flex min-w-0 flex-1 flex-col">
          {assets.length > 1 ? (
            <AssetMenu
              assets={assets}
              asset={asset}
              onAssetChange={onAssetChange}
              amountFor={(option) => positionOf(holdings, option).amount}
              align="start"
              trigger={
                <SwitchRow className="text-title">
                  <span className="truncate">{assetName(asset)}</span>
                </SwitchRow>
              }
            />
          ) : (
            <span className="truncate text-title">{assetName(asset)}</span>
          )}
          {chains.length > 1 ? (
            <ChainMenu
              chains={chains}
              chain={holdings.chain}
              onChainChange={onChainChange}
              trigger={
                <SwitchRow className="text-caption text-fg-muted hover:text-fg data-popup-open:text-fg">
                  {chainLine}
                </SwitchRow>
              }
            />
          ) : (
            <span className="flex min-w-0 items-center gap-1 text-caption text-fg-muted">
              {chainLine}
            </span>
          )}
        </div>
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
          chains={allHoldings.map((entry) => entry.chain)}
          asset={asset}
          onAssetChange={(next) => {
            saveShownAsset.mutate(next)
          }}
          onChainChange={(next) => {
            saveShownAsset.mutate(shownAsset(next, settings.data.shownTokens))
            void navigate({ to: "/wallet/$chain", params: { chain: next } })
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
