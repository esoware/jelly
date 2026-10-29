import { useRender } from "@base-ui/react/use-render"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import type { PointerEvent, ReactNode } from "react"

import { appendClassName } from "@/lib/class-name.ts"
import { Button } from "@/ui/button.tsx"
import { Icon } from "@/ui/icon.tsx"

const DRAG_THRESHOLD_PX = 4

const EDGE_SLACK_PX = 1

const PAGE_FRACTION = 0.75

const ACTIVE_MARGIN_PX = 40

interface Edges {
  hasBefore: boolean
  hasAfter: boolean
}

interface Drag {
  pointerId: number
  startX: number
  startScroll: number
  hasMoved: boolean
}

function edgesOf(viewport: HTMLElement): Edges {
  const end = viewport.scrollWidth - viewport.clientWidth
  return {
    hasBefore: viewport.scrollLeft > EDGE_SLACK_PX,
    hasAfter: viewport.scrollLeft < end - EDGE_SLACK_PX,
  }
}

function settle(current: Edges, next: Edges): Edges {
  return current.hasBefore === next.hasBefore && current.hasAfter === next.hasAfter ? current : next
}

function fitSpacer(spacer: HTMLElement, viewport: HTMLElement): void {
  spacer.style.height = `${String(viewport.scrollWidth - viewport.clientWidth)}px`
}

interface CarouselProps {
  activeKey: string
  className?: string | undefined
  frame?: useRender.RenderProp | undefined
  children: ReactNode
}

export function Carousel({ activeKey, className, frame, children }: CarouselProps) {
  const scrollerRef = useRef<HTMLDivElement>(null)
  const viewportRef = useRef<HTMLDivElement>(null)
  const trackRef = useRef<HTMLDivElement>(null)
  const spacerRef = useRef<HTMLDivElement>(null)
  const mirroredLeftRef = useRef(0)
  const dragRef = useRef<Drag | null>(null)
  const shouldSwallowClickRef = useRef(false)
  const revealedKeyRef = useRef<string | null>(null)
  const hasRevealedActiveRef = useRef(false)
  const [edges, setEdges] = useState<Edges>({ hasBefore: false, hasAfter: false })
  const [isDragging, setIsDragging] = useState(false)

  useEffect(() => {
    const scroller = scrollerRef.current
    const viewport = viewportRef.current
    const track = trackRef.current
    const spacer = spacerRef.current
    if (scroller === null || viewport === null || track === null || spacer === null) {
      return undefined
    }
    const observer = new ResizeObserver(() => {
      scroller.style.height = `${String(viewport.clientWidth)}px`
      scroller.style.marginBottom = `${String(viewport.offsetHeight - viewport.clientWidth)}px`
      fitSpacer(spacer, viewport)
      setEdges((current) => settle(current, edgesOf(viewport)))
    })
    observer.observe(viewport)
    observer.observe(track)
    const target: HTMLDivElement = scroller
    function handleWheel(event: WheelEvent): void {
      if (Math.abs(event.deltaX) <= Math.abs(event.deltaY)) {
        return
      }
      event.preventDefault()
      target.scrollTop += event.deltaX
    }
    scroller.addEventListener("wheel", handleWheel, { passive: false })
    return () => {
      observer.disconnect()
      scroller.removeEventListener("wheel", handleWheel)
    }
  }, [])

  useLayoutEffect(() => {
    const scroller = scrollerRef.current
    const viewport = viewportRef.current
    const spacer = spacerRef.current
    const previous = revealedKeyRef.current
    if (scroller === null || viewport === null || spacer === null || previous === activeKey) {
      return
    }
    revealedKeyRef.current = activeKey
    const hasRevealedActive = hasRevealedActiveRef.current
    const active = viewport.querySelector<HTMLElement>("[data-active]")
    hasRevealedActiveRef.current = active !== null
    if (active === null) {
      return
    }
    const view = viewport.getBoundingClientRect()
    const item = active.getBoundingClientRect()
    const isClear =
      item.left >= view.left + ACTIVE_MARGIN_PX && item.right <= view.right - ACTIVE_MARGIN_PX
    if (isClear) {
      return
    }
    const center = item.left - view.left + item.width / 2
    fitSpacer(spacer, viewport)
    scroller.scrollTo({
      top: scroller.scrollTop + center - view.width / 2,
      behavior: hasRevealedActive ? "smooth" : "instant",
    })
  }, [activeKey])

  function page(direction: 1 | -1): void {
    const scroller = scrollerRef.current
    if (scroller !== null) {
      scroller.scrollBy({
        top: direction * scroller.clientWidth * PAGE_FRACTION,
        behavior: "smooth",
      })
    }
  }

  function handlePointerDown(event: PointerEvent<HTMLDivElement>): void {
    shouldSwallowClickRef.current = false
    if (event.pointerType !== "mouse" || event.button !== 0) {
      return
    }
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startScroll: event.currentTarget.scrollTop,
      hasMoved: false,
    }
  }

  function handlePointerMove(event: PointerEvent<HTMLDivElement>): void {
    const drag = dragRef.current
    if (drag === null || drag.pointerId !== event.pointerId) {
      return
    }
    const distance = event.clientX - drag.startX
    if (!drag.hasMoved) {
      if (Math.abs(distance) < DRAG_THRESHOLD_PX) {
        return
      }
      drag.hasMoved = true
      event.currentTarget.setPointerCapture(event.pointerId)
      setIsDragging(true)
    }
    event.currentTarget.scrollTop = drag.startScroll - distance
  }

  function handlePointerEnd(event: PointerEvent<HTMLDivElement>): void {
    const drag = dragRef.current
    if (drag === null || drag.pointerId !== event.pointerId) {
      return
    }
    dragRef.current = null
    if (drag.hasMoved) {
      shouldSwallowClickRef.current = true
      setIsDragging(false)
    }
  }

  const strip = useRender({
    defaultTagName: "div",
    ...(frame === undefined ? {} : { render: frame }),
    props: {
      className: "relative min-w-0 overflow-clip rounded-full",
      children: (
        <div
          ref={scrollerRef}
          className="relative scrollbar-none overflow-x-hidden overflow-y-auto overscroll-y-contain data-[dragging=true]:cursor-grabbing data-[dragging=true]:select-none"
          data-dragging={isDragging}
          onScroll={(event) => {
            const viewport = viewportRef.current
            if (viewport !== null) {
              viewport.scrollLeft = event.currentTarget.scrollTop
              mirroredLeftRef.current = viewport.scrollLeft
            }
          }}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerEnd}
          onPointerCancel={handlePointerEnd}
          onClickCapture={(event) => {
            if (shouldSwallowClickRef.current) {
              shouldSwallowClickRef.current = false
              event.stopPropagation()
              event.preventDefault()
            }
          }}
        >
          <div
            ref={viewportRef}
            className="sticky top-0 flex overflow-hidden mask-[linear-gradient(to_right,transparent,#000_var(--carousel-fade-before),#000_calc(100%-var(--carousel-fade-after)),transparent)] transition-[--carousel-fade-before,--carousel-fade-after] duration-base ease-standard data-[after=true]:[--carousel-fade-after:--spacing(10)] data-[before=true]:[--carousel-fade-before:--spacing(10)]"
            data-before={edges.hasBefore}
            data-after={edges.hasAfter}
            onScroll={(event) => {
              const viewport = event.currentTarget
              setEdges((current) => settle(current, edgesOf(viewport)))
              const scroller = scrollerRef.current
              if (scroller !== null && viewport.scrollLeft !== mirroredLeftRef.current) {
                scroller.scrollTop = viewport.scrollLeft
              }
            }}
          >
            <div ref={trackRef} className="flex w-max shrink-0">
              {children}
            </div>
          </div>
          <div ref={spacerRef} className="absolute top-full left-0 w-px" />
        </div>
      ),
    },
  })

  return (
    <div className={appendClassName("flex min-w-0 items-center justify-center gap-1", className)}>
      <Button
        variant="ghost"
        size="icon-sm"
        className="size-control-md rounded-full"
        disabled={!edges.hasBefore}
        onClick={() => {
          page(-1)
        }}
      >
        <Icon name="previous" />
      </Button>
      {strip}
      <Button
        variant="ghost"
        size="icon-sm"
        className="size-control-md rounded-full"
        disabled={!edges.hasAfter}
        onClick={() => {
          page(1)
        }}
      >
        <Icon name="next" />
      </Button>
    </div>
  )
}
