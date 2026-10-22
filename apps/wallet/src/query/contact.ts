import { queryOptions } from "@tanstack/react-query"

import { commands } from "@/ipc/bindings.gen.ts"
import type { ChainId, ContactRecord } from "@/ipc/bindings.gen.ts"
import { unwrap } from "@/ipc/client.ts"

const NAME_ORDER = new Intl.Collator(undefined, { sensitivity: "base", numeric: true })

export const contactKeys = {
  root: ["contacts"] as const,
}

export const contactQueries = {
  all: () =>
    queryOptions({
      queryKey: contactKeys.root,
      queryFn: async (): Promise<ContactRecord[]> => {
        const rows = await unwrap(commands.listContacts())
        return rows.toSorted((a, b) => NAME_ORDER.compare(a.name, b.name))
      },
    }),
}

export const contactCommands = {
  add: (chain: ChainId, address: string, name: string, note: string | null): Promise<null> =>
    unwrap(commands.addContact(chain, address, name, note)),

  update: (contactId: number, address: string, name: string, note: string | null): Promise<null> =>
    unwrap(commands.updateContact(contactId, address, name, note)),

  remove: (contactId: number): Promise<null> => unwrap(commands.deleteContact(contactId)),
}
