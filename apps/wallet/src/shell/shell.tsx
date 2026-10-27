import logoUrl from "@jelly/branding/logo-colored.svg"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Outlet, useMatches, useNavigate } from "@tanstack/react-router"
import { useEffect, useRef, useState } from "react"
import type { ComponentPropsWithoutRef, ReactNode } from "react"

import type { LockState } from "@/ipc/bindings.gen.ts"
import {
  closeWindow,
  minimizeWindow,
  setWindowTitle,
  startWindowDrag,
  toggleMaximizeWindow,
  watchWindowState,
} from "@/ipc/client.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { useActiveWallet, walletCommands, walletQueries } from "@/query/wallet.ts"
import { StartupFailed, StartupGate } from "@/shell/startup.tsx"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"
import {
  Menu,
  MenuPopup,
  MenuGroup,
  MenuItem,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "@/ui/menu.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { WalletAvatar, walletBadges } from "@/widgets/logos.tsx"

const LOCKED: LockState = { kind: "locked", reason: "requested" }

export function useLockWallet(): (preselectedId: number | null) => void {
  const client = useQueryClient()
  const navigate = useNavigate()

  const lock = useIpcMutation({
    mutationFn: walletCommands.lock,
    errorTitle: "Could not lock the wallet",
  })

  async function lockToUnlockScreen(preselectedId: number | null): Promise<void> {
    try {
      await lock.mutateAsync()
    } catch {
      void client.invalidateQueries({ queryKey: walletQueries.lockState().queryKey })
      return
    }
    client.setQueryData(walletQueries.lockState().queryKey, LOCKED)
    await navigate({
      to: "/unlock",
      search: preselectedId === null ? {} : { walletId: preselectedId },
      replace: true,
    })
  }

  return (preselectedId) => {
    void lockToUnlockScreen(preselectedId)
  }
}

const TOUCH_THROTTLE_MS = 30_000

function useSessionActivity(isUnlocked: boolean): void {
  useEffect(() => {
    if (!isUnlocked) {
      return undefined
    }

    let lastTouchMs = 0
    function handleActivity(): void {
      const nowMs = Date.now()
      if (nowMs - lastTouchMs < TOUCH_THROTTLE_MS) {
        return
      }
      lastTouchMs = nowMs
      walletCommands.touch()
    }

    const options = { passive: true } as const
    window.addEventListener("keydown", handleActivity, options)
    window.addEventListener("pointerdown", handleActivity, options)
    window.addEventListener("wheel", handleActivity, options)

    return () => {
      window.removeEventListener("keydown", handleActivity)
      window.removeEventListener("pointerdown", handleActivity)
      window.removeEventListener("wheel", handleActivity)
    }
  }, [isUnlocked])
}

type CaptionGlyph = "minimize" | "maximize" | "restore" | "close"

const GLYPH_PROPS: ComponentPropsWithoutRef<"svg"> = {
  width: 12,
  height: 12,
  viewBox: "-1 -1 12 12",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1,
}

const GLYPH_SHAPES: Record<CaptionGlyph, ReactNode> = {
  minimize: <line x1="0.5" y1="5" x2="9.5" y2="5" />,
  maximize: <rect x="0.5" y="0.5" width="9" height="9" rx="1.5" />,
  restore: (
    <>
      <path d="M 2.5 2 A 1.5 1.5 0 0 1 4 0.5 H 8 A 1.5 1.5 0 0 1 9.5 2 V 6 A 1.5 1.5 0 0 1 8 7.5" />
      <rect x="0.5" y="2.5" width="7" height="7" rx="1.5" />
    </>
  ),
  close: (
    <>
      <line x1="0.75" y1="0.75" x2="9.25" y2="9.25" />
      <line x1="9.25" y1="0.75" x2="0.75" y2="9.25" />
    </>
  ),
}

interface CaptionButtonProps {
  glyph: CaptionGlyph
  onClick: () => void
}

function CaptionButton({ glyph, onClick }: CaptionButtonProps) {
  const isClose = glyph === "close"

  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex h-full w-11 items-center justify-center text-fg-muted transition-colors ${isClose ? "hover:bg-caption-close hover:text-caption-close-glyph" : "hover:bg-hover hover:text-fg"}`}
    >
      <svg {...GLYPH_PROPS} strokeLinecap={isClose ? "round" : undefined}>
        {GLYPH_SHAPES[glyph]}
      </svg>
    </button>
  )
}

const CLICKABLE_SELECTOR =
  "a, button, input, select, textarea, label, summary, [tabindex]:not([tabindex='-1']), " +
  "[contenteditable]:not([contenteditable='false']), [role='button'], [role='link'], " +
  "[role='menuitem'], [role='tab'], [role='checkbox'], [role='radio'], [role='switch'], " +
  "[role='option']"

function isWithin(rect: DOMRect, event: MouseEvent): boolean {
  return (
    event.clientX >= rect.left &&
    event.clientX < rect.right &&
    event.clientY >= rect.top &&
    event.clientY < rect.bottom
  )
}

function isCoveredDragPress(bar: HTMLElement, event: MouseEvent): boolean {
  const { target } = event
  if (
    !(target instanceof Element) ||
    bar.contains(target) ||
    target.getAttribute("role") !== "presentation" ||
    !isWithin(bar.getBoundingClientRect(), event)
  ) {
    return false
  }
  return ![...bar.querySelectorAll(CLICKABLE_SELECTOR)].some((element) =>
    isWithin(element.getBoundingClientRect(), event),
  )
}

interface TitlebarProps {
  title?: string | null | undefined
  center?: ReactNode | undefined
  right?: ReactNode | undefined
}

function Titlebar({ title, center, right }: TitlebarProps) {
  const [isMaximized, setIsMaximized] = useState(false)
  const [isFullscreen, setIsFullscreen] = useState(false)
  const barRef = useRef<HTMLElement>(null)

  useEffect(
    () =>
      watchWindowState((state) => {
        setIsMaximized(state.isMaximized)
        setIsFullscreen(state.isFullscreen)
      }),
    [],
  )

  useEffect(() => {
    function handleMouseDown(event: MouseEvent): void {
      const bar = barRef.current
      if (
        bar === null ||
        event.button !== 0 ||
        (event.detail !== 1 && event.detail !== 2) ||
        !isCoveredDragPress(bar, event)
      ) {
        return
      }
      event.preventDefault()
      event.stopPropagation()
      if (event.detail === 2) {
        toggleMaximizeWindow()
      } else {
        startWindowDrag()
      }
    }

    window.addEventListener("mousedown", handleMouseDown, true)
    return () => {
      window.removeEventListener("mousedown", handleMouseDown, true)
    }
  }, [])

  const osTitle =
    title === null || title === undefined || title === "" ? "Jelly" : `Jelly - ${title}`

  useEffect(() => {
    setWindowTitle(osTitle)
    document.title = osTitle
  }, [osTitle])

  return (
    <header
      ref={barRef}
      data-tauri-drag-region="deep"
      data-fullscreen={isFullscreen}
      className="relative flex h-8 shrink-0 items-center gap-1 pl-2 select-none app-region-drag in-data-[platform=macos]:data-[fullscreen=false]:pl-20 [&_button]:app-region-no-drag"
    >
      <img
        src={logoUrl}
        alt=""
        className="pointer-events-none size-icon-lg in-data-[platform=macos]:in-data-[fullscreen=false]:hidden"
      />

      {center === null || center === undefined ? null : (
        <div className="absolute top-1/2 left-1/2 max-w-64 -translate-1/2 truncate text-caption text-fg-muted">
          {center}
        </div>
      )}

      <div className="ml-auto flex h-full items-center">
        {right === null || right === undefined ? null : (
          <div className="flex items-center gap-1 pr-2">{right}</div>
        )}
        <div className="flex h-full in-data-[window-controls=system]:hidden">
          <CaptionButton glyph="minimize" onClick={minimizeWindow} />
          <CaptionButton
            glyph={isMaximized ? "restore" : "maximize"}
            onClick={toggleMaximizeWindow}
          />
          <CaptionButton glyph="close" onClick={closeWindow} />
        </div>
      </div>
    </header>
  )
}

interface TitlebarActionsProps {
  isSettings: boolean
}

function TitlebarActions({ isSettings }: TitlebarActionsProps) {
  const navigate = useNavigate()

  return (
    <TooltipText
      content={isSettings ? "Back to the wallet" : "Settings"}
      side="bottom"
      render={
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => {
            void navigate({ to: isSettings ? "/wallet/portfolio" : "/settings" })
          }}
        />
      }
    >
      <Icon name={isSettings ? "back" : "settings"} />
    </TooltipText>
  )
}

function WalletMenu() {
  const navigate = useNavigate()
  const lockWallet = useLockWallet()
  const { wallet, walletId, walletName } = useActiveWallet()
  const wallets = useQuery(walletQueries.all())

  const rows = wallets.data ?? []
  const label = wallet?.name ?? walletName ?? "Wallet"
  const current = walletId === null ? "" : String(walletId)

  return (
    <Menu>
      <MenuTrigger className="flex h-control-sm items-center gap-2 rounded-md px-2 text-left transition-colors hover:bg-hover data-popup-open:bg-active">
        <WalletAvatar color={wallet?.color} />
        <span className="max-w-36 truncate">{label}</span>
        <Icon name="caret" className="size-icon-sm shrink-0 text-fg-faint" />
      </MenuTrigger>

      <MenuPopup align="center">
        <MenuRadioGroup
          label="Wallets"
          value={current}
          onValueChange={(next) => {
            lockWallet(Number(next))
          }}
        >
          {rows.map((row) => (
            <MenuRadioItem key={row.id} value={String(row.id)} badges={walletBadges(row)}>
              <WalletAvatar color={row.color} />
              {row.name}
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>

        <MenuGroup>
          <MenuItem
            onClick={() => {
              void navigate({ to: "/setup" })
            }}
          >
            <Icon name="add" />
            Add wallet
          </MenuItem>
          <MenuItem
            onClick={() => {
              lockWallet(null)
            }}
          >
            <Icon name="lock" />
            Lock wallet
          </MenuItem>
        </MenuGroup>
      </MenuPopup>
    </Menu>
  )
}

export function Shell() {
  const title = useMatches({
    select: (matches) =>
      matches.findLast((match) => match.staticData.title !== undefined)?.staticData.title ?? null,
  })
  const shellKind = useMatches({
    select: (matches) =>
      matches.findLast((match) => match.staticData.shell !== undefined)?.staticData.shell ?? null,
  })
  const { walletName, isUnlocked } = useActiveWallet()

  useSessionActivity(isUnlocked)

  const isInShell = isUnlocked && shellKind !== null

  return (
    <div className="flex h-full flex-col">
      <Titlebar
        title={title ?? walletName}
        center={isInShell ? <WalletMenu /> : title}
        right={isInShell ? <TitlebarActions isSettings={shellKind === "settings"} /> : null}
      />
      <div className="min-h-0 flex-1">
        <StartupGate>
          <Outlet />
        </StartupGate>
      </div>
    </div>
  )
}

interface BootFailedProps {
  message: string
}

export function BootFailed({ message }: BootFailedProps) {
  return (
    <div className="flex h-full flex-col">
      <Titlebar />
      <div className="min-h-0 flex-1">
        <StartupFailed message={message} />
      </div>
    </div>
  )
}
