import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query"
import { useVirtualizer } from "@tanstack/react-virtual"
import { useEffect, useMemo, useRef, useState } from "react"

import {
  ConfirmationMark,
  TxAmount,
  TxDetailDialog,
  useTxFiat,
} from "@/features/wallet/tx-detail.tsx"
import type { AddressRecord, ChainId, TxDirection, TxEntry, TxQuery } from "@/ipc/bindings.gen.ts"
import { assetChain, assetFromKey, assetKey, assetSymbol, assetsForChain } from "@/lib/chain.ts"
import { HISTORICAL_PRICE_NOTICE } from "@/lib/fiat.ts"
import { formatClock, truncateMiddle } from "@/lib/format.ts"
import {
  DIRECTION_PARTIES,
  DIRECTION_TITLES,
  TX_DIRECTIONS,
  isTxDirection,
  outcomeLabel,
  txListEntries,
  txTitle,
} from "@/lib/tx.ts"
import { addressQueries } from "@/query/address.ts"
import { txPageQuery, txQueries } from "@/query/tx.ts"
import { Button } from "@/ui/button.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import { Icon } from "@/ui/icon.tsx"
import type { IconName } from "@/ui/icon.tsx"
import { Menu, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "@/ui/menu.tsx"
import { MiddleTruncate } from "@/ui/middle-truncate.tsx"
import { Panel, PanelBody, PanelHeader } from "@/ui/panel.tsx"
import { Row } from "@/ui/row.tsx"
import { Spinner } from "@/ui/spinner.tsx"
import { FiatValue } from "@/widgets/amounts.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import type { ListState } from "@/widgets/list-status.tsx"
import { AssetLogo } from "@/widgets/logos.tsx"

type TxFilter = Pick<TxQuery, "asset" | "addressId" | "direction">

const NO_TX_FILTER: TxFilter = { asset: null, addressId: null, direction: null }

function isTxFiltered(filter: TxFilter): boolean {
  return filter.asset !== null || filter.addressId !== null || filter.direction !== null
}

interface TxStream {
  state: ListState
  rows: readonly TxEntry[]
  canLoadMore: boolean
  isLoadingMore: boolean
  loadMore: () => void
}

function useTxStream(chain: ChainId, filter: TxFilter): TxStream {
  const query = useInfiniteQuery({
    ...txQueries.stream(txPageQuery({ chain, ...filter })),
    placeholderData: keepPreviousData,
    select: (data) => data.pages.flatMap((page) => page.rows),
  })

  return {
    state: query,
    rows: query.data ?? [],
    canLoadMore: query.hasNextPage,
    isLoadingMore: query.isFetchingNextPage,
    loadMore: () => {
      if (query.hasNextPage && !query.isFetchingNextPage) {
        void query.fetchNextPage()
      }
    },
  }
}

const ANY = "any"

const ADDRESS_HEAD_CHARS = 8
const ADDRESS_TAIL_CHARS = 5

interface Choice {
  value: string
  label: string
}

const DIRECTION_CHOICES: readonly Choice[] = TX_DIRECTIONS.map((direction) => ({
  value: direction,
  label: DIRECTION_TITLES[direction],
}))

function addressName(row: AddressRecord): string {
  return row.label ?? truncateMiddle(row.address, ADDRESS_HEAD_CHARS, ADDRESS_TAIL_CHARS)
}

interface ClearChipProps {
  label: string
  onClear: () => void
}

function ClearChip({ label, onClear }: ClearChipProps) {
  return (
    <Button variant="subtle" size="xs" className="pr-1" onClick={onClear}>
      {label}
      <Icon name="close" />
    </Button>
  )
}

interface FilterGroupProps {
  label: string
  anyLabel: string
  value: string
  choices: readonly Choice[]
  onValueChange: (value: string) => void
}

function FilterGroup({ label, anyLabel, value, choices, onValueChange }: FilterGroupProps) {
  return (
    <MenuRadioGroup label={label} value={value} onValueChange={onValueChange}>
      <MenuRadioItem value={ANY} shouldStayOpen>
        {anyLabel}
      </MenuRadioItem>
      {choices.map((entry) => (
        <MenuRadioItem key={entry.value} value={entry.value} shouldStayOpen>
          {entry.label}
        </MenuRadioItem>
      ))}
    </MenuRadioGroup>
  )
}

interface FilterMenuProps {
  chain: ChainId
  filter: TxFilter
  onFilterChange: (filter: TxFilter) => void
}

function FilterMenu({ chain, filter, onFilterChange }: FilterMenuProps) {
  const addresses = useQuery(addressQueries.list(chain))
  const rows = addresses.data ?? []
  const assetChoices = assetsForChain(chain).map((asset) => ({
    value: assetKey(asset),
    label: assetSymbol(asset),
  }))
  const addressChoices = rows.map((row) => ({ value: String(row.id), label: addressName(row) }))
  const chosenAddress = rows.find((row) => row.id === filter.addressId) ?? null

  function update(patch: Partial<TxFilter>): void {
    onFilterChange({ ...filter, ...patch })
  }

  return (
    <>
      {filter.direction === null ? null : (
        <ClearChip
          label={DIRECTION_TITLES[filter.direction]}
          onClear={() => {
            update({ direction: null })
          }}
        />
      )}
      {filter.asset === null ? null : (
        <ClearChip
          label={assetSymbol(filter.asset)}
          onClear={() => {
            update({ asset: null })
          }}
        />
      )}
      {chosenAddress === null ? null : (
        <ClearChip
          label={addressName(chosenAddress)}
          onClear={() => {
            update({ addressId: null })
          }}
        />
      )}
      <Menu>
        <MenuTrigger
          render={<Button variant={isTxFiltered(filter) ? "secondary" : "ghost"} size="icon-sm" />}
        >
          <Icon name="filter" />
        </MenuTrigger>
        <MenuPopup align="end">
          <FilterGroup
            label="Direction"
            anyLabel="Any direction"
            value={filter.direction ?? ANY}
            choices={DIRECTION_CHOICES}
            onValueChange={(next) => {
              update({ direction: isTxDirection(next) ? next : null })
            }}
          />
          {assetChoices.length > 1 ? (
            <FilterGroup
              label="Asset"
              anyLabel="All assets"
              value={filter.asset === null ? ANY : assetKey(filter.asset)}
              choices={assetChoices}
              onValueChange={(next) => {
                update({ asset: next === ANY ? null : assetFromKey(next) })
              }}
            />
          ) : null}
          {addressChoices.length > 1 ? (
            <FilterGroup
              label="Address"
              anyLabel="All addresses"
              value={filter.addressId === null ? ANY : String(filter.addressId)}
              choices={addressChoices}
              onValueChange={(next) => {
                update({ addressId: rows.find((row) => String(row.id) === next)?.id ?? null })
              }}
            />
          ) : null}
        </MenuPopup>
      </Menu>
    </>
  )
}

interface TxFiatProps {
  row: TxEntry
  className?: string | undefined
}

function TxFiat({ row, className }: TxFiatProps) {
  const fiat = useTxFiat(row)
  const value = fiat.historicalValue ?? fiat.currentValue
  const isHistorical = fiat.historicalValue !== null

  if (value === null) {
    return null
  }

  return (
    <FiatValue
      value={value}
      currency={fiat.currency}
      isStale={!isHistorical && fiat.isCurrentStale}
      notice={isHistorical ? HISTORICAL_PRICE_NOTICE : undefined}
      isSigned
      className={className}
    />
  )
}

const DIRECTION_GLYPHS: Record<TxDirection, IconName> = {
  incoming: "receive",
  outgoing: "send",
  self: "self",
}

interface TxRowProps {
  row: TxEntry
  shouldShowChain: boolean
  onOpen: () => void
}

function TxRow({ row, shouldShowChain, onOpen }: TxRowProps) {
  const outcomeText = outcomeLabel(row)
  return (
    <Row render={<button type="button" />} onClick={onOpen}>
      <span className="relative shrink-0">
        <span
          data-direction={row.direction}
          className="flex size-8 items-center justify-center rounded-full data-[direction=incoming]:bg-ok-subtle data-[direction=incoming]:text-ok data-[direction=outgoing]:bg-danger-subtle data-[direction=outgoing]:text-danger data-[direction=self]:bg-surface data-[direction=self]:text-fg-muted [&_svg]:size-icon-md"
        >
          <Icon name={DIRECTION_GLYPHS[row.direction]} />
        </span>
        {shouldShowChain || row.asset.kind === "token" ? (
          <AssetLogo asset={row.asset} shouldShowChain className="absolute -right-1 -bottom-1" />
        ) : null}
      </span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="text-body">{txTitle(row, shouldShowChain)}</span>
        <span className="flex min-w-0 items-center gap-2 text-caption text-fg-faint">
          <span className="flex shrink-0 items-center gap-1.5 tabular-nums empty:hidden">
            {outcomeText === null ? (
              <ConfirmationMark chain={assetChain(row.asset)} confirmations={row.confirmations} />
            ) : (
              <span data-outcome={row.outcome} className="data-[outcome=failed]:text-danger">
                {outcomeText}
              </span>
            )}
            {row.timestampUnixSecs === null ? null : formatClock(row.timestampUnixSecs)}
          </span>
          {row.counterparty === null ? null : (
            <span className="flex min-w-0 items-center gap-1">
              {DIRECTION_PARTIES[row.direction]}
              <MiddleTruncate value={row.counterparty} />
            </span>
          )}
        </span>
      </span>
      <span className="flex shrink-0 flex-col items-end text-body">
        <TxAmount
          row={row}
          className={outcomeText === null ? undefined : "text-fg-faint line-through"}
        />
        <TxFiat row={row} className="text-caption text-fg-faint" />
      </span>
    </Row>
  )
}

const HEADING_HEIGHT_PX = 28
const ROW_HEIGHT_PX = 48
const OVERSCAN = 8

interface TxDayListProps {
  rows: readonly TxEntry[]
  scrollElement: HTMLElement | null
  shouldShowChain?: boolean | undefined
  onOpen: (row: TxEntry) => void
}

export function TxDayList({
  rows,
  scrollElement,
  shouldShowChain = false,
  onOpen,
}: TxDayListProps) {
  const entries = useMemo(() => txListEntries(rows), [rows])
  // oxlint-disable-next-line react/incompatible-library -- The compiler skips this component, so entries is memoized by hand.
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => scrollElement,
    estimateSize: (index) => (entries[index]?.kind === "day" ? HEADING_HEIGHT_PX : ROW_HEIGHT_PX),
    getItemKey: (index) => entries[index]?.key ?? index,
    overscan: OVERSCAN,
  })
  const items = virtualizer.getVirtualItems()

  return (
    <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
      <div
        className="absolute top-0 left-0 w-full"
        style={{ transform: `translateY(${String(items[0]?.start ?? 0)}px)` }}
      >
        {items.map((item) => {
          const entry = entries[item.index]
          if (entry === undefined) {
            return null
          }
          return (
            <div key={item.key} data-index={item.index} ref={virtualizer.measureElement}>
              {entry.kind === "day" ? (
                <h3 className="px-2 pt-2 pb-1 text-caption font-medium text-fg-faint">
                  {entry.label}
                </h3>
              ) : (
                <TxRow
                  row={entry.row}
                  shouldShowChain={shouldShowChain}
                  onOpen={() => {
                    onOpen(entry.row)
                  }}
                />
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}

const SKELETON_ROWS = 4

interface ActivityPanelProps {
  chain: ChainId
  onReceive: () => void
}

export function ActivityPanel({ chain, onReceive }: ActivityPanelProps) {
  const [chosenFilter, setChosenFilter] = useState<TxFilter>(NO_TX_FILTER)
  const addresses = useQuery(addressQueries.list(chain))
  const isChosenAddressGone =
    chosenFilter.addressId !== null &&
    addresses.data !== undefined &&
    !addresses.data.some((row) => row.id === chosenFilter.addressId)
  const filter = isChosenAddressGone ? { ...chosenFilter, addressId: null } : chosenFilter
  const detail = useDialogPayload<TxEntry>()
  const feed = useTxStream(chain, filter)
  const [scrollArea, setScrollArea] = useState<HTMLDivElement | null>(null)
  const sentinelRef = useRef<HTMLDivElement>(null)
  const { canLoadMore, loadMore } = feed

  useEffect(() => {
    const node = sentinelRef.current
    if (node === null || !canLoadMore) {
      return undefined
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        loadMore()
      }
    })
    observer.observe(node)
    return () => {
      observer.disconnect()
    }
  }, [canLoadMore, loadMore])

  const isFiltered = isTxFiltered(filter)

  return (
    <Panel className="min-w-0 flex-1">
      <PanelHeader
        title="Activity"
        actions={<FilterMenu chain={chain} filter={filter} onFilterChange={setChosenFilter} />}
      />

      <PanelBody ref={setScrollArea}>
        <ListStatus
          state={feed.state}
          isEmpty={feed.rows.length === 0}
          errorTitle="Could not load the activity"
          emptyTitle={isFiltered ? "Nothing matches these filters" : "No transactions yet"}
          emptyDescription={
            isFiltered
              ? "Remove a filter to see everything."
              : "Receive something and it will show up here."
          }
          emptyAction={
            isFiltered ? undefined : (
              <Button size="sm" variant="secondary" onClick={onReceive}>
                <Icon name="receive" />
                Receive
              </Button>
            )
          }
          skeletonRowCount={SKELETON_ROWS}
        >
          <TxDayList
            rows={feed.rows}
            scrollElement={scrollArea}
            onOpen={(row) => {
              detail.show(row)
            }}
          />
          <div ref={sentinelRef} className="flex h-10 items-center justify-center text-fg-faint">
            {feed.isLoadingMore ? <Spinner /> : null}
          </div>
        </ListStatus>
      </PanelBody>

      <TxDetailDialog key={detail.key} {...detail.props} row={detail.payload} />
    </Panel>
  )
}
