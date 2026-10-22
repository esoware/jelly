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

interface CarouselProps {
  activeKey: string
  className?: string | undefined
  frame?: useRender.RenderProp | undefined
  children: ReactNode
}

export function Carousel({ activeKey, className, frame, children }: CarouselProps) {
  const viewportRef = useRef<HTMLDivElement>(null)
  const trackRef = useRef<HTMLDivElement>(null)
  const dragRef = useRef<Drag | null>(null)
  const shouldSwallowClickRef = useRef(false)
  const revealedKeyRef = useRef<string | null>(null)
  const hasRevealedActiveRef = useRef(false)
  const [edges, setEdges] = useState<Edges>({ hasBefore: false, hasAfter: false })
  const [isDragging, setIsDragging] = useState(false)

  useEffect(() => {
    const viewport = viewportRef.current
    const track = trackRef.current
    if (viewport === null || track === null) {
      return undefined
    }
    const observer = new ResizeObserver(() => {
      setEdges((current) => settle(current, edgesOf(viewport)))
    })
    observer.observe(viewport)
    observer.observe(track)
    const scroller: HTMLDivElement = viewport
    function handleWheel(event: WheelEvent): void {
      const canScroll = scroller.scrollWidth > scroller.clientWidth
      if (!canScroll || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) {
        return
      }
      event.preventDefault()
      scroller.scrollLeft += event.deltaY
    }
    viewport.addEventListener("wheel", handleWheel, { passive: false })
    return () => {
      observer.disconnect()
      viewport.removeEventListener("wheel", handleWheel)
    }
  }, [])

  useLayoutEffect(() => {
    const viewport = viewportRef.current
    const previous = revealedKeyRef.current
    if (viewport === null || previous === activeKey) {
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
    viewport.scrollTo({
      left: viewport.scrollLeft + center - view.width / 2,
      behavior: hasRevealedActive ? "smooth" : "instant",
    })
  }, [activeKey])

  function page(direction: 1 | -1): void {
    const viewport = viewportRef.current
    if (viewport !== null) {
      viewport.scrollBy({
        left: direction * viewport.clientWidth * PAGE_FRACTION,
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
      startScroll: event.currentTarget.scrollLeft,
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
    event.currentTarget.scrollLeft = drag.startScroll - distance
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
      className: "relative min-w-0 overflow-hidden rounded-full",
      children: (
        <div
          ref={viewportRef}
          className="flex scrollbar-none overflow-x-auto overflow-y-hidden overscroll-x-contain mask-[linear-gradient(to_right,transparent,#000_var(--carousel-fade-before),#000_calc(100%-var(--carousel-fade-after)),transparent)] transition-[--carousel-fade-before,--carousel-fade-after] duration-base ease-standard data-[after=true]:[--carousel-fade-after:--spacing(10)] data-[before=true]:[--carousel-fade-before:--spacing(10)] data-[dragging=true]:cursor-grabbing data-[dragging=true]:select-none"
          data-before={edges.hasBefore}
          data-after={edges.hasAfter}
          data-dragging={isDragging}
          onScroll={(event) => {
            const viewport = event.currentTarget
            setEdges((current) => settle(current, edgesOf(viewport)))
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
          <div ref={trackRef} className="flex w-max shrink-0">
            {children}
          </div>
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
