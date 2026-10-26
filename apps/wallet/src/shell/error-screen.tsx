import { useQueryErrorResetBoundary } from "@tanstack/react-query"
import { Navigate, useRouter } from "@tanstack/react-router"
import type { ErrorComponentProps } from "@tanstack/react-router"
import { useEffect } from "react"

import { describeFailure } from "@/lib/failure.ts"
import { useLockWallet } from "@/shell/shell.tsx"
import { Button } from "@/ui/button.tsx"
import { Card } from "@/ui/card.tsx"
import { Mono } from "@/ui/mono.tsx"
import { CopyButton } from "@/widgets/copyable.tsx"

export function ErrorScreen({ error }: ErrorComponentProps) {
  const router = useRouter()
  const lockWallet = useLockWallet()
  const queryErrors = useQueryErrorResetBoundary()
  const message = describeFailure(error)

  useEffect(() => {
    queryErrors.reset()
  }, [queryErrors])

  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 py-8">
      <div className="flex flex-col items-center gap-1 text-center">
        <p className="text-title">This screen stopped working</p>
        <p className="max-w-sm text-body text-fg-muted">Your keys are unchanged.</p>
      </div>

      <Card className="flex w-full max-w-sm items-start gap-1 p-3">
        <Mono value={message} className="min-w-0 flex-1" />
        <CopyButton value={message} />
      </Card>

      <div className="flex items-center gap-2">
        <Button
          variant="secondary"
          onClick={() => {
            void router.invalidate()
          }}
        >
          Try again
        </Button>
        <Button
          variant="secondary"
          onClick={() => {
            window.location.reload()
          }}
        >
          Reload
        </Button>
        <Button
          variant="danger"
          onClick={() => {
            lockWallet(null)
          }}
        >
          Lock wallet
        </Button>
      </div>
    </div>
  )
}

export function NotFound() {
  return <Navigate to="/" replace />
}
