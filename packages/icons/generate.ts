import { readdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const SVG_OPEN_TAG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">'
const SVG_CLOSE_TAG = "</svg>"
const SVG_EXTENSION = ".svg"
const ICON_NAME_PATTERN = /^[a-z0-9]+(?:-[a-z0-9]+)*$/u
const SHAPE_PATTERN = /<(?<tag>path|circle|rect)(?<attributes>(?: [a-z-]+="[^"]*")+)\/>/guy
const ATTRIBUTE_PATTERN = / (?<name>[a-z-]+)="(?<value>[^"]*)"/guy
const PAINTED_ATTRIBUTES = new Set(["fill", "stroke"])
const PAINTS = new Set(["currentColor", "none"])
const JSX_ATTRIBUTE_NAMES = new Map([
  ["d", "d"],
  ["cx", "cx"],
  ["cy", "cy"],
  ["r", "r"],
  ["x", "x"],
  ["y", "y"],
  ["width", "width"],
  ["height", "height"],
  ["rx", "rx"],
  ["fill", "fill"],
  ["fill-rule", "fillRule"],
  ["clip-rule", "clipRule"],
  ["opacity", "opacity"],
  ["stroke", "stroke"],
  ["stroke-width", "strokeWidth"],
  ["stroke-linecap", "strokeLinecap"],
  ["stroke-linejoin", "strokeLinejoin"],
  ["stroke-opacity", "strokeOpacity"],
])

const svgFolder = join(import.meta.dirname, "svg")
const outputPath = join(import.meta.dirname, "src", "icons.gen.tsx")

function componentName(iconName: string): string {
  const words = iconName.split("-").map((word) => word.charAt(0).toUpperCase() + word.slice(1))
  return `Icon${words.join("")}`
}

function jsxAttributes(fileName: string, attributes: string): string {
  let consumed = 0
  let jsx = ""
  for (const match of attributes.matchAll(ATTRIBUTE_PATTERN)) {
    const { name = "", value = "" } = match.groups ?? {}
    consumed += match[0].length
    const jsxName = JSX_ATTRIBUTE_NAMES.get(name)
    if (jsxName === undefined) {
      throw new Error(
        `${fileName} uses the attribute ${name}, which the generator doesn't support.`,
      )
    }
    if (PAINTED_ATTRIBUTES.has(name) && !PAINTS.has(value)) {
      throw new Error(`${fileName} paints with ${value}. Icons paint with currentColor only.`)
    }
    jsx += ` ${jsxName}="${value}"`
  }
  if (consumed !== attributes.length) {
    throw new Error(`${fileName} has an attribute the generator can't read.`)
  }
  return jsx
}

function jsxShapes(fileName: string, svg: string): string[] {
  if (!svg.startsWith(SVG_OPEN_TAG) || !svg.endsWith(SVG_CLOSE_TAG)) {
    throw new Error(`${fileName} must be a 24 by 24 icon that starts with ${SVG_OPEN_TAG}.`)
  }
  const body = svg.slice(SVG_OPEN_TAG.length, -SVG_CLOSE_TAG.length)
  let consumed = 0
  const shapes: string[] = []
  for (const match of body.matchAll(SHAPE_PATTERN)) {
    const { tag = "", attributes = "" } = match.groups ?? {}
    consumed += match[0].length
    shapes.push(`      <${tag}${jsxAttributes(fileName, attributes)} />`)
  }
  if (consumed !== body.length || shapes.length === 0) {
    throw new Error(`${fileName} holds something other than path, circle, and rect elements.`)
  }
  return shapes
}

function component(iconName: string): string {
  if (!ICON_NAME_PATTERN.test(iconName)) {
    throw new Error(`${iconName}${SVG_EXTENSION} needs a lowercase kebab-case name.`)
  }
  const fileName = `${iconName}${SVG_EXTENSION}`
  const svg = readFileSync(join(svgFolder, fileName), "utf8").trim()
  return [
    `export function ${componentName(iconName)}(props: ComponentProps<"svg">) {`,
    "  return (",
    '    <svg width={24} height={24} viewBox="0 0 24 24" fill="none" {...props}>',
    ...jsxShapes(fileName, svg),
    "    </svg>",
    "  )",
    "}",
  ].join("\n")
}

const iconNames = readdirSync(svgFolder)
  .filter((fileName) => fileName.endsWith(SVG_EXTENSION))
  .map((fileName) => fileName.slice(0, -SVG_EXTENSION.length))
  .toSorted()

if (new Set(iconNames.map((iconName) => componentName(iconName))).size !== iconNames.length) {
  throw new Error("Two icon names map to the same component name.")
}

const header = 'import type { ComponentProps } from "react"'
const components = iconNames.map((iconName) => component(iconName))
writeFileSync(outputPath, `${[header, ...components].join("\n\n")}\n`)
