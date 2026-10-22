import type { ReactNode } from "react"

import type { IpcError } from "@/ipc/client.ts"
import { describeFailure } from "@/lib/failure.ts"
import { Button } from "@/ui/button.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Skeleton } from "@/ui/skeleton.tsx"

export interface ListState {
  isPending: boolean
  error: IpcError | null
  refetch: () => Promise<unknown>
}

interface ListStatusProps {
  state: ListState
  isEmpty: boolean
  errorTitle: string
  emptyTitle: string
  emptyDescription?: ReactNode | undefined
  emptyAction?: ReactNode | undefined
  skeletonRowCount: number
  skeletonClassName?: string | undefined
  isCompact?: boolean | undefined
  emptyClassName?: string | undefined
  children: ReactNode
}

export function ListStatus({
  state,
  isEmpty,
  errorTitle,
  emptyTitle,
  emptyDescription,
  emptyAction,
  skeletonRowCount,
  skeletonClassName,
  isCompact,
  emptyClassName,
  children,
}: ListStatusProps) {
  if (state.isPending) {
    return (
      <div className={isCompact === true ? "space-y-1 p-1" : "space-y-2 p-2"}>
        {Array.from({ length: skeletonRowCount }, (_, i) => (
          <Skeleton key={i} className={skeletonClassName ?? "h-12"} />
        ))}
      </div>
    )
  }
  if (state.error !== null) {
    return (
      <EmptyState
        isCompact={isCompact}
        className={emptyClassName}
        title={errorTitle}
        description={describeFailure(state.error)}
        action={
          <Button
            size="sm"
            variant="secondary"
            onClick={() => {
              void state.refetch()
            }}
          >
            Try again
          </Button>
        }
      />
    )
  }
  if (isEmpty) {
    return (
      <EmptyState
        isCompact={isCompact}
        className={emptyClassName}
        title={emptyTitle}
        description={emptyDescription}
        action={emptyAction}
      />
    )
  }
  return children
}
