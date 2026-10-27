import { execFileSync, execSync } from "node:child_process"
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"

interface Section {
  title: string
  intro: string
  components: string[]
  text: string | null
}

interface AboutCrate {
  name: string
  version: string
  license: string | null
  manifest_path: string
}

interface AboutLicense {
  id: string
  text: string
  source_path: string | null
  used_by: { crate: AboutCrate }[]
}

interface PnpmPackage {
  name: string
  license: string
  versions: string[]
  paths: string[]
}

const ACCEPTED_PATTERN = /^accepted = \[(?<list>[^\]]*)\]/mu
const QUOTED_PATTERN = /"(?<value>[^"]*)"/gu
const LICENSE_TOKEN_PATTERN = /[()]|[^\s()]+/gu
const SEVERAL_LICENSES_PATTERN = /\s(?:AND|OR)\s/u
const LICENSE_FILE_PATTERN = /^(?:licen[cs]e|copying)/iu
const LEADING_BLANK_LINES_PATTERN = /^\s*\n/u
const RULE_WIDTH = 80
const PART_RULE = "#".repeat(RULE_WIDTH)
const SECTION_RULE = "=".repeat(RULE_WIDTH)
const TEXT_RULE = "-".repeat(RULE_WIDTH)
const USED_BY = "Used by:"
const STANDARD_TEXT_INTRO =
  "These packages ship no copy of this license, so its standard text follows."
const GPL_INTRO =
  "The GNU Lesser General Public License version 3 incorporates the GNU General Public License version 3, which follows."
const LOGOS_INTRO =
  "The chain and token logos are trademarks of their owners. Jelly shows them only to identify the networks and assets it supports."
const PREAMBLE = [
  "Third-party notices for Jelly",
  "",
  "Jelly includes the third-party software below. Each section names a license, lists the components under it, and then gives the license text.",
  "",
  "Cargo.lock in Jelly's source code pins the source of every Rust crate listed here, and pnpm-lock.yaml pins that of every npm package. Jelly's source code, which can be rebuilt against a modified version of any of these components, is at https://github.com/esoware/jelly.",
].join("\n")

const walletFolder = import.meta.dirname
const repoRoot = join(walletFolder, "..", "..")
const targetFolder = join(repoRoot, "target")
const aboutPath = join(targetFolder, "about.json")
const outputPath = join(targetFolder, "THIRD-PARTY-NOTICES.txt")

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item: unknown) => typeof item === "string")
}

function normalize(text: string): string {
  return text.replaceAll("\r\n", "\n").replace(LEADING_BLANK_LINES_PATTERN, "").trimEnd()
}

function readRepoFile(path: string): string {
  return normalize(readFileSync(join(repoRoot, path), "utf8"))
}

const { list: acceptedList } = ACCEPTED_PATTERN.exec(readRepoFile("about.toml"))?.groups ?? {}
if (acceptedList === undefined) {
  throw new Error("about.toml has no accepted list.")
}
const acceptedLicenses = [...acceptedList.matchAll(QUOTED_PATTERN)].map((match) => {
  const { value = "" } = match.groups ?? {}
  return value
})

function findLicenseText(
  packageFolder: string,
  expression: string,
  license: string,
  component: string,
) {
  const files = readdirSync(packageFolder, { withFileTypes: true })
    .filter(
      (entry) =>
        entry.isFile() && LICENSE_FILE_PATTERN.test(entry.name) && !entry.name.endsWith(".spdx"),
    )
    .map((entry) => ({
      name: entry.name,
      text: normalize(readFileSync(join(packageFolder, entry.name), "utf8")),
    }))
    .filter(({ text }) => text !== "")
  const nameHint = license.split("-")[0]?.toLowerCase() ?? license
  const named = files.find(({ name }) => name.toLowerCase().includes(nameHint))
  if (named !== undefined) {
    return named.text
  }
  if (files.length > 1) {
    throw new Error(`${component} ships several license files, and none of them is for ${license}.`)
  }
  return SEVERAL_LICENSES_PATTERN.test(expression) ? null : (files[0]?.text ?? null)
}

function chooseLicenses(tokens: string[]): string[] | null {
  for (const operator of ["OR", "AND"]) {
    const operands: string[][] = [[]]
    let depth = 0
    for (const token of tokens) {
      if (token === "(") {
        depth += 1
      } else if (token === ")") {
        depth -= 1
      }
      if (depth === 0 && token === operator) {
        operands.push([])
      } else {
        operands.at(-1)?.push(token)
      }
    }
    if (operands.length > 1) {
      const choices = operands.map((operand) => chooseLicenses(operand))
      if (operator === "AND") {
        return choices.every((choice) => choice !== null) ? choices.flat() : null
      }
      const ranked = choices
        .filter((choice) => choice !== null)
        .map((choice) => ({
          choice,
          rank: Math.max(...choice.map((license) => acceptedLicenses.indexOf(license))),
        }))
      return ranked.toSorted((left, right) => left.rank - right.rank)[0]?.choice ?? null
    }
  }
  if (tokens[0] === "(") {
    return chooseLicenses(tokens.slice(1, -1))
  }
  const license = tokens.join(" ")
  return acceptedLicenses.includes(license) ? [license] : null
}

function addComponent(
  sections: Map<string, Section>,
  section: Omit<Section, "components">,
  component: string,
) {
  const key = [section.title, section.intro, section.text ?? ""].join("\0")
  const existing = sections.get(key)
  if (existing === undefined) {
    sections.set(key, { ...section, components: [component] })
  } else if (!existing.components.includes(component)) {
    existing.components.push(component)
  }
}

function sortSections(sections: Map<string, Section>): Section[] {
  return [...sections.values()]
    .map((section) => ({
      section,
      key: [section.title, section.components.toSorted()[0] ?? "", section.text ?? ""].join("\0"),
    }))
    .toSorted((left, right) => (left.key < right.key ? -1 : 1))
    .map(({ section }) => section)
}

function renderPart(title: string, sections: Section[]): string {
  return [
    [PART_RULE, title, PART_RULE].join("\n"),
    ...sections.map((section) => {
      const listing = [
        SECTION_RULE,
        "",
        section.title,
        "",
        section.intro,
        ...section.components.toSorted().map((component) => `    ${component}`),
      ]
      return (section.text === null ? listing : [...listing, "", TEXT_RULE, "", section.text]).join(
        "\n",
      )
    }),
  ].join("\n\n")
}

mkdirSync(targetFolder, { recursive: true })
execFileSync(
  "cargo",
  [
    "about",
    "generate",
    "--format",
    "json",
    "--workspace",
    "--locked",
    "--fail",
    "--output-file",
    aboutPath,
  ],
  { cwd: repoRoot, stdio: "inherit" },
)
const about: unknown = JSON.parse(readFileSync(aboutPath, "utf8"))
if (
  typeof about !== "object" ||
  about === null ||
  !("licenses" in about) ||
  !Array.isArray(about.licenses) ||
  !about.licenses.every(
    (entry: unknown): entry is AboutLicense =>
      typeof entry === "object" &&
      entry !== null &&
      "id" in entry &&
      typeof entry.id === "string" &&
      "text" in entry &&
      typeof entry.text === "string" &&
      "source_path" in entry &&
      (entry.source_path === null || typeof entry.source_path === "string") &&
      "used_by" in entry &&
      Array.isArray(entry.used_by) &&
      entry.used_by.every(
        (user: unknown) =>
          typeof user === "object" &&
          user !== null &&
          "crate" in user &&
          typeof user.crate === "object" &&
          user.crate !== null &&
          "name" in user.crate &&
          typeof user.crate.name === "string" &&
          "version" in user.crate &&
          typeof user.crate.version === "string" &&
          "license" in user.crate &&
          (user.crate.license === null || typeof user.crate.license === "string") &&
          "manifest_path" in user.crate &&
          typeof user.crate.manifest_path === "string",
      ),
  )
) {
  throw new Error("cargo about wrote JSON in a shape this script doesn't know.")
}

const standardTexts = new Map<string, string>()
const crateSections = new Map<string, Section>()
for (const entry of about.licenses) {
  const text = normalize(entry.text)
  if (entry.source_path === null) {
    standardTexts.set(entry.id, text)
  }
  for (const { crate } of entry.used_by) {
    const component = `${crate.name} ${crate.version}`
    const fileText =
      entry.source_path === null
        ? findLicenseText(dirname(crate.manifest_path), crate.license ?? "", entry.id, component)
        : text
    addComponent(
      crateSections,
      {
        title: entry.id,
        intro: fileText === null ? STANDARD_TEXT_INTRO : USED_BY,
        text: fileText ?? text,
      },
      component,
    )
  }
}

const packagesByLicense: unknown = JSON.parse(
  execSync("pnpm licenses list --prod --json", { cwd: walletFolder, encoding: "utf8" }),
)
if (typeof packagesByLicense !== "object" || packagesByLicense === null) {
  throw new Error("pnpm licenses list printed JSON that isn't an object.")
}
const packages: unknown[] = Object.values(packagesByLicense).flat()
if (
  !packages.every(
    (entry): entry is PnpmPackage =>
      typeof entry === "object" &&
      entry !== null &&
      "name" in entry &&
      typeof entry.name === "string" &&
      "license" in entry &&
      typeof entry.license === "string" &&
      "versions" in entry &&
      isStringArray(entry.versions) &&
      "paths" in entry &&
      isStringArray(entry.paths) &&
      entry.paths.length === entry.versions.length,
  )
) {
  throw new Error("pnpm licenses list printed a package in a shape this script doesn't know.")
}
const packageSections = new Map<string, Section>()
for (const { name, license: expression, versions, paths } of packages) {
  const chosen = chooseLicenses(expression.match(LICENSE_TOKEN_PATTERN) ?? [])
  if (chosen === null) {
    throw new Error(`${name} is under ${expression}, which about.toml doesn't accept.`)
  }
  for (const [index, path] of paths.entries()) {
    const component = `${name} ${versions[index] ?? ""}`
    for (const license of chosen) {
      const fileText = findLicenseText(path, expression, license, component)
      const text = fileText ?? standardTexts.get(license)
      if (text === undefined) {
        throw new Error(
          `${component} ships no copy of ${license}, and no crate needs its standard text.`,
        )
      }
      addComponent(
        packageSections,
        { title: license, intro: fileText === null ? STANDARD_TEXT_INTRO : USED_BY, text },
        component,
      )
    }
  }
}

const otherSections: Section[] = [
  {
    title: "OFL-1.1",
    intro: USED_BY,
    components: ["Clarity City font"],
    text: readRepoFile("apps/wallet/src/assets/fonts/clarity-city-license.txt"),
  },
  {
    title: "OFL-1.1",
    intro: USED_BY,
    components: ["Cascadia Mono font"],
    text: readRepoFile("apps/wallet/src/assets/fonts/cascadia-mono-license.txt"),
  },
  {
    title: "LGPL-3.0-only",
    intro: USED_BY,
    components: ["TRON protocol definitions from java-tron GreatVoyage-v4.8.2"],
    text: readRepoFile("crates/chains/jelly_tron/proto/COPYING.LESSER"),
  },
  {
    title: "GPL-3.0-only",
    intro: GPL_INTRO,
    components: [],
    text: readRepoFile("crates/chains/jelly_tron/proto/COPYING"),
  },
  { title: "Chain and token logos", intro: LOGOS_INTRO, components: [], text: null },
]

const notices = [
  PREAMBLE,
  renderPart("Rust crates", sortSections(crateSections)),
  renderPart("npm packages", sortSections(packageSections)),
  renderPart("Other components", otherSections),
]
writeFileSync(outputPath, `${notices.join("\n\n")}\n`)
