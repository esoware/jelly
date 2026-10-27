import type { QueryClient } from "@tanstack/react-query"
import {
  createMemoryHistory,
  createRootRouteWithContext,
  createRoute,
  createRouter,
  redirect,
} from "@tanstack/react-router"

import { AboutView } from "@/features/settings/about.tsx"
import { ApiKeysView } from "@/features/settings/api-keys.tsx"
import { AppearanceView } from "@/features/settings/appearance.tsx"
import { ChainsView } from "@/features/settings/chains.tsx"
import { CurrencyView } from "@/features/settings/currency.tsx"
import { SettingsLayout } from "@/features/settings/layout.tsx"
import { NetworkView } from "@/features/settings/network.tsx"
import { SecurityView } from "@/features/settings/security.tsx"
import { WalletsView } from "@/features/settings/wallets.tsx"
import { SetupView } from "@/features/setup.tsx"
import { UnlockView } from "@/features/unlock.tsx"
import { ChainView } from "@/features/wallet/chain.tsx"
import { ContactsView } from "@/features/wallet/contacts.tsx"
import { WalletLayout, WalletLayoutFallback } from "@/features/wallet/layout.tsx"
import type { WalletView } from "@/features/wallet/layout.tsx"
import { PortfolioView } from "@/features/wallet/portfolio.tsx"
import { isChainId } from "@/lib/chain.ts"
import { appQueries } from "@/query/app.ts"
import { chainQueries } from "@/query/chain.ts"
import { moneyQueries } from "@/query/money.ts"
import { settingQueries } from "@/query/setting.ts"
import { ensureLockState, walletQueries } from "@/query/wallet.ts"
import { ErrorScreen, NotFound } from "@/shell/error-screen.tsx"
import { Shell } from "@/shell/shell.tsx"
import { Splash } from "@/shell/startup.tsx"

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>
  }

  interface StaticDataRouteOption {
    title?: string
    walletView?: WalletView
  }
}

interface RouterContext {
  queryClient: QueryClient
}

interface UnlockSearch {
  walletId?: number
}

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: Shell,
  notFoundComponent: NotFound,
})

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  beforeLoad: async ({ context: { queryClient } }) => {
    const lock = await ensureLockState(queryClient)
    if (lock?.kind === "unlocked") {
      throw redirect({ to: "/wallet", replace: true })
    }
    if (lock === null) {
      throw redirect({ to: "/unlock", replace: true })
    }

    let hasWallets = true
    try {
      const wallets = await queryClient.query(walletQueries.all())
      hasWallets = wallets.length > 0
    } catch {}

    throw redirect({ to: hasWallets ? "/unlock" : "/setup", replace: true })
  },
  pendingMs: 0,
})

const unlockedRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "_unlocked",
  beforeLoad: async ({ context: { queryClient } }) => {
    const lock = await ensureLockState(queryClient)
    if (lock?.kind !== "unlocked") {
      throw redirect({ to: "/unlock", replace: true })
    }
  },
})

const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "setup",
  staticData: { title: "Setup" },
  component: SetupView,
})

const unlockRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "unlock",
  staticData: { title: "Locked" },
  validateSearch: (search: Record<string, unknown>): UnlockSearch => {
    const { walletId } = search
    return typeof walletId === "number" ? { walletId } : {}
  },
  remountDeps: ({ search }) => search,
  beforeLoad: async ({ context: { queryClient } }) => {
    const lock = await ensureLockState(queryClient)
    if (lock?.kind === "unlocked") {
      throw redirect({ to: "/wallet", replace: true })
    }
  },
  loader: async ({ context: { queryClient } }) => {
    try {
      await queryClient.query({ ...walletQueries.all(), staleTime: "static" })
    } catch {}
  },
  component: UnlockView,
})

const walletRoute = createRoute({
  getParentRoute: () => unlockedRoute,
  path: "wallet",
  loader: async ({ context: { queryClient } }) => {
    await Promise.all([
      queryClient.query(chainQueries.enabled()),
      queryClient.query(moneyQueries.totals()),
      queryClient.query(settingQueries.all()),
    ])
  },
  component: WalletLayout,
  pendingComponent: WalletLayoutFallback,
})

const walletIndexRoute = createRoute({
  getParentRoute: () => walletRoute,
  path: "/",
  beforeLoad: async ({ context: { queryClient } }) => {
    const chains = await queryClient.query(chainQueries.enabled())
    if (chains.length === 0) {
      throw redirect({ to: "/wallet/settings/chains", replace: true })
    }
    throw redirect({ to: "/wallet/portfolio", replace: true })
  },
})

const settingsRoute = createRoute({
  getParentRoute: () => walletRoute,
  path: "settings",
  staticData: { walletView: "settings" },
  component: SettingsLayout,
})

const settingsIndexRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: "/wallet/settings/chains", replace: true })
  },
})

const aboutRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "about",
  loader: async ({ context: { queryClient } }) => {
    await Promise.all([
      queryClient.query(appQueries.version()),
      queryClient.query(appQueries.dataDir()),
    ])
  },
  component: AboutView,
})

const apiKeysRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "api-keys",
  loader: ({ context: { queryClient } }) => queryClient.query(settingQueries.all()),
  component: ApiKeysView,
})

const appearanceRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "appearance",
  loader: async ({ context: { queryClient } }) => {
    await Promise.all([
      queryClient.query(settingQueries.all()),
      queryClient.query(appQueries.windowBackdrop()),
    ])
  },
  component: AppearanceView,
})

const chainsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "chains",
  component: ChainsView,
})

const currencyRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "currency",
  loader: ({ context: { queryClient } }) => queryClient.query(settingQueries.all()),
  component: CurrencyView,
})

const networkRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "network",
  loader: ({ context: { queryClient } }) => queryClient.query(settingQueries.all()),
  component: NetworkView,
})

const securityRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "security",
  loader: ({ context: { queryClient } }) => queryClient.query(settingQueries.all()),
  component: SecurityView,
})

const walletsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "wallets",
  component: WalletsView,
})

const chainRoute = createRoute({
  getParentRoute: () => walletRoute,
  path: "$chain",
  params: {
    parse: ({ chain }) => (isChainId(chain) ? { chain } : false),
  },
  staticData: { walletView: "wallet" },
  remountDeps: ({ params }) => params,
  loader: async ({ context: { queryClient }, params: { chain } }) => {
    const [chains] = await Promise.all([
      queryClient.query(chainQueries.enabled()),
      queryClient.query(settingQueries.all()),
    ])
    if (!chains.includes(chain)) {
      throw redirect({ to: "/wallet", replace: true })
    }
  },
  component: ChainView,
})

const contactsRoute = createRoute({
  getParentRoute: () => walletRoute,
  path: "contacts",
  staticData: { walletView: "contacts" },
  component: ContactsView,
})

const portfolioRoute = createRoute({
  getParentRoute: () => walletRoute,
  path: "portfolio",
  staticData: { walletView: "portfolio" },
  component: PortfolioView,
})

const settingsTree = settingsRoute.addChildren([
  aboutRoute,
  apiKeysRoute,
  appearanceRoute,
  chainsRoute,
  currencyRoute,
  networkRoute,
  securityRoute,
  walletsRoute,
  settingsIndexRoute,
])

const routeTree = rootRoute.addChildren([
  indexRoute,
  unlockedRoute.addChildren([
    walletRoute.addChildren([
      chainRoute,
      contactsRoute,
      portfolioRoute,
      settingsTree,
      walletIndexRoute,
    ]),
  ]),
  setupRoute,
  unlockRoute,
])

export function makeRouter(queryClient: QueryClient) {
  return createRouter({
    routeTree,
    context: { queryClient },
    history: createMemoryHistory(),
    defaultPreload: "intent",
    defaultPreloadStaleTime: 0,
    defaultPendingComponent: Splash,
    defaultPendingMinMs: 0,
    defaultErrorComponent: ErrorScreen,
  })
}
