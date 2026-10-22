import { queryOptions, useQuery } from "@tanstack/react-query"
import type { QueryClient } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type {
  CreatedWallet,
  LockState,
  PassphraseInput,
  PhraseWords,
  WalletColor,
  WalletRecord,
} from "@/ipc/bindings.gen.ts"
import { fireAndForget, unwrap } from "@/ipc/client.ts"

export const walletKeys = {
  root: ["wallets"] as const,
}

export const walletQueries = {
  all: () =>
    queryOptions({
      queryKey: walletKeys.root,
      queryFn: () => unwrap(commands.listWallets()),
    }),

  lockState: () =>
    queryOptions({
      queryKey: ["session", "lock"] as const,
      queryFn: () => unwrap(commands.getLockState()),
    }),
}

export async function ensureLockState(client: QueryClient): Promise<LockState | null> {
  try {
    return await client.query({ ...walletQueries.lockState(), staleTime: "static" })
  } catch {
    return null
  }
}

export function patchWallet(
  client: QueryClient,
  walletId: number | null,
  update: (wallet: WalletRecord) => WalletRecord,
): void {
  client.setQueryData(walletQueries.all().queryKey, (rows) =>
    rows?.map((row) => (row.id === walletId ? update(row) : row)),
  )
}

export const walletCommands = {
  createSeed: (
    name: string,
    color: WalletColor,
    password: string,
    words: PhraseWords,
    passphrase: PassphraseInput,
  ): Promise<CreatedWallet> =>
    unwrap(commands.createSeedWallet(name, color, password, words, passphrase)),

  importSeed: (
    name: string,
    color: WalletColor,
    password: string,
    phrase: string,
    passphrase: PassphraseInput,
  ): Promise<null> => unwrap(commands.importSeedWallet(name, color, password, phrase, passphrase)),

  unlock: (walletId: number, password: string, passphrase: string | null): Promise<null> =>
    unwrap(commands.unlockWallet(walletId, password, passphrase)),

  lock: (): Promise<null> => unwrap(commands.lockWallet()),

  changePassword: (currentPassword: string, newPassword: string): Promise<null> =>
    unwrap(commands.changePassword(currentPassword, newPassword)),

  rename: (walletId: number, name: string): Promise<null> =>
    unwrap(commands.renameWallet(walletId, name)),

  destroy: (walletId: number, password: string, confirmation: string): Promise<null> =>
    unwrap(commands.deleteWallet(walletId, password, confirmation)),

  touch: (): void => {
    fireAndForget(commands.touchSession())
  },
}

interface ActiveWallet {
  wallet: WalletRecord | null
  walletId: number | null
  walletName: string | null
  isUnlocked: boolean
}

export function useActiveWallet(): ActiveWallet {
  const lock = useQuery(walletQueries.lockState())
  const unlocked = lock.data?.kind === "unlocked" ? lock.data : null
  const walletId = unlocked?.walletId ?? null
  const wallets = useQuery({
    ...walletQueries.all(),
    select: (rows: WalletRecord[]) => rows.find((row) => row.id === walletId) ?? null,
  })

  return {
    wallet: wallets.data ?? null,
    walletId,
    walletName: unlocked?.walletName ?? null,
    isUnlocked: unlocked !== null,
  }
}
