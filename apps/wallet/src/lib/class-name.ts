import { extendTailwindMerge } from "tailwind-merge"

const mergeClassNames = extendTailwindMerge({
  extend: {
    theme: {
      text: ["caption", "body", "title", "headline", "display"],
      spacing: [
        "control-xs",
        "control-sm",
        "control-md",
        "icon-sm",
        "icon-md",
        "icon-lg",
        "scrollbar",
      ],
      ease: ["standard", "enter", "exit"],
      animate: ["enter-forward", "enter-backward", "slide", "sweep"],
    },
    classGroups: {
      duration: [{ duration: ["fast", "base", "slow"] }],
    },
  },
})

export function appendClassName(own: string, extra: string | undefined): string {
  return mergeClassNames(own, extra)
}
