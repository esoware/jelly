#!/usr/bin/env -S just --justfile

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]
set shell := ["bash", "-cu"]

_default:
  @just --list -u

alias r := ready
alias t := test
alias f := fmt

# Install cargo-nextest and cargo-deny
init:
  cargo install cargo-nextest cargo-deny --locked

# Run fmt, clippy, oxlint, knip, tsc, the tests, and cargo-deny
ready:
  just fmt
  just lint
  pnpm lint
  pnpm knip
  pnpm check
  just test
  just deny
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

# Check advisories, bans, and sources
deny:
  cargo deny check advisories bans sources

# Regenerate apps/wallet/src/ipc/bindings.gen.ts after an IPC change
bindings:
  cargo test -p jelly --test it regenerate_bindings -- --ignored
