import { useQuery, useSuspenseQuery } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { useState } from "react"

import { TxDayList } from "@/features/wallet/activity.tsx"
import { AllocationMark, Donut, allocationSlices } from "@/features/wallet/donut.tsx"
import {
  HeroFigure,
  HeroLogo,
  HeroPanel,
  HeroPanelFigures,
  HeroPanelTop,
  useHoldings,
  useSetShownAsset,
} from "@/features/wallet/layout.tsx"
import { TxDetailDialog } from "@/features/wallet/tx-detail.tsx"
import type { AssetId, FiatCurrency, TxEntry } from "@/ipc/bindings.gen.ts"
import { assetChain, assetFromKey, assetKey, assetName, chainName } from "@/lib/chain.ts"
import { formatFiat } from "@/lib/fiat.ts"
import { NONE } from "@/lib/format.ts"
import { allocate, formatShare } from "@/lib/portfolio.ts"
import type { Allocation, AssetValue } from "@/lib/portfolio.ts"
import { settingQueries } from "@/query/setting.ts"
import { txPageQuery, txQueries } from "@/query/tx.ts"
import { usePrices } from "@/store.ts"
import { useDialogPayload } from "@/ui/dialog.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Panel, PanelBody, PanelHeader } from "@/ui/panel.tsx"
import { Row } from "@/ui/row.tsx"
import { Segmented } from "@/ui/segmented.tsx"
import { CryptoAmount, FiatValue } from "@/widgets/amounts.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import { AssetLogo, ChainLogo } from "@/widgets/logos.tsx"

const LONG_FIGURE_CHARS = 15

function isUnvalued(allocation: Allocation): boolean {
  return allocation.assets.length > 0 && allocation.total === 0n && allocation.hasUnpriced
}

function priceNote(allocation: Allocation, isPriceFeedEnabled: boolean): string | null {
  if (!allocation.hasUnpriced) {
    return null
  }
  if (!isUnvalued(allocation)) {
    return "Some holdings have no price."
  }
  return isPriceFeedEnabled
    ? "Values appear once prices arrive."
    : "Turn on prices in settings to see values."
}

interface SummaryPanelProps {
  allocation: Allocation
  isPriceFeedEnabled: boolean
}

function SummaryPanel({ allocation, isPriceFeedEnabled }: SummaryPanelProps) {
  const isKnown = !isUnvalued(allocation)
  const text = isKnown ? formatFiat(allocation.total, allocation.currency) : NONE
  const chainCount = new Set(allocation.assets.map((entry) => assetChain(entry.asset))).size
  const note = priceNote(allocation, isPriceFeedEnabled)

  return (
    <HeroPanel>
      <HeroPanelTop>
        <HeroLogo chain={null}>
          <AllocationMark allocation={allocation} className="size-8" />
        </HeroLogo>
        <span className="text-title">Portfolio</span>
      </HeroPanelTop>
      <HeroPanelFigures>
        <HeroFigure isLong={text.length > LONG_FIGURE_CHARS}>
          {isKnown ? (
            <FiatValue
              value={allocation.total}
              currency={allocation.currency}
              isStale={allocation.isStale}
            />
          ) : (
            <span className="text-fg-faint">{text}</span>
          )}
        </HeroFigure>
        <span className="text-body text-fg-muted">
          {chainCount === 0
            ? "No holdings yet"
            : `Held on ${String(chainCount)} chain${chainCount === 1 ? "" : "s"}`}
        </span>
        {note === null ? null : <span className="text-caption text-fg-faint">{note}</span>}
      </HeroPanelFigures>
    </HeroPanel>
  )
}

interface HoldingRowProps {
  entry: AssetValue
  total: bigint
  currency: FiatCurrency
  isActive: boolean
  onActiveChange: (key: string | null) => void
  onSelect: (asset: AssetId) => void
}

function HoldingRow({
  entry,
  total,
  currency,
  isActive,
  onActiveChange,
  onSelect,
}: HoldingRowProps) {
  const { asset } = entry
  const chain = assetChain(asset)
  const key = assetKey(asset)

  return (
    <Row
      render={<Link to="/wallet/$chain" params={{ chain }} />}
      isActive={isActive}
      onClick={() => {
        onSelect(asset)
      }}
      onPointerEnter={() => {
        onActiveChange(key)
      }}
      onPointerLeave={() => {
        onActiveChange(null)
      }}
      onFocus={() => {
        onActiveChange(key)
      }}
      onBlur={() => {
        onActiveChange(null)
      }}
    >
      <AssetLogo asset={asset} shouldShowChain className="size-8" />
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="flex items-baseline justify-between gap-2">
          <span className="truncate text-body">{assetName(asset)}</span>
          {entry.isPriced ? (
            <FiatValue
              value={entry.value}
              currency={currency}
              isStale={entry.isStale}
              className="shrink-0 text-body"
            />
          ) : (
            <span className="shrink-0 text-body">No price</span>
          )}
        </span>
        <span className="flex items-baseline justify-between gap-2">
          <CryptoAmount
            amount={entry.amount}
            asset={asset}
            isCompact
            className="min-w-0 text-caption text-fg-faint"
          />
          <span className="shrink-0 text-caption text-fg-muted tabular-nums">
            {entry.isPriced ? formatShare(entry.value, total) : NONE}
          </span>
        </span>
      </span>
    </Row>
  )
}

interface HoldingsPanelProps {
  allocation: Allocation
  activeKey: string | null
  onActiveChange: (key: string | null) => void
  onSelect: (asset: AssetId) => void
}

function HoldingsPanel({ allocation, activeKey, onActiveChange, onSelect }: HoldingsPanelProps) {
  return (
    <Panel className="flex-1">
      <PanelHeader title="Holdings" count={allocation.assets.length} />
      <PanelBody>
        {allocation.assets.length === 0 ? (
          <EmptyState
            isCompact
            title="No holdings yet"
            description="Receive coins and they will show up here."
          />
        ) : (
          allocation.assets.map((entry) => (
            <HoldingRow
              key={assetKey(entry.asset)}
              entry={entry}
              total={allocation.total}
              currency={allocation.currency}
              isActive={assetKey(entry.asset) === activeKey}
              onActiveChange={onActiveChange}
              onSelect={onSelect}
            />
          ))
        )}
      </PanelBody>
    </Panel>
  )
}

function emptyWheelLabel(allocation: Allocation, isPriceFeedEnabled: boolean): string {
  if (allocation.assets.length === 0) {
    return "No holdings yet"
  }
  return isPriceFeedEnabled ? "No prices yet" : "Prices are off"
}

interface AllocationStageProps {
  allocation: Allocation
  activeKey: string | null
  onActiveChange: (key: string | null) => void
  onSelect: (asset: AssetId) => void
  isPriceFeedEnabled: boolean
}

function AllocationStage({
  allocation,
  activeKey,
  onActiveChange,
  onSelect,
  isPriceFeedEnabled,
}: AllocationStageProps) {
  const navigate = useNavigate()
  const slices = allocationSlices(allocation)
  const focused =
    allocation.assets.find((entry) => assetKey(entry.asset) === activeKey) ??
    allocation.assets.at(0)

  return (
    <div className="@container-size flex min-h-0 flex-1 items-center justify-center px-4 pb-4">
      <Donut
        slices={slices}
        activeId={activeKey}
        onActiveChange={onActiveChange}
        onSelect={(id) => {
          const asset = assetFromKey(id)
          if (asset !== null) {
            onSelect(asset)
            void navigate({ to: "/wallet/$chain", params: { chain: assetChain(asset) } })
          }
        }}
        className="size-[100cqmin] max-h-88 max-w-88"
      >
        {slices.length === 0 || focused === undefined ? (
          <span className="mt-2 max-w-3/5 truncate text-body text-fg-muted">
            {emptyWheelLabel(allocation, isPriceFeedEnabled)}
          </span>
        ) : (
          <>
            <AssetLogo asset={focused.asset} className="mb-2 size-5" />
            <span className="text-display">
              {focused.isPriced ? formatShare(focused.value, allocation.total) : NONE}
            </span>
            <span className="mt-2 max-w-3/5 truncate text-body text-fg-muted">
              {assetName(focused.asset)}
            </span>
            <span className="mb-1 flex items-center gap-1 text-caption text-fg-faint">
              <ChainLogo chain={assetChain(focused.asset)} className="size-icon-sm" />
              {chainName(assetChain(focused.asset))}
            </span>
            <span className="text-caption text-fg-faint tabular-nums">
              {focused.isPriced ? formatFiat(focused.value, allocation.currency) : "No price"}
            </span>
          </>
        )}
      </Donut>
    </div>
  )
}

const SKELETON_ROWS = 4

function RecentActivity() {
  const page = useQuery(txQueries.page(txPageQuery({ chain: null })))
  const detail = useDialogPayload<TxEntry>()
  const [scrollArea, setScrollArea] = useState<HTMLDivElement | null>(null)
  const rows = page.data?.rows ?? []

  return (
    <PanelBody ref={setScrollArea}>
      <ListStatus
        state={page}
        isEmpty={rows.length === 0}
        errorTitle="Could not load the activity"
        emptyTitle="No transactions yet"
        emptyDescription="Receive something and it will show up here."
        skeletonRowCount={SKELETON_ROWS}
      >
        <TxDayList
          rows={rows}
          scrollElement={scrollArea}
          shouldShowChain
          onOpen={(row) => {
            detail.show(row)
          }}
        />
      </ListStatus>
      <TxDetailDialog key={detail.key} {...detail.props} row={detail.payload} shouldNameNative />
    </PanelBody>
  )
}

type Board = "allocation" | "activity"

const BOARDS = [
  { value: "allocation", label: "Allocation" },
  { value: "activity", label: "Activity" },
] as const satisfies readonly { value: Board; label: string }[]

export function PortfolioView() {
  const holdings = useHoldings()
  const prices = usePrices()
  const allocation = allocate(holdings, prices)
  const { isPriceFeedEnabled } = useSuspenseQuery(settingQueries.all()).data
  const saveShownAsset = useSetShownAsset()
  const [activeKey, setActiveKey] = useState<string | null>(null)
  const [board, setBoard] = useState<Board>("allocation")

  function showAsset(asset: AssetId): void {
    saveShownAsset.mutate(asset)
  }

  return (
    <>
      <div className="flex w-72 shrink-0 flex-col gap-3">
        <SummaryPanel allocation={allocation} isPriceFeedEnabled={isPriceFeedEnabled} />
        <HoldingsPanel
          allocation={allocation}
          activeKey={activeKey}
          onActiveChange={setActiveKey}
          onSelect={showAsset}
        />
      </div>
      <Panel className="min-w-0 flex-1">
        <PanelHeader>
          <Segmented value={board} onValueChange={setBoard} items={BOARDS} />
        </PanelHeader>
        {board === "allocation" ? (
          <AllocationStage
            allocation={allocation}
            activeKey={activeKey}
            onActiveChange={setActiveKey}
            onSelect={showAsset}
            isPriceFeedEnabled={isPriceFeedEnabled}
          />
        ) : (
          <RecentActivity />
        )}
      </Panel>
    </>
  )
}
