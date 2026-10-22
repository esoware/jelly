interface BadgeProps {
  children: string
}

export function Badge({ children }: BadgeProps) {
  return (
    <span className="inline-flex h-4 shrink-0 items-center rounded-full bg-surface px-2 text-caption whitespace-nowrap text-fg-muted">
      {children}
    </span>
  )
}
