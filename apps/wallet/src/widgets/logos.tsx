import { AU, CA, CH, EU, GB, JP, US } from "country-flag-icons/react/1x1"

import aaveLogo from "@/assets/brands/aave.svg"
import arbitrumLogo from "@/assets/brands/arbitrum.svg"
import avalancheLogo from "@/assets/brands/avalanche.svg"
import baseLogo from "@/assets/brands/base.svg"
import bitcoinCashLogo from "@/assets/brands/bitcoin-cash.svg"
import bitcoinLogo from "@/assets/brands/bitcoin.svg"
import bnbSmartChainLogo from "@/assets/brands/bnb-smart-chain.svg"
import chainlinkLogo from "@/assets/brands/chainlink.svg"
import daiLogo from "@/assets/brands/dai.svg"
import dogecoinLogo from "@/assets/brands/dogecoin.svg"
import ethenaLogo from "@/assets/brands/ethena.svg"
import ethereumLogo from "@/assets/brands/ethereum.svg"
import globalDollarLogo from "@/assets/brands/global-dollar.svg"
import gramLogo from "@/assets/brands/gram.svg"
import jupiterLogo from "@/assets/brands/jupiter.svg"
import litecoinLogo from "@/assets/brands/litecoin.svg"
import moneroLogo from "@/assets/brands/monero.svg"
import ondoLogo from "@/assets/brands/ondo.svg"
import optimismLogo from "@/assets/brands/optimism.svg"
import paxGoldLogo from "@/assets/brands/pax-gold.svg"
import paypalUsdLogo from "@/assets/brands/paypal-usd.svg"
import pepeLogo from "@/assets/brands/pepe.svg"
import polygonLogo from "@/assets/brands/polygon.svg"
import pumpFunLogo from "@/assets/brands/pump-fun.svg"
import shibaInuLogo from "@/assets/brands/shiba-inu.svg"
import solanaLogo from "@/assets/brands/solana.svg"
import tetherGoldLogo from "@/assets/brands/tether-gold.svg"
import tetherUsdLogo from "@/assets/brands/tether-usd.svg"
import tronLogo from "@/assets/brands/tron.svg"
import uniswapLogo from "@/assets/brands/uniswap.svg"
import usdCoinLogo from "@/assets/brands/usd-coin.svg"
import usdsLogo from "@/assets/brands/usds.svg"
import wrappedBitcoinLogo from "@/assets/brands/wrapped-bitcoin.svg"
import wrappedEtherLogo from "@/assets/brands/wrapped-ether.svg"
import xrpLogo from "@/assets/brands/xrp.svg"
import zcashLogo from "@/assets/brands/zcash.svg"
import type { AssetId, ChainId, Coin, FiatCurrency, WalletRecord } from "@/ipc/bindings.gen.ts"
import { assetChain, assetCoin, chainMeta } from "@/lib/chain.ts"
import { appendClassName } from "@/lib/class-name.ts"
import { useAccentColor } from "@/ui/use-accent-color.ts"

interface Brand {
  readonly color: string
  readonly logo: string
  readonly gradient?: readonly [string, string] | undefined
}

const ARBITRUM: Brand = { color: "#1b4add", logo: arbitrumLogo }
const OPTIMISM: Brand = { color: "#ff0421", logo: optimismLogo }

export const CHAIN_BRANDS: Record<ChainId, Brand> = {
  bitcoin: { color: "#f7931a", logo: bitcoinLogo },
  litecoin: { color: "#345d9d", logo: litecoinLogo },
  dogecoin: { color: "#c2a633", logo: dogecoinLogo },
  bitcoin_cash: { color: "#0ac18e", logo: bitcoinCashLogo },
  zcash: { color: "#f4b728", logo: zcashLogo },
  ethereum: { color: "#627eea", logo: ethereumLogo },
  arbitrum: ARBITRUM,
  base: { color: "#0000ff", logo: baseLogo },
  optimism: OPTIMISM,
  bsc: { color: "#f0b90b", logo: bnbSmartChainLogo },
  polygon: { color: "#670de5", logo: polygonLogo },
  avalanche: { color: "#e6212f", logo: avalancheLogo },
  solana: { color: "#9945ff", logo: solanaLogo, gradient: ["#9945ff", "#19fb9b"] },
  tron: { color: "#ea0029", logo: tronLogo },
  xrp: { color: "#141414", logo: xrpLogo },
  ton: { color: "#30a1f5", logo: gramLogo },
  monero: { color: "#f26822", logo: moneroLogo },
}

const COIN_BRANDS: Record<Coin, Brand> = {
  link: { color: "#2a5ada", logo: chainlinkLogo },
  wbtc: { color: "#f09242", logo: wrappedBitcoinLogo },
  weth: { color: "#ec1c79", logo: wrappedEtherLogo },
  uni: { color: "#ff007a", logo: uniswapLogo },
  shib: { color: "#ffa409", logo: shibaInuLogo },
  ena: { color: "#8c919c", logo: ethenaLogo, gradient: ["#626874", "#c1c5cd"] },
  aave: { color: "#9391f7", logo: aaveLogo },
  ondo: { color: "#4d78a8", logo: ondoLogo, gradient: ["#345477", "#8cafd4"] },
  pepe: { color: "#3d8130", logo: pepeLogo },
  arb: ARBITRUM,
  op: OPTIMISM,
  pump: { color: "#54d38e", logo: pumpFunLogo },
  jup: { color: "#c6f462", logo: jupiterLogo, gradient: ["#33d9ff", "#c6f462"] },
  usdt: { color: "#009393", logo: tetherUsdLogo },
  usdc: { color: "#0b53bf", logo: usdCoinLogo },
  usds: { color: "#ff9b50", logo: usdsLogo },
  dai: { color: "#f5ac37", logo: daiLogo },
  usdg: { color: "#c7e36c", logo: globalDollarLogo },
  pyusd: { color: "#0079c1", logo: paypalUsdLogo },
  xaut: { color: "#d3b25a", logo: tetherGoldLogo },
  paxg: { color: "#dbba08", logo: paxGoldLogo },
}

export function assetBrand(asset: AssetId): Brand {
  const coin = assetCoin(asset)
  return coin === null ? CHAIN_BRANDS[chainMeta(assetChain(asset)).coinChain] : COIN_BRANDS[coin]
}

interface BrandLogoProps {
  brand: Brand
  className?: string | undefined
}

function BrandLogo({ brand, className }: BrandLogoProps) {
  return (
    <img src={brand.logo} alt="" className={appendClassName("size-icon-lg shrink-0", className)} />
  )
}

interface ChainLogoProps {
  chain: ChainId
  className?: string | undefined
}

export function ChainLogo({ chain, className }: ChainLogoProps) {
  return <BrandLogo brand={CHAIN_BRANDS[chain]} className={className} />
}

interface AssetLogoProps {
  asset: AssetId
  shouldShowChain?: boolean | undefined
  className?: string | undefined
}

export function AssetLogo({ asset, shouldShowChain = false, className }: AssetLogoProps) {
  const brand = assetBrand(asset)
  const chain = assetChain(asset)
  if (!shouldShowChain || (asset.kind === "native" && chainMeta(chain).coinChain === chain)) {
    return <BrandLogo brand={brand} className={className} />
  }

  return (
    <span className={appendClassName("relative inline-flex size-icon-lg shrink-0", className)}>
      <BrandLogo brand={brand} className="size-full" />
      <ChainLogo chain={chain} className="absolute -right-1/8 -bottom-1/8 size-1/2" />
    </span>
  )
}

const FLAGS: Record<FiatCurrency, typeof US> = {
  usd: US,
  eur: EU,
  gbp: GB,
  aud: AU,
  cad: CA,
  jpy: JP,
  chf: CH,
}

interface CurrencyFlagProps {
  currency: FiatCurrency
}

export function CurrencyFlag({ currency }: CurrencyFlagProps) {
  const Flag = FLAGS[currency]
  return <Flag className="size-icon-lg shrink-0 overflow-hidden rounded-full" />
}

export const DEFAULT_WALLET_COLOR = "#71717a"

export function walletBadges(wallet: WalletRecord): string[] | undefined {
  return wallet.passphrasePolicy === "prompt" ? ["Passphrase"] : undefined
}

interface WalletAvatarProps {
  color?: string | undefined
}

export function WalletAvatar({ color = DEFAULT_WALLET_COLOR }: WalletAvatarProps) {
  const root = useAccentColor<HTMLSpanElement>(color)

  return (
    <span
      ref={root}
      className="inline-flex size-icon-lg shrink-0 overflow-hidden rounded-full bg-[oklch(from_var(--color-accent)_clamp(0.5,l,0.72)_min(c,0.16)_h)] bg-linear-to-t from-avatar-shade to-transparent"
    >
      <svg viewBox="0 0 32 32" className="size-full fill-avatar-glyph">
        <circle cx="16" cy="12.5" r="5.5" />
        <path d="M6.2 28.6C7.8 23.4 11.6 20.4 16 20.4s8.2 3 9.8 8.2A16 16 0 0 1 6.2 28.6Z" />
      </svg>
    </span>
  )
}
