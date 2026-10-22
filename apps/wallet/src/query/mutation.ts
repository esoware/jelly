import { useMutation, useQueryClient } from "@tanstack/react-query"
import type { QueryClient, QueryKey, UseMutationResult } from "@tanstack/react-query"

import { hasCode } from "@/ipc/client.ts"
import type { IpcError } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { notify } from "@/store.ts"

interface IpcMutationOptions<TData, TVariables> {
  mutationFn: (variables: TVariables) => Promise<TData>
  errorTitle: string
  invalidates?: readonly QueryKey[] | ((variables: TVariables) => readonly QueryKey[]) | undefined
  optimistic?: ((client: QueryClient, variables: TVariables) => void) | undefined
  shouldRefreshOnError?: boolean | undefined
  successToast?: string | ((data: TData, variables: TVariables) => string) | undefined
  onDone?: ((data: TData, variables: TVariables) => void) | undefined
}

export function useIpcMutation<TData, TVariables = void>(
  options: IpcMutationOptions<TData, TVariables>,
): UseMutationResult<TData, IpcError, TVariables> {
  const client = useQueryClient()
  const {
    mutationFn,
    errorTitle,
    invalidates,
    optimistic,
    shouldRefreshOnError = false,
    successToast,
    onDone,
  } = options

  function invalidatedKeys(variables: TVariables): readonly QueryKey[] {
    return typeof invalidates === "function" ? invalidates(variables) : (invalidates ?? [])
  }

  function refresh(variables: TVariables): void {
    for (const key of invalidatedKeys(variables)) {
      void client.invalidateQueries({ queryKey: key })
    }
  }

  return useMutation<TData, IpcError, TVariables>({
    mutationFn,
    ...(optimistic === undefined
      ? {}
      : {
          onMutate: async (variables: TVariables) => {
            await Promise.all(
              invalidatedKeys(variables).map((key) => client.cancelQueries({ queryKey: key })),
            )
            optimistic(client, variables)
          },
        }),
    onSuccess: (data, variables) => {
      refresh(variables)
      if (typeof successToast === "string") {
        notify.success(successToast)
      } else if (successToast !== undefined) {
        notify.success(successToast(data, variables))
      }
      onDone?.(data, variables)
    },
    onError: (error, variables) => {
      if (optimistic !== undefined || shouldRefreshOnError) {
        refresh(variables)
      }
      if (hasCode(error, "locked")) {
        notify.warning("The wallet is locked", "Unlock the wallet to continue.")
        return
      }
      notify.failure(errorTitle, describeFailure(error))
    },
  })
}
