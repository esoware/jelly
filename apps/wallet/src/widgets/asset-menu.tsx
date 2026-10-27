import type { ReactElement } from "react"

import type { Amount, AssetId } from "@/ipc/bindings.gen.ts"
import { assetFromKey, assetKey, assetName, groupAssets } from "@/lib/chain.ts"
import { Menu, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "@/ui/menu.tsx"
import { CryptoAmount } from "@/widgets/amounts.tsx"
import { AssetLogo } from "@/widgets/logos.tsx"

interface AssetMenuItemsProps {
  assets: readonly AssetId[]
  asset: AssetId | null
  onAssetChange: (asset: AssetId) => void
  amountFor: (asset: AssetId) => Amount | bigint | null
}

export function AssetMenuItems({ assets, asset, onAssetChange, amountFor }: AssetMenuItemsProps) {
  function pick(key: string): void {
    const next = assetFromKey(key)
    if (next !== null) {
      onAssetChange(next)
    }
  }

  return groupAssets(assets).map((group) => (
    <MenuRadioGroup
      key={group.label ?? "native"}
      label={group.label}
      value={asset === null ? "" : assetKey(asset)}
      onValueChange={pick}
    >
      {group.assets.map((option) => (
        <MenuRadioItem
          key={assetKey(option)}
          value={assetKey(option)}
          description={<CryptoAmount amount={amountFor(option)} asset={option} isCompact />}
        >
          <AssetLogo asset={option} />
          <span className="min-w-0 truncate">{assetName(option)}</span>
        </MenuRadioItem>
      ))}
    </MenuRadioGroup>
  ))
}

interface AssetMenuProps {
  assets: readonly AssetId[]
  asset: AssetId
  onAssetChange: (asset: AssetId) => void
  amountFor: (asset: AssetId) => Amount | bigint | null
  trigger: ReactElement
}

export function AssetMenu({ assets, asset, onAssetChange, amountFor, trigger }: AssetMenuProps) {
  return (
    <Menu>
      <MenuTrigger render={trigger} />
      <MenuPopup align="end" className="min-w-60">
        <AssetMenuItems
          assets={assets}
          asset={asset}
          onAssetChange={onAssetChange}
          amountFor={amountFor}
        />
      </MenuPopup>
    </Menu>
  )
}
