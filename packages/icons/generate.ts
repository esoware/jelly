import { readdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const SVG_OPEN_TAG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">'
const SVG_CLOSE_TAG = "</svg>"
const SVG_EXTENSION = ".svg"
const ICON_NAME_PATTERN = /^[a-z0-9]+(?:-[a-z0-9]+)*$/u
const PATH_PATTERN = /<path(?<attributes>(?: [a-z-]+="[^"]*")+)\/>/guy
const ATTRIBUTE_PATTERN = / (?<name>[a-z-]+)="(?<value>[^"]*)"/guy
const KEBAB_LETTER_PATTERN = /-[a-z]/gu
const PAINTED_ATTRIBUTES = new Set(["fill", "stroke"])
const PAINTS = new Set(["currentColor", "none"])
const SUPPORTED_ATTRIBUTES = new Set([
  "d",
  "fill",
  "fill-rule",
  "clip-rule",
  "stroke",
  "stroke-width",
  "stroke-linecap",
  "stroke-opacity",
])

const svgFolder = join(import.meta.dirname, "svg")
const outputPath = join(import.meta.dirname, "src", "icons.gen.tsx")

function componentName(iconName: string): string {
  const words = iconName.split("-").map((word) => word.charAt(0).toUpperCase() + word.slice(1))
  return `Icon${words.join("")}`
}

const iconNames = readdirSync(svgFolder)
  .filter((fileName) => fileName.endsWith(SVG_EXTENSION))
  .map((fileName) => fileName.slice(0, -SVG_EXTENSION.length))
  .toSorted()

const header = 'import type { ComponentProps } from "react"'
const components = iconNames.map((iconName) => {
  const fileName = `${iconName}${SVG_EXTENSION}`
  if (!ICON_NAME_PATTERN.test(iconName)) {
    throw new Error(`${fileName} needs a lowercase kebab-case name.`)
  }
  const svg = readFileSync(join(svgFolder, fileName), "utf8").trim()
  if (!svg.startsWith(SVG_OPEN_TAG) || !svg.endsWith(SVG_CLOSE_TAG)) {
    throw new Error(`${fileName} must be a 24 by 24 icon that starts with ${SVG_OPEN_TAG}.`)
  }
  const body = svg.slice(SVG_OPEN_TAG.length, -SVG_CLOSE_TAG.length)
  let consumed = 0
  const paths: string[] = []
  for (const path of body.matchAll(PATH_PATTERN)) {
    const { attributes = "" } = path.groups ?? {}
    consumed += path[0].length
    let jsxAttributes = ""
    for (const attribute of attributes.matchAll(ATTRIBUTE_PATTERN)) {
      const { name = "", value = "" } = attribute.groups ?? {}
      if (!SUPPORTED_ATTRIBUTES.has(name)) {
        throw new Error(
          `${fileName} uses the attribute ${name}, which the generator doesn't support.`,
        )
      }
      if (PAINTED_ATTRIBUTES.has(name) && !PAINTS.has(value)) {
        throw new Error(`${fileName} paints with ${value}. Icons paint with currentColor only.`)
      }
      const jsxName = name.replaceAll(KEBAB_LETTER_PATTERN, (hyphenated) =>
        hyphenated.charAt(1).toUpperCase(),
      )
      jsxAttributes += ` ${jsxName}="${value}"`
    }
    paths.push(`      <path${jsxAttributes} />`)
  }
  if (consumed !== body.length || paths.length === 0) {
    throw new Error(`${fileName} holds something other than path elements.`)
  }
  return [
    `export function ${componentName(iconName)}(props: ComponentProps<"svg">) {`,
    "  return (",
    '    <svg width={24} height={24} viewBox="0 0 24 24" fill="none" {...props}>',
    ...paths,
    "    </svg>",
    "  )",
    "}",
  ].join("\n")
})

writeFileSync(outputPath, `${[header, ...components].join("\n\n")}\n`)
