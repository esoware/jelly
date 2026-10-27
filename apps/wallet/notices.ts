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

const ALLOWED_LICENSES = [
  "MIT",
  "Apache-2.0",
  "0BSD",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "BSL-1.0",
  "CC0-1.0",
  "CDLA-Permissive-2.0",
  "ISC",
  "MITNFA",
  "MPL-2.0",
  "Unicode-3.0",
  "Unlicense",
  "Zlib",
]
const BUNDLED_DEV_PACKAGES = new Set(["tailwindcss"])
const BUNDLED_WORKS = [
  {
    title: "OFL-1.1",
    component: "Clarity City font",
    licensePath: "apps/wallet/src/assets/fonts/clarity-city-license.txt",
  },
  {
    title: "OFL-1.1",
    component: "Cascadia Mono font",
    licensePath: "apps/wallet/src/assets/fonts/cascadia-mono-license.txt",
  },
  {
    title: "LGPL-3.0-only",
    component: "TRON protocol definitions from java-tron GreatVoyage-v4.8.2",
    licensePath: "crates/chains/jelly_tron/proto/COPYING.LESSER",
  },
]
const GPL_PATH = "crates/chains/jelly_tron/proto/COPYING"
const LICENSE_FILE_PATTERN = /^(?:licen[cs]e|copying)/iu
const LEADING_BLANK_LINES_PATTERN = /^\s*\n/u
const RULE_WIDTH = 80
const PART_RULE = "#".repeat(RULE_WIDTH)
const SECTION_RULE = "=".repeat(RULE_WIDTH)
const TEXT_RULE = "-".repeat(RULE_WIDTH)
const USED_BY = "Used by:"
const STANDARD_TEXT_INTRO =
  "These crates ship no license file, so the standard text of their license follows."
const NO_TEXT_INTRO = "These packages ship no license text."
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

function isAboutCrate(value: unknown): value is AboutCrate {
  return (
    typeof value === "object" &&
    value !== null &&
    "name" in value &&
    typeof value.name === "string" &&
    "version" in value &&
    typeof value.version === "string" &&
    "manifest_path" in value &&
    typeof value.manifest_path === "string"
  )
}

function isAboutLicense(value: unknown): value is AboutLicense {
  return (
    typeof value === "object" &&
    value !== null &&
    "id" in value &&
    typeof value.id === "string" &&
    "text" in value &&
    typeof value.text === "string" &&
    "source_path" in value &&
    (value.source_path === null || typeof value.source_path === "string") &&
    "used_by" in value &&
    Array.isArray(value.used_by) &&
    value.used_by.every(
      (user: unknown) =>
        typeof user === "object" && user !== null && "crate" in user && isAboutCrate(user.crate),
    )
  )
}

function isPnpmPackage(value: unknown): value is PnpmPackage {
  return (
    typeof value === "object" &&
    value !== null &&
    "name" in value &&
    typeof value.name === "string" &&
    "license" in value &&
    typeof value.license === "string" &&
    "versions" in value &&
    isStringArray(value.versions) &&
    "paths" in value &&
    isStringArray(value.paths) &&
    value.paths.length === value.versions.length
  )
}

function normalize(text: string): string {
  return text.replaceAll("\r\n", "\n").replace(LEADING_BLANK_LINES_PATTERN, "").trimEnd()
}

function readRepoFile(path: string): string {
  return normalize(readFileSync(join(repoRoot, path), "utf8"))
}

function findLicenseText(packageFolder: string, license: string, component: string) {
  const candidates = readdirSync(packageFolder, { withFileTypes: true })
    .filter(
      (entry) =>
        entry.isFile() && LICENSE_FILE_PATTERN.test(entry.name) && !entry.name.endsWith(".spdx"),
    )
    .map((entry) => entry.name)
  if (candidates.length === 0) {
    return null
  }
  const nameHint = license.split("-")[0]?.toLowerCase() ?? license
  const fileName =
    candidates.length === 1
      ? candidates[0]
      : candidates.find((candidate) => candidate.toLowerCase().includes(nameHint))
  if (fileName === undefined) {
    throw new Error(`${component} ships several license files, and none of them is for ${license}.`)
  }
  return normalize(readFileSync(join(packageFolder, fileName), "utf8"))
}

function chooseLicense(expression: string, component: string): string {
  const alternatives = new Set(expression.replaceAll(/[()]/gu, "").split(" OR "))
  const license = ALLOWED_LICENSES.find((candidate) => alternatives.has(candidate))
  if (license === undefined) {
    throw new Error(`${component} is under ${expression}, which deny.toml doesn't allow.`)
  }
  return license
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

function sortKey({ title, components, text }: Section): string {
  return [title, components.toSorted()[0] ?? "", text ?? ""].join("\0")
}

function sortSections(sections: Map<string, Section>): Section[] {
  return [...sections.values()].toSorted((left, right) => (sortKey(left) < sortKey(right) ? -1 : 1))
}

function crateSections(): Section[] {
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
    !about.licenses.every((entry: unknown) => isAboutLicense(entry))
  ) {
    throw new Error("cargo about wrote JSON in a shape this script doesn't know.")
  }
  const licenses: AboutLicense[] = about.licenses
  const sections = new Map<string, Section>()
  for (const entry of licenses) {
    for (const { crate } of entry.used_by) {
      const component = `${crate.name} ${crate.version}`
      const text =
        entry.source_path === null
          ? findLicenseText(dirname(crate.manifest_path), entry.id, component)
          : normalize(entry.text)
      addComponent(
        sections,
        {
          title: entry.id,
          intro: text === null ? STANDARD_TEXT_INTRO : USED_BY,
          text: text ?? normalize(entry.text),
        },
        component,
      )
    }
  }
  return sortSections(sections)
}

function listPackages(command: string): PnpmPackage[] {
  const groups: unknown = JSON.parse(execSync(command, { cwd: walletFolder, encoding: "utf8" }))
  if (typeof groups !== "object" || groups === null) {
    throw new Error(`${command} printed JSON in a shape this script doesn't know.`)
  }
  const packages: unknown[] = Object.values(groups).flat()
  if (!packages.every((entry) => isPnpmPackage(entry))) {
    throw new Error(`${command} printed JSON in a shape this script doesn't know.`)
  }
  return packages
}

function packageSections(): Section[] {
  const packages = [
    ...listPackages("pnpm licenses list --prod --json"),
    ...listPackages("pnpm licenses list --json").filter(({ name }) =>
      BUNDLED_DEV_PACKAGES.has(name),
    ),
  ]
  const sections = new Map<string, Section>()
  for (const { name, license, versions, paths } of packages) {
    for (const [index, path] of paths.entries()) {
      const component = `${name} ${versions[index] ?? ""}`
      const chosen = chooseLicense(license, component)
      const text = findLicenseText(path, chosen, component)
      addComponent(
        sections,
        { title: chosen, intro: text === null ? NO_TEXT_INTRO : USED_BY, text },
        component,
      )
    }
  }
  return sortSections(sections)
}

function otherSections(): Section[] {
  return [
    ...BUNDLED_WORKS.map(({ title, component, licensePath }) => ({
      title,
      intro: USED_BY,
      components: [component],
      text: readRepoFile(licensePath),
    })),
    { title: "GPL-3.0-only", intro: GPL_INTRO, components: [], text: readRepoFile(GPL_PATH) },
    { title: "Chain and token logos", intro: LOGOS_INTRO, components: [], text: null },
  ]
}

function renderSection({ title, intro, components, text }: Section): string {
  const listing = [
    SECTION_RULE,
    "",
    title,
    "",
    intro,
    ...components.toSorted().map((component) => `    ${component}`),
  ]
  return (text === null ? listing : [...listing, "", TEXT_RULE, "", text]).join("\n")
}

function renderPart(title: string, sections: Section[]): string {
  return [
    [PART_RULE, title, PART_RULE].join("\n"),
    ...sections.map((section) => renderSection(section)),
  ].join("\n\n")
}

const notices = [
  PREAMBLE,
  renderPart("Rust crates", crateSections()),
  renderPart("npm packages", packageSections()),
  renderPart("Other components", otherSections()),
]
writeFileSync(outputPath, `${notices.join("\n\n")}\n`)
