import { useEffect, useState } from "react"

import { describeFailure } from "@/lib/failure.ts"
import { MS_PER_SECOND, phraseWords } from "@/lib/format.ts"
import { useSecretScreen } from "@/query/export.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import { Card } from "@/ui/card.tsx"
import {
  Dialog,
  DialogBody,
  DialogClose,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
  DialogTransition,
} from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { CopyButton } from "@/widgets/copyable.tsx"

interface WordGridProps {
  words: readonly string[]
  isBlurred?: boolean | undefined
}

export function WordGrid({ words, isBlurred }: WordGridProps) {
  const entries = words.map((word, i) => ({ position: i + 1, word }))
  return (
    <ol
      className={`grid auto-rows-min grid-cols-4 gap-2 font-mono text-body ${isBlurred === true ? "blur-sm select-none" : ""}`}
    >
      {entries.map((entry) => (
        <li
          key={entry.position}
          className="flex items-baseline gap-2 rounded-md bg-surface px-2 py-1"
        >
          <span className="w-4 shrink-0 text-right text-caption text-fg-faint">
            {entry.position}
          </span>
          <span className="truncate">{entry.word}</span>
        </li>
      ))}
    </ol>
  )
}

const AUTO_CLEAR_SECONDS = 60
const TICK_MS = 250
const PHRASE_MIN_WORDS = 12

interface SecretRevealDialogProps extends DialogStateProps {
  title: string
  warning: string
  isWordGrid?: boolean | undefined
  fetchSecret: (password: string) => Promise<string>
}

export function SecretRevealDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  title,
  warning,
  isWordGrid,
  fetchSecret,
}: SecretRevealDialogProps) {
  const [password, setPassword] = useState("")
  const [secret, setSecret] = useState<string | null>(null)
  const [isVisible, setIsVisible] = useState(false)
  const [isBusy, setIsBusy] = useState(false)
  const [errorMessage, setErrorMessage] = useState<string | null>(null)
  const [remainingSeconds, setRemainingSeconds] = useState(AUTO_CLEAR_SECONDS)

  useSecretScreen()

  useEffect(() => {
    if (secret === null) {
      return undefined
    }
    const deadlineMs = Date.now() + AUTO_CLEAR_SECONDS * MS_PER_SECOND
    const handle = setInterval(() => {
      const secondsLeft = Math.max(0, Math.ceil((deadlineMs - Date.now()) / MS_PER_SECOND))
      setRemainingSeconds(secondsLeft)
      if (secondsLeft === 0) {
        setSecret(null)
        setIsVisible(false)
      }
    }, TICK_MS)
    return () => {
      clearInterval(handle)
    }
  }, [secret])

  async function reveal(): Promise<void> {
    setIsBusy(true)
    setErrorMessage(null)
    try {
      const value = await fetchSecret(password)
      setPassword("")
      setRemainingSeconds(AUTO_CLEAR_SECONDS)
      setSecret(value)
    } catch (error) {
      setErrorMessage(describeFailure(error))
    } finally {
      setIsBusy(false)
    }
  }

  const words = secret === null || isWordGrid !== true ? [] : phraseWords(secret)
  const isGrid = words.length >= PHRASE_MIN_WORDS

  return (
    <Dialog open={open} onOpenChange={onOpenChange} onOpenChangeComplete={onOpenChangeComplete}>
      <DialogPopup>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            Keep this private. Never share it with support staff or enter it on a website.
          </DialogDescription>
        </DialogHeader>

        <DialogTransition step={secret === null ? 0 : 1}>
          {secret === null ? (
            <>
              <DialogBody>
                <Callout tone="danger" heading="Protect your funds">
                  {warning}
                </Callout>
                <Field label="Password" hint="Enter your wallet password to continue.">
                  <PasswordInput
                    autoFocus
                    autoComplete="current-password"
                    value={password}
                    onChange={(e) => {
                      setPassword(e.target.value)
                      setErrorMessage(null)
                    }}
                  />
                </Field>
                {errorMessage === null ? null : (
                  <Callout tone="danger" heading="Could not reveal the secret">
                    {errorMessage}
                  </Callout>
                )}
              </DialogBody>
              <DialogFooter>
                <DialogClose render={<Button variant="secondary" />}>Cancel</DialogClose>
                <Button
                  variant="danger"
                  isLoading={isBusy}
                  disabled={password === ""}
                  onClick={() => {
                    void reveal()
                  }}
                >
                  Reveal
                </Button>
              </DialogFooter>
            </>
          ) : (
            <>
              <DialogBody>
                <Card className="p-3">
                  {isGrid ? (
                    <WordGrid words={words} isBlurred={!isVisible} />
                  ) : (
                    <p
                      className={`font-mono text-body break-all whitespace-pre-wrap ${isVisible ? "" : "blur-sm select-none"}`}
                    >
                      {secret}
                    </p>
                  )}
                </Card>

                <div className="flex items-center justify-between text-caption text-fg-faint">
                  <span className="tabular-nums">
                    {`Hides in ${String(remainingSeconds)} ${remainingSeconds === 1 ? "second" : "seconds"}.`}
                  </span>
                  <div className="flex items-center gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setIsVisible((isShown) => !isShown)
                      }}
                    >
                      <Icon name={isVisible ? "hide" : "reveal"} />
                      {isVisible ? "Hide" : "Show"}
                    </Button>
                    <CopyButton value={secret} size="icon-sm" isSensitive />
                  </div>
                </div>
              </DialogBody>

              <DialogFooter>
                <DialogClose render={<Button variant="secondary" />}>Close</DialogClose>
              </DialogFooter>
            </>
          )}
        </DialogTransition>
      </DialogPopup>
    </Dialog>
  )
}
