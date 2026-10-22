import { Switch as BaseSwitch } from "@base-ui/react/switch"

interface SwitchProps {
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  disabled?: boolean | undefined
}

export function Switch({ checked, onCheckedChange, disabled }: SwitchProps) {
  return (
    <BaseSwitch.Root
      checked={checked}
      disabled={disabled}
      onCheckedChange={(next) => {
        onCheckedChange(next)
      }}
      className="inline-flex h-5 w-9 shrink-0 rounded-full border border-edge bg-surface p-0.5 -outline-offset-1 transition-colors hover:bg-hover data-checked:bg-active data-disabled:pointer-events-none data-disabled:opacity-40"
    >
      <BaseSwitch.Thumb className="pointer-events-none block aspect-square h-full shrink-0 rounded-full bg-fg-faint transition data-checked:translate-x-4 data-checked:bg-fg" />
    </BaseSwitch.Root>
  )
}
