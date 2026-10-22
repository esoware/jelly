import { Popover } from "@base-ui/react/popover"
import { useState } from "react"
import type { ReactNode } from "react"
import { ColorArea, ColorThumb } from "react-aria-components/ColorArea"
import { ColorField, Input, Label } from "react-aria-components/ColorField"
import { ColorPicker as AriaColorPicker, parseColor } from "react-aria-components/ColorPicker"
import { ColorSlider, SliderTrack } from "react-aria-components/ColorSlider"

import { FIELD_LABEL_CLASSES } from "@/ui/field.tsx"
import { POPUP_SIDE_OFFSET_PX, POPUP_TRIGGER_CLASSES } from "@/ui/popup.tsx"
import { TEXT_CONTROL_CLASSES } from "@/ui/textarea.tsx"

const THUMB_CLASSES =
  "top-1/2 box-border size-3.5 rounded-full border-2 border-thumb shadow-[0_0_0_1px_oklch(0%_0_0/25%),0_1px_3px_oklch(0%_0_0/20%)] data-focus-visible:outline-2 data-focus-visible:outline-offset-2 data-focus-visible:outline-fg"

interface ColorPickerProps {
  value: string
  onValueChange: (value: string) => void
  attachment: ReactNode
}

export function ColorPicker({ value, onValueChange, attachment }: ColorPickerProps) {
  const [color, setColor] = useState(() => parseColor(value).toFormat("hsb"))
  const [lastValue, setLastValue] = useState(value)

  if (value !== lastValue) {
    setLastValue(value)
    const next = parseColor(value)
    if (next.toString("hex") !== color.toString("hex")) {
      setColor(next.toFormat("hsb"))
    }
  }

  return (
    <AriaColorPicker
      value={color}
      onChange={(next) => {
        setColor(next.toFormat("hsb"))
        onValueChange(next.toString("hex").toLowerCase())
      }}
    >
      <Popover.Root>
        <Popover.Trigger className={`${POPUP_TRIGGER_CLASSES} h-control-md`}>
          <span className="flex shrink-0 items-center">{attachment}</span>
          <span className="font-mono">{value}</span>
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Positioner
            className="outline-none"
            sideOffset={POPUP_SIDE_OFFSET_PX}
            align="end"
          >
            <Popover.Popup className="flex max-h-(--available-height) w-60 min-w-0 origin-(--transform-origin) motion-pop flex-col gap-3 overflow-x-hidden overflow-y-auto rounded-lg overlay-sm p-3 outline-none app-region-no-drag">
              <ColorArea
                className="aspect-square w-full shrink-0 rounded-md"
                colorSpace="hsb"
                xChannel="saturation"
                yChannel="brightness"
              >
                <ColorThumb className={THUMB_CLASSES} />
              </ColorArea>
              <ColorSlider colorSpace="hsb" channel="hue" aria-label="Hue">
                <SliderTrack className="h-3 w-full rounded-full">
                  <ColorThumb className={THUMB_CLASSES} />
                </SliderTrack>
              </ColorSlider>
              <ColorField className="flex items-center gap-3">
                <Label className={FIELD_LABEL_CLASSES}>Hex</Label>
                <Input className={`${TEXT_CONTROL_CLASSES} h-control-sm px-2`} spellCheck={false} />
              </ColorField>
            </Popover.Popup>
          </Popover.Positioner>
        </Popover.Portal>
      </Popover.Root>
    </AriaColorPicker>
  )
}
