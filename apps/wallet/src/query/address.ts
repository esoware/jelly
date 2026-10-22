import { queryOptions, skipToken } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type { ChainId, DerivationPath, ScriptKind } from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

const VALIDATION_GC_MS = 30_000

export const addressKeys = {
  root: ["addresses"] as const,
}

export const addressQueries = {
  list: (chain: ChainId) =>
    queryOptions({
      queryKey: [...addressKeys.root, "list", chain] as const,
      queryFn: () => unwrap(commands.listAddresses(chain)),
    }),

  xpubFor: (addressId: number | null) =>
    queryOptions({
      queryKey: [...addressKeys.root, "xpub", "address", addressId] as const,
      queryFn: addressId === null ? skipToken : () => unwrap(commands.getXpubForAddress(addressId)),
      retry: false,
    }),

  xpubAt: (path: DerivationPath | null) =>
    queryOptions({
      queryKey: [...addressKeys.root, "xpub", "path", path] as const,
      queryFn: path === null ? skipToken : () => unwrap(commands.getXpub(path)),
      retry: false,
    }),

  validate: (chain: ChainId, address: string) =>
    queryOptions({
      queryKey: ["address-validation", chain, address] as const,
      queryFn:
        address.trim() === "" ? skipToken : () => unwrap(commands.validateAddress(chain, address)),
      retry: false,
      gcTime: VALIDATION_GC_MS,
    }),
}

export const addressCommands = {
  openExplorer: (chain: ChainId, address: string): Promise<null> =>
    unwrap(commands.openAddressExplorer(chain, address)),

  add: (chain: ChainId, script: ScriptKind | null, label: string | null): Promise<null> =>
    unwrap(commands.addAddress(chain, script, label)),

  addAt: (
    chain: ChainId,
    path: DerivationPath,
    script: ScriptKind | null,
    label: string | null,
  ): Promise<null> => unwrap(commands.addAddressAt(chain, path, script, label)),

  generate: (chain: ChainId, script: ScriptKind | null, label: string | null): Promise<null> =>
    unwrap(commands.createImportedAddress(chain, script, label)),

  importPrivateKey: (
    chain: ChainId,
    input: string,
    keyPassword: string | null,
    script: ScriptKind | null,
    label: string | null,
  ): Promise<null> => unwrap(commands.importPrivateKey(chain, input, keyPassword, script, label)),

  setLabel: (addressId: number, label: string | null): Promise<null> =>
    unwrap(commands.setAddressLabel(addressId, label)),

  remove: (addressId: number): Promise<null> => unwrap(commands.deleteAddress(addressId)),

  deleteKey: (addressId: number, password: string, confirmation: string): Promise<null> =>
    unwrap(commands.deleteKey(addressId, password, confirmation)),
}
