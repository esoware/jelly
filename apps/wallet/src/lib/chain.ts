import type {
  AddressRecord,
  Amount,
  AssetId,
  AssetMeta,
  ChainId,
  ChainMeta,
  Coin,
  EndpointOrigin,
  KeySource,
  ProbeFailure,
  ProbeReport,
  ScriptKind,
  TokenId,
  Transport,
} from "@/ipc/bindings.gen.ts"
import { formatAmount } from "@/lib/amount.ts"

let chainsById: ReadonlyMap<ChainId, ChainMeta> = new Map()

let assetsByKey: ReadonlyMap<string, AssetMeta> = new Map()

export function assetKey(asset: AssetId): string {
  return `${asset.kind}:${asset.value}`
}

export function installCatalog(chains: readonly ChainMeta[], assets: readonly AssetMeta[]): void {
  chainsById = new Map(chains.map((entry) => [entry.chain, entry]))
  assetsByKey = new Map(assets.map((entry) => [assetKey(entry.asset), entry]))
}

export function allChains(): readonly ChainId[] {
  return [...chainsById.keys()]
}

function allAssets(): readonly AssetMeta[] {
  return [...assetsByKey.values()]
}

export function chainMeta(chain: ChainId): ChainMeta {
  const entry = chainsById.get(chain)
  if (entry === undefined) {
    throw new Error(`The catalog has no chain ${chain}`)
  }
  return entry
}

function assetMeta(asset: AssetId): AssetMeta {
  const entry = assetsByKey.get(assetKey(asset))
  if (entry === undefined) {
    throw new Error(`The catalog has no asset ${assetKey(asset)}`)
  }
  return entry
}

export function assetFromKey(key: string): AssetId | null {
  return assetsByKey.get(key)?.asset ?? null
}

const MAX_EXPLORER_TXID_CHARS = 128

export function isChainId(value: string): value is ChainId {
  return allChains().some((chain) => chain === value)
}

export function chainName(chain: ChainId): string {
  return chainMeta(chain).name
}

export function chainTicker(chain: ChainId): string {
  return assetMeta({ kind: "native", value: chain }).symbol
}

export function hasScriptChoice(chain: ChainId): boolean {
  return chainMeta(chain).scriptKinds.length > 1
}

export function examplePath(chain: ChainId): string {
  return chainMeta(chain).defaultPath.replace("{index}", "0")
}

export function explorerTxUrl(chain: ChainId, txid: string): string | null {
  if (txid.length === 0 || txid.length > MAX_EXPLORER_TXID_CHARS || /[^A-Za-z0-9]/u.test(txid)) {
    return null
  }
  return explorerUrl(chainMeta(chain).explorerTxUrl, "{txid}", txid)
}

export function explorerAddressUrl(chain: ChainId, address: string): string | null {
  return explorerUrl(chainMeta(chain).explorerAddressUrl, "{address}", address)
}

function explorerUrl(template: string | null, placeholder: string, value: string): string | null {
  if (template === null) {
    return null
  }
  return template.replace(placeholder, encodeURIComponent(value).replaceAll("%3A", ":"))
}

export function nativeAsset(chain: ChainId): AssetId {
  return { kind: "native", value: chain }
}

export function assetDecimals(asset: AssetId): number {
  return assetMeta(asset).decimals
}

export function assetSymbol(asset: AssetId): string {
  return assetMeta(asset).symbol
}

export function assetName(asset: AssetId): string {
  return assetMeta(asset).name
}

export function assetChain(asset: AssetId): ChainId {
  return assetMeta(asset).chain
}

export function assetCoin(asset: AssetId): Coin | null {
  return assetMeta(asset).coin
}

export function shownAsset(chain: ChainId, shownTokens: readonly TokenId[]): AssetId {
  const token = shownTokens.find((entry) => assetChain({ kind: "token", value: entry }) === chain)
  return token === undefined ? nativeAsset(chain) : { kind: "token", value: token }
}

export function withShownAsset(shownTokens: readonly TokenId[], asset: AssetId): TokenId[] {
  const chain = assetChain(asset)
  const others = shownTokens.filter(
    (token) => assetChain({ kind: "token", value: token }) !== chain,
  )
  return asset.kind === "token" ? [...others, asset.value] : others
}

export function assetLabel(asset: AssetId): string {
  const entry = assetMeta(asset)
  return `${entry.symbol} on ${chainName(entry.chain)}`
}

export function formatAssetAmount(amount: Amount, asset: AssetId): string {
  return `${formatAmount(amount, assetDecimals(asset))} ${assetLabel(asset)}`
}

function nativeFirst(asset: AssetId): number {
  return asset.kind === "native" ? 0 : 1
}

export function assetsForChain(chain: ChainId): AssetId[] {
  return allAssets()
    .filter((entry) => entry.chain === chain)
    .map((entry) => entry.asset)
    .toSorted((a, b) => nativeFirst(a) - nativeFirst(b))
}

interface AssetGroup {
  label: string | undefined
  assets: readonly AssetId[]
}

export function groupAssets(assets: readonly AssetId[]): AssetGroup[] {
  const groups: AssetGroup[] = [
    { label: undefined, assets: assets.filter((asset) => asset.kind === "native") },
    {
      label: "Tokens",
      assets: assets.filter(
        (asset) => asset.kind === "token" && !assetMeta(asset).isRealWorldAsset,
      ),
    },
    {
      label: "Real world assets",
      assets: assets.filter((asset) => assetMeta(asset).isRealWorldAsset),
    },
  ]
  return groups.filter((group) => group.assets.length > 0)
}

export const SCRIPT_LABELS: Record<ScriptKind, string> = {
  p2tr: "Taproot",
  p2wpkh: "Native SegWit",
  p2sh_p2wpkh: "Wrapped SegWit",
  p2pkh: "Legacy",
  ton_v5r1: "V5R1",
  ton_v4r2: "V4R2",
  ton_v3r2: "V3R2",
}

export function describeKeySource(source: KeySource): string {
  switch (source.kind) {
    case "derived": {
      return source.path
    }
    case "imported": {
      return "Standalone key"
    }
    case "subaddress": {
      return `Subaddress ${String(source.major)}/${String(source.minor)}`
    }
  }
}

export function isStandaloneKey(source: KeySource): boolean {
  return source.kind === "imported"
}

export function keyHolders(
  rows: readonly AddressRecord[],
  row: AddressRecord,
): readonly AddressRecord[] {
  const { source } = row
  if (source.kind !== "imported") {
    return [row]
  }
  return rows.filter(
    (other) => other.source.kind === "imported" && other.source.vaultEntry === source.vaultEntry,
  )
}

export const ORIGIN_LABELS: Record<EndpointOrigin, string> = {
  pinned: "Pinned",
  user: "Custom",
  default: "Default",
}

export const TRANSPORT_LABELS: Record<Transport, string> = {
  electrum_ssl: "Electrum over SSL",
  http_jsonrpc: "HTTP JSON-RPC",
  ws_jsonrpc: "WebSocket JSON-RPC",
  monero_rpc: "Monero RPC",
  tron_http: "TRON HTTP",
  blockbook: "Blockbook",
  fee_api: "Fee API",
  history_api: "History API",
}

const PROBE_FAILURE_TEXTS: Record<ProbeFailure, string> = {
  timed_out: "The endpoint did not answer in time.",
  no_connection: "Jelly could not connect to the endpoint.",
  tls: "The endpoint's secure connection could not be verified.",
  dropped: "The connection closed before the endpoint answered.",
  http_status: "The endpoint answered with an error.",
  rate_limited: "The endpoint is limiting requests. Try again later.",
  bad_answer: "The endpoint answered with something Jelly cannot read.",
  not_probed: "Jelly cannot check this kind of endpoint on this network.",
}

interface ProbeVerdict {
  heading: string
  tone: "info" | "danger"
}

const NO_ANSWER: ProbeVerdict = { heading: "Endpoint did not answer", tone: "danger" }

const UNUSABLE_ANSWER: ProbeVerdict = { heading: "Endpoint gave no usable answer", tone: "danger" }

const PROBE_FAILURE_VERDICTS: Record<ProbeFailure, ProbeVerdict> = {
  timed_out: NO_ANSWER,
  no_connection: NO_ANSWER,
  tls: NO_ANSWER,
  dropped: NO_ANSWER,
  http_status: UNUSABLE_ANSWER,
  rate_limited: UNUSABLE_ANSWER,
  bad_answer: UNUSABLE_ANSWER,
  not_probed: { heading: "Endpoint not checked", tone: "info" },
}

export function probeVerdict(report: ProbeReport): ProbeVerdict {
  switch (report.kind) {
    case "verified":
    case "reachable": {
      return { heading: "Endpoint answered", tone: "info" }
    }
    case "wrong_network": {
      return { heading: "Endpoint is on another network", tone: "danger" }
    }
    case "unreachable": {
      return PROBE_FAILURE_VERDICTS[report.failure]
    }
  }
}

export function probeDetail(report: ProbeReport): string {
  switch (report.kind) {
    case "verified":
    case "reachable": {
      return `The round trip took ${String(Math.round(report.latencyMs))} ms.`
    }
    case "wrong_network": {
      return "This endpoint serves a different network."
    }
    case "unreachable": {
      return PROBE_FAILURE_TEXTS[report.failure]
    }
  }
}
