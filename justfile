#!/usr/bin/env -S just --justfile

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]
set shell := ["bash", "-cu"]

_default:
  @just --list -u

alias r := ready
alias t := test
alias f := fmt

# Install the frontend packages, cargo-nextest, cargo-deny, cargo-shear, and cargo-about
init:
  pnpm install
  cargo install --locked cargo-nextest@0.9.148 cargo-deny@0.20.2 cargo-shear@1.14.0
  cargo install --locked --features cli cargo-about@0.9.2

# Run the app with the frontend served by Vite
dev:
  pnpm --filter jelly-wallet tauri dev

# Build the release app with the third-party notices, or pass tauri build args like --target aarch64-apple-darwin or --debug
build *args: notices
  pnpm --filter jelly-wallet tauri build --config src-tauri/tauri.notices.conf.json {{args}}

# CI runs this on every OS and fails if it leaves any file changed.

# Regenerate the icons, then run fmt, clippy, rustdoc, cargo-shear, oxlint, knip, the type checks, the frontend build, the tests, cargo-deny, the third-party notices, and pnpm audit
ready:
  just icons
  just fmt
  just lint
  cargo doc --workspace --no-deps --document-private-items --keep-going --config "build.warnings='deny'"
  cargo shear
  pnpm lint
  pnpm knip
  pnpm --filter @jelly/icons typecheck
  pnpm --filter jelly-wallet build
  just test
  just deny
  just notices
  pnpm audit --prod
  git status

# Run all tests, or narrow them with nextest args like -p jelly_core amount
test *args:
  cargo nextest run --workspace {{args}}

# Format Rust and frontend code
fmt:
  cargo fmt --all
  pnpm fmt

# A crate that fails clippy hides every crate that depends on it, so lints stay warnings
# until the end, where build.warnings fails the run if any were reported.

# Clippy with zero warnings allowed
lint *args:
  cargo clippy --workspace --all-targets --all-features --keep-going --config "build.warnings='deny'" {{args}} -- --cap-lints warn

# Apply oxlint's fixes and suggestions, such as canonical Tailwind classes
lint-fix:
  pnpm lint:fix

# Check advisories, bans, licenses, and sources
deny:
  cargo deny check advisories bans licenses sources

# Write target/THIRD-PARTY-NOTICES.txt, which fails if a crate or npm package the app ships has a license deny.toml doesn't allow
notices:
  pnpm --filter jelly-wallet notices

# Regenerate apps/wallet/src/ipc/bindings.gen.ts after an IPC change
bindings:
  cargo test -p jelly --test it regenerate_bindings -- --ignored

# Regenerate packages/icons/src/icons.gen.tsx after adding or removing an SVG in packages/icons/svg
icons:
  pnpm --filter @jelly/icons generate
