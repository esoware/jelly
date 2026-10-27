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
import type { AssetId, ChainId, FiatCurrency, TokenId, WalletRecord } from "@/ipc/bindings.gen.ts"
import { assetChain, chainMeta } from "@/lib/chain.ts"
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

const CHAINLINK: Brand = { color: "#2a5ada", logo: chainlinkLogo }
const WRAPPED_ETHER: Brand = { color: "#ec1c79", logo: wrappedEtherLogo }
const TETHER: Brand = { color: "#009393", logo: tetherUsdLogo }
const USD_COIN: Brand = { color: "#0b53bf", logo: usdCoinLogo }
const USDS: Brand = { color: "#ff9b50", logo: usdsLogo }
const GLOBAL_DOLLAR: Brand = { color: "#c7e36c", logo: globalDollarLogo }
const PAYPAL_USD: Brand = { color: "#0079c1", logo: paypalUsdLogo }
const TETHER_GOLD: Brand = { color: "#d3b25a", logo: tetherGoldLogo }
const PAX_GOLD: Brand = { color: "#dbba08", logo: paxGoldLogo }

const TOKEN_BRANDS: Record<TokenId, Brand> = {
  link_erc20: CHAINLINK,
  wbtc_erc20: { color: "#f09242", logo: wrappedBitcoinLogo },
  weth_erc20: WRAPPED_ETHER,
  uni_erc20: { color: "#ff007a", logo: uniswapLogo },
  shib_erc20: { color: "#ffa409", logo: shibaInuLogo },
  ena_erc20: { color: "#8c919c", logo: ethenaLogo, gradient: ["#626874", "#c1c5cd"] },
  aave_erc20: { color: "#9391f7", logo: aaveLogo },
  ondo_erc20: { color: "#4d78a8", logo: ondoLogo, gradient: ["#345477", "#8cafd4"] },
  pepe_erc20: { color: "#3d8130", logo: pepeLogo },
  arb_erc20: ARBITRUM,
  usdt_erc20: TETHER,
  usdc_erc20: USD_COIN,
  usds_erc20: USDS,
  dai_erc20: { color: "#f5ac37", logo: daiLogo },
  usdg_erc20: GLOBAL_DOLLAR,
  pyusd_erc20: PAYPAL_USD,
  xaut_erc20: TETHER_GOLD,
  paxg_erc20: PAX_GOLD,
  link_arbitrum: CHAINLINK,
  weth_arbitrum: WRAPPED_ETHER,
  arb_arbitrum: ARBITRUM,
  usdt_arbitrum: TETHER,
  usdc_arbitrum: USD_COIN,
  usds_arbitrum: USDS,
  usdg_arbitrum: GLOBAL_DOLLAR,
  pyusd_arbitrum: PAYPAL_USD,
  xaut_arbitrum: TETHER_GOLD,
  link_base: CHAINLINK,
  weth_base: WRAPPED_ETHER,
  usdc_base: USD_COIN,
  usds_base: USDS,
  link_optimism: CHAINLINK,
  weth_optimism: WRAPPED_ETHER,
  op_optimism: OPTIMISM,
  usdt_optimism: TETHER,
  usdc_optimism: USD_COIN,
  usds_optimism: USDS,
  link_bep20: CHAINLINK,
  weth_bep20: WRAPPED_ETHER,
  usdt_bep20: TETHER,
  usdc_bep20: USD_COIN,
  xaut_bep20: TETHER_GOLD,
  link_polygon: CHAINLINK,
  weth_polygon: WRAPPED_ETHER,
  usdt_polygon: TETHER,
  usdc_polygon: USD_COIN,
  pyusd_polygon: PAYPAL_USD,
  xaut_polygon: TETHER_GOLD,
  link_avalanche: CHAINLINK,
  weth_avalanche: WRAPPED_ETHER,
  usdt_avalanche: TETHER,
  usdc_avalanche: USD_COIN,
  usds_avalanche: USDS,
  xaut_avalanche: TETHER_GOLD,
  link_spl: CHAINLINK,
  pump_spl: { color: "#54d38e", logo: pumpFunLogo },
  jup_spl: { color: "#c6f462", logo: jupiterLogo, gradient: ["#33d9ff", "#c6f462"] },
  usdt_spl: TETHER,
  usdc_spl: USD_COIN,
  usds_spl: USDS,
  usdg_spl: GLOBAL_DOLLAR,
  pyusd_spl: PAYPAL_USD,
  paxg_spl: PAX_GOLD,
  usdt_trc20: TETHER,
  usdt_jetton: TETHER,
  xaut_jetton: TETHER_GOLD,
}

export function assetBrand(asset: AssetId): Brand {
  return asset.kind === "native"
    ? CHAIN_BRANDS[chainMeta(asset.value).coinChain]
    : TOKEN_BRANDS[asset.value]
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
  if (!shouldShowChain || brand === CHAIN_BRANDS[chain]) {
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
