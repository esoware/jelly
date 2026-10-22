import { infiniteQueryOptions, queryOptions, skipToken } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type {
  FeeSelection,
  PendingTxId,
  PreparedTx,
  PreviewDigest,
  TxId,
  TxPage,
  TxQuery,
  TxRequest,
} from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

const TX_PAGE_SIZE = 50

const QUOTE_STALE_MS = 20_000
const QUOTE_GC_MS = 60_000

type TxPageFilters = Pick<TxQuery, "chain"> &
  Partial<Pick<TxQuery, "asset" | "addressId" | "txid" | "direction">>

export function txPageQuery(filters: TxPageFilters): TxQuery {
  return {
    chain: filters.chain,
    asset: filters.asset ?? null,
    addressId: filters.addressId ?? null,
    txid: filters.txid ?? null,
    direction: filters.direction ?? null,
    cursor: null,
    limit: TX_PAGE_SIZE,
  }
}

export type QuoteTarget =
  | { kind: "send"; request: TxRequest | null }
  | { kind: "bump"; txid: TxId }
  | { kind: "cancel"; txid: TxId }

function quoteFn(target: QuoteTarget, fee: FeeSelection | null) {
  if (fee === null) {
    return skipToken
  }
  switch (target.kind) {
    case "bump": {
      return () => unwrap(commands.quoteBump(target.txid, fee))
    }
    case "cancel": {
      return () => unwrap(commands.quoteCancel(target.txid, fee))
    }
    case "send": {
      const { request } = target
      return request === null ? skipToken : () => unwrap(commands.quoteFees({ ...request, fee }))
    }
  }
}

export const txKeys = {
  root: ["txs"] as const,
  quotes: ["fee-quotes"] as const,
}

export const txQueries = {
  page: (query: TxQuery) =>
    queryOptions({
      queryKey: [...txKeys.root, "page", query] as const,
      queryFn: () => unwrap(commands.listTxs(query)),
    }),

  stream: (query: TxQuery) =>
    infiniteQueryOptions({
      queryKey: [...txKeys.root, "stream", query] as const,
      queryFn: ({ pageParam }: { pageParam: string | null }) =>
        unwrap(commands.listTxs({ ...query, cursor: pageParam })),
      initialPageParam: null,
      getNextPageParam: (page: TxPage): string | null => page.nextCursor,
    }),

  quote: (target: QuoteTarget, fee: FeeSelection | null) =>
    queryOptions({
      queryKey: [...txKeys.quotes, target, fee] as const,
      queryFn: quoteFn(target, fee),
      retry: false,
      staleTime: QUOTE_STALE_MS,
      gcTime: QUOTE_GC_MS,
    }),
}

export const txCommands = {
  prepare: (request: TxRequest): Promise<PreparedTx> => unwrap(commands.prepareTx(request)),

  submit: (pendingId: PendingTxId, password: string, previewDigest: PreviewDigest): Promise<TxId> =>
    unwrap(commands.submitTx(pendingId, password, previewDigest)),

  discard: (pendingId: PendingTxId): Promise<null> => unwrap(commands.deletePendingTx(pendingId)),

  bumpFee: (txid: TxId, fee: FeeSelection): Promise<PreparedTx> =>
    unwrap(commands.bumpFee(txid, fee)),

  cancel: (txid: TxId, fee: FeeSelection): Promise<PreparedTx> =>
    unwrap(commands.cancelTx(txid, fee)),

  openExplorer: (txid: TxId): Promise<null> => unwrap(commands.openTxExplorer(txid)),
}
