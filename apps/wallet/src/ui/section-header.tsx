import type { ReactNode } from "react"

const HEADING_TAGS = { page: "h1", section: "h2", subsection: "h3" } as const

const HEADING_CLASSES = {
  page: "text-[1.375rem]/6.5 font-semibold tracking-(--text-display--letter-spacing)",
  section: "text-title",
  subsection: "text-body font-semibold",
} as const satisfies Record<keyof typeof HEADING_TAGS, string>

interface SectionHeaderProps {
  title: string
  level?: keyof typeof HEADING_TAGS | undefined
  description?: ReactNode | undefined
  actions?: ReactNode | undefined
}

export function SectionHeader({
  title,
  level = "section",
  description,
  actions,
}: SectionHeaderProps) {
  const Heading = HEADING_TAGS[level]
  return (
    <div className="flex items-start justify-between gap-4">
      <div className={`flex flex-col ${level === "page" ? "gap-2" : "gap-1"}`}>
        <Heading className={HEADING_CLASSES[level]}>{title}</Heading>
        {description === undefined || description === null ? null : (
          <p className={`text-fg-muted ${level === "page" ? "text-body/5" : "text-caption"}`}>
            {description}
          </p>
        )}
      </div>
      {actions === undefined || actions === null ? null : (
        <div className="flex shrink-0 items-center gap-2">{actions}</div>
      )}
    </div>
  )
}
