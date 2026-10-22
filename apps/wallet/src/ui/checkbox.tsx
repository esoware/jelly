import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox"

import { Icon } from "@/ui/icon.tsx"

interface CheckboxProps {
  checked: boolean
  onCheckedChange: (checked: boolean) => void
}

export function Checkbox({ checked, onCheckedChange }: CheckboxProps) {
  return (
    <BaseCheckbox.Root
      checked={checked}
      onCheckedChange={(next) => {
        onCheckedChange(next)
      }}
      className="flex size-4 shrink-0 items-center justify-center rounded-sm border border-edge bg-surface -outline-offset-1 transition-colors hover:bg-hover data-checked:bg-active"
    >
      <BaseCheckbox.Indicator className="size-icon-sm text-fg" render={<Icon name="check" />} />
    </BaseCheckbox.Root>
  )
}
