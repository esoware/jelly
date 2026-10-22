import { hashKey } from "@tanstack/react-query"
import type { QueryClient, QueryKey } from "@tanstack/react-query"
import type { AnyRouter } from "@tanstack/react-router"

import { events } from "@/ipc/bindings.gen.ts"
import type { ChainId, LockState, StartupStatus, TxDetected } from "@/ipc/bindings.gen.ts"
import { attempt, onSessionLost } from "@/ipc/client.ts"
import { allChains } from "@/lib/chain.ts"
import { describeIncomingTx } from "@/lib/tx.ts"
import { addressQueries } from "@/query/address.ts"
import { appCommands, appQueries } from "@/query/app.ts"
import { chainQueries } from "@/query/chain.ts"
import { contactQueries } from "@/query/contact.ts"
import { endpointQueries } from "@/query/endpoint.ts"
import { loadPrices, moneyKeys, moneyQueries } from "@/query/money.ts"
import { settingQueries } from "@/query/setting.ts"
import { txKeys, txPageQuery, txQueries } from "@/query/tx.ts"
import { ensureLockState, walletKeys, walletQueries } from "@/query/wallet.ts"
import {
  notify,
  priceSignal,
  recordStartupStatus,
  resetPriceHistory,
  resetPrices,
  setPriceHistory,
  setPrices,
} from "@/store.ts"

interface Channel<T> {
  listen: (handler: (event: { payload: T }) => void) => Promise<unknown>
}

function subscribe<T>(channel: Channel<T>, listener: (payload: T) => void): void {
  attempt("Event listener registration", () =>
    channel.listen((event) => {
      listener(event.payload)
    }),
  )
}

function prefetchAppQueries(client: QueryClient): void {
  void Promise.allSettled([client.query(appQueries.version()), client.query(appQueries.dataDir())])
}

async function prefetchSession(client: QueryClient): Promise<void> {
  void Promise.allSettled([
    client.query(walletQueries.all()),
    client.query(settingQueries.all()),
    client.query(contactQueries.all()),
    client.query(moneyQueries.balances()),
    client.query(moneyQueries.totals()),
    ...allChains().map((chain) => client.query(endpointQueries.list(chain))),
  ])

  let chains: readonly ChainId[] = []
  try {
    chains = await client.query(chainQueries.enabled())
  } catch {}

  void Promise.allSettled(
    chains.flatMap((chain) => [
      client.query(addressQueries.list(chain)),
      client.infiniteQuery(txQueries.stream(txPageQuery({ chain }))),
    ]),
  )
}

const INVALIDATE_DEBOUNCE_MS = 250
const KEPT_ON_LOCK_HASHES = new Set([
  hashKey(walletQueries.lockState().queryKey),
  hashKey(walletQueries.all().queryKey),
  hashKey(appQueries.version().queryKey),
  hashKey(appQueries.dataDir().queryKey),
])

function createInvalidator(client: QueryClient): (key: QueryKey) => void {
  const scheduledHashes = new Set<string>()

  function schedule(key: QueryKey): void {
    const hash = hashKey(key)
    if (scheduledHashes.has(hash)) {
      return
    }
    scheduledHashes.add(hash)
    setTimeout(() => {
      scheduledHashes.delete(hash)
      void client.invalidateQueries({ queryKey: key })
    }, INVALIDATE_DEBOUNCE_MS)
  }

  return schedule
}

async function announceIncoming(client: QueryClient, detected: TxDetected): Promise<void> {
  const lock = client.getQueryData(walletQueries.lockState().queryKey)
  if (lock?.kind !== "unlocked") {
    return
  }
  const query = txPageQuery({
    chain: detected.txid.chain,
    asset: detected.asset,
    txid: detected.txid.value,
  })
  try {
    const page = await client.query({ ...txQueries.page(query), staleTime: 0, retry: false })
    const entry = page.rows.at(0)
    if (entry?.direction === "incoming") {
      notify.success("Incoming transaction", describeIncomingTx(entry, priceSignal.get()))
    }
  } catch {}
}

async function readStartupStatus(): Promise<StartupStatus | null> {
  try {
    const status = await appCommands.startupStatus()
    recordStartupStatus(status)
    return status
  } catch {
    return null
  }
}

async function loadStartupStatus(client: QueryClient): Promise<void> {
  const status = await readStartupStatus()
  if (status?.kind !== "starting") {
    return
  }
  try {
    await client.query({ ...walletQueries.lockState(), staleTime: 0, retry: false })
  } catch {}
  await loadStartupStatus(client)
}

async function followLockState(router: AnyRouter, state: LockState): Promise<void> {
  const routeIds = new Set(router.state.matches.map((match) => match.routeId))
  if (state.kind === "locked" && routeIds.has("/_unlocked")) {
    await router.navigate({ to: "/unlock", replace: true })
  } else if (state.kind === "unlocked" && routeIds.has("/unlock")) {
    await router.navigate({ to: "/wallet", replace: true })
  }
}

async function retireSession(
  client: QueryClient,
  router: AnyRouter,
  state: LockState,
): Promise<void> {
  try {
    await followLockState(router, state)
  } finally {
    client.removeQueries({ predicate: (query) => !KEPT_ON_LOCK_HASHES.has(query.queryHash) })
    client.getMutationCache().clear()
    resetPrices()
    resetPriceHistory()
  }
}

function startUnlockedSession(client: QueryClient): void {
  void prefetchSession(client)
  void loadPrices()
}

function applyLockState(client: QueryClient, router: AnyRouter, state: LockState): void {
  client.setQueryData(walletQueries.lockState().queryKey, state)

  if (state.kind === "locked") {
    if (state.reason === "idle") {
      notify.warning("The wallet locked automatically", "Unlock the wallet to continue.")
    }
    void retireSession(client, router, state)
    return
  }

  void client.invalidateQueries({ queryKey: walletKeys.root, refetchType: "none" })
  startUnlockedSession(client)
  void followLockState(router, state)
}

async function resyncLockState(client: QueryClient, router: AnyRouter): Promise<void> {
  const key = walletQueries.lockState().queryKey
  await client.invalidateQueries({ queryKey: key })
  const state = client.getQueryData(key)
  if (state?.kind === "locked") {
    await retireSession(client, router, state)
  } else if (state !== undefined) {
    await followLockState(router, state)
  }
}

async function resumeSession(client: QueryClient): Promise<void> {
  const lock = await ensureLockState(client)
  if (lock?.kind === "unlocked") {
    startUnlockedSession(client)
  }
}

export function startSession(client: QueryClient, router: AnyRouter): void {
  const scheduleInvalidation = createInvalidator(client)

  subscribe(events.balanceChanged, () => {
    scheduleInvalidation(moneyKeys.root)
  })

  subscribe(events.txDetected, (detected) => {
    scheduleInvalidation(txKeys.root)
    scheduleInvalidation(moneyKeys.root)
    void announceIncoming(client, detected)
  })

  subscribe(events.txUpdated, () => {
    scheduleInvalidation(txKeys.root)
    scheduleInvalidation(moneyKeys.root)
  })

  subscribe(events.txConfirmed, () => {
    scheduleInvalidation(txKeys.root)
  })

  subscribe(events.startupChanged, recordStartupStatus)

  subscribe(events.pricesChanged, setPrices)
  subscribe(events.priceHistoryChanged, setPriceHistory)

  subscribe(events.lockChanged, (state) => {
    applyLockState(client, router, state)
  })

  onSessionLost(() => {
    void resyncLockState(client, router)
  })

  prefetchAppQueries(client)
  void loadStartupStatus(client)
  void resumeSession(client)
}
