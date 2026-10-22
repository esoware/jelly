# AGENTS.md

Jelly is a self-custody desktop wallet for the Bitcoin family, EVM chains, Solana, TRON, XRP, TON, and Monero, built with Rust, Tauri 2, and React 19.

## Commands

Use `just`. `just --list` shows every recipe. Run `just ready` before you call a change done.

- CI runs `just ready` on Windows, macOS, and Linux and fails if it leaves a file changed, so commit what it formats.
- There are no frontend tests.
- `just dev` runs the app. Only when the user asks.

## Commits and pull requests

Every commit message, PR title, and PR description follows [CONTRIBUTING.md](CONTRIBUTING.md#commits-and-pull-requests).

## Everywhere

### Money never touches a float

- **Rust** holds amounts in `Amount` and fee rates in `FeeRate`. `Amount::parse` and `Amount::format` are its only decimal conversions.
- **IPC** carries an amount as a string of base units, the `Amount` type in `bindings.gen.ts`.
- **TypeScript** holds amounts and fiat values in `bigint`. `toBaseUnits` and `formatAmount` in `lib/amount.ts` are its only decimal conversions, and `formatFiat` in `lib/fiat.ts` formats fiat values.

### Fix lints instead of silencing them

If a lint really has to go, suppress that one line with a reason:

- **Rust:** `#[expect(lint, reason = "...")]`. Never `#[allow]`, except on a module of generated code such as `jelly_tron`'s `proto.rs`, where which lints fire changes with the generator.
- **TypeScript:** `// oxlint-disable-next-line plugin/rule -- reason`.

### Naming

- Prefer descriptive names. Conventional single-letter names are fine in small scopes or established notation, such as `i` for an index, `f` for a formatter, or `T` for a type parameter.
- Don't use clipped names like `val`, `tmp`, `res`, `buf`, `ctx`, `cfg`, `req`, `resp`, `msg`, `err`, `addr`, `prev`, `len`, or `idx`, in tests either.
- Use these domain abbreviations instead of the long form: `tx`, `txid`, `utxo`, `xpub`, `pubkey`, `rpc`, `url`, `id`, `db`, `params`, `args`, and `props` in React. `tx` always means an on-chain transaction, so channel ends are `sender` and `receiver`.
- A bool starts with `is`, `has`, `should`, or `can`: `is_open` in Rust, `isOpen` in TypeScript.
- A raw integer that carries a unit names it: `burn_sun`, `fee_wei`, or `timeout_millis` in Rust, `TICK_MS` or `unixSeconds` in TypeScript. An `Amount` or `Duration` doesn't need to.
- A field that mirrors a node's API, a SQL column, or a persisted document keeps the name it mirrors.

### Comments

- Plain ASCII in complete sentences. No TODO or FIXME, no step narration, no banners, no history, and no comments about the edit itself.
- **Rust:** default to none, and match how sparse the file already is. Write one only for what the code can't say: a reason, an assumption the code relies on that isn't obvious, a contract, a safety argument, a disk or wire format, or an external fact. Doc comments follow the same rule, so a doc that restates the item's name should not exist.
- **TypeScript and CSS:** no comments at all, except tool directives.

### Copy

Copy is any text a user reads. That means every string the frontend renders, plus every Rust string that can reach the screen, such as `IpcError.message`.

- Sentence case, and ASCII only, except `·` as a separator between values and the currency symbols in `lib/fiat.ts`.
- It never shows a raw wire string, an enum tag, a URL, a file path, or a library's error text.
- Labels and data values take no period. That covers buttons, headings, badges, field names, and the number or name in a detail row.
- Prose, error messages included, is full sentences that end with a period.

### Scope

- Three similar lines beat a premature helper.
- No speculative config and no single-caller helpers.
- Delete code nothing calls.

## IPC

- The webview has no network access. Every remote call goes through Rust, the price feed included. If the frontend needs remote data, add a Rust command.
- No raw URL goes into an error, log line, event, or IPC payload. Use `EndpointLabel`.
- `IpcError.message` is copy. It never carries a secret, a URL, a file path, or a library's error text. The frontend shows it verbatim for `invalid_input` and maps every other code to its own text in `lib/failure.ts`.
- Capabilities in `apps/wallet/src-tauri/capabilities/*.json` are explicit lists. Never add `core:default`.
- A Rust type that the backend only returns derives `Serialize` and not `Deserialize`, so it can't become a command argument by accident. Types the frontend also sends derive both.
- Every command and every field that crosses IPC has a reader in the frontend. If nothing reads it, delete it from Rust.
- `bindings.gen.ts` is generated from the Rust types, doc comments included. Never hand-edit it. Regenerate it after you change a command, an IPC type, or a doc comment on one.

### The seed phrase

- **Rust:** the phrase leaves Rust on its own only once, in the `CreatedWallet` that `create_seed_wallet` returns. Every later read is a password-checked export command in the `secret-exports` capability.
- **TypeScript:** never cache it. Mutations run with `gcTime: 0` in `main.tsx` so the phrase doesn't linger in the mutation cache. Keep it out of queries and stores too.

## Rust

### Crates and modules

- `jelly_core` depends on nothing in the workspace. `jelly_keypair`, `jelly_vault`, `jelly_store`, and `jelly_net` depend on `jelly_core` only, `jelly_mnemonic` on `jelly_core` and `jelly_keypair`, and `jelly_electrum` and `jelly_prices` on `jelly_core` and `jelly_net`.
- A framework depends on `jelly_core`, `jelly_keypair`, `jelly_net`, and `jelly_electrum`, never on a chain crate except as a dev-dependency of its tests.
- A chain crate depends on `jelly_core`, `jelly_keypair`, `jelly_net`, and at most one framework. Never on another chain crate, and never on `jelly_mnemonic`, `jelly_engine`, `jelly_store`, or `jelly_vault`. It exports `SPEC`, `ENTRY`, `build`, and `probe`, and ships its `endpoints.json`. A chain on a framework is thin: everything the Bitcoin-family chains differ by is data in their `UtxoParams` and `UtxoEntry`, and everything the EVM networks differ by is data in their `EvmParams` and `EvmEntry`, never a `match` on the chain.
- `jelly_chains` is the only crate that depends on a chain crate. `jelly_engine` depends on `jelly_core`, `jelly_keypair`, `jelly_mnemonic`, `jelly_store`, and `jelly_vault`, not on `jelly_net` or `jelly_chains` except as a dev-dependency of its tests. The app depends on both and wires them together.
- Only `jelly_engine` may depend on `jelly_store`. Everything else reaches stored data through the engine's operations.
- Only `jelly_net` may depend on reqwest or rustls. reqwest links `rustls-platform-verifier`, which `jelly_net` never constructs, and a comment in `deny.toml` says so. Leave it. Don't switch TLS backends to get rid of it.
- Only `jelly_win32` may use `unsafe`, so it repeats the workspace lints with `unsafe_code` denied rather than forbidden.
- A chain's facts live in its `ChainSpec`, its code in its crate. `ChainId` keeps only its wire string, display name, ticker, decimals, and `is_utxo`, because `Amount`, `AssetMeta`, and the store need those without a table in hand.
- A chain's quirk reaches the engine as a `ChainSpec` field or a `ChainEntry` method with a default, such as `payment_target` or `subaddress`, never as a `match` on `ChainId` in the engine. The engine names a chain only where a storage or IPC field already does.
- Adding a chain is a `ChainId` variant, a chain crate, and an arm in every exhaustive match that lists chains: `jelly_chains`, `ScanStream`, `ScanStream::is_reconstructible`, and `ReservationSlot` in `jelly_core`, the vault-or-database decision in `jelly_engine`, and `Ticker` in `jelly_prices`. Each of those matches is exhaustive so a new chain has to answer it.
- Modules are private, and each crate re-exports its public items from `lib.rs`, so every item has exactly one path. No `pub mod`, except `#[cfg(feature = "test-util")] pub mod test_util;`. `lib.rs` holds the crate doc, the crate attributes, the module list, and the re-exports, and no other code.
- Types that two crates share when neither depends on the other live in `jelly_core`, such as `ProxySetting`, which the engine stores and `jelly_net` routes through, or `EndpointHealth`, which `jelly_net` records and the engine stores. A storage row never derives `specta::Type`, so `jelly_store` has no `ipc` feature.
- The app version is set once, in `[workspace.package]` in the root `Cargo.toml`, and nowhere else: not in a library crate, a client identifier, `tauri.conf.json`, or a `package.json`.

### Types and errors

- An error enum reaches the frontend through one conversion into `ChainError` or `CoreError`, or into `StartupError` during startup, and that conversion names every variant with no wildcard arm. The same goes for a helper that classifies its own enum, such as `kind()`, `outcome()`, or `label()`: list every variant, so a new one has to decide its answer. Where only one variant matters, use `if let` or `matches!` rather than a `match` with a wildcard.
- Declare a closed enum, one whose unit variants each have a fixed string on the wire or in storage, with `jelly_core::wire_enum!` at the visibility it needs, and re-export a public one from `lib.rs`.
- Secrets live in `zeroize` or `secrecy` types, and code reads them through their `with_bytes` closure. A secret type gets no `as_bytes`, no `Serialize`, and a redacted `Debug` with a test that proves it.
- Every spawned task has an owner that can stop it. That's a `TaskTracker` with a `CancellationToken`, a `JoinSet`, or a `JoinHandle` someone holds. No detached `tokio::spawn`.

### Imports

- Group them by hand in three blocks: std, then external crates with `jelly_*` among them, then `crate` and `super`.
- Write one `use jelly_x::{A, B};` per crate per scope.
- rustfmt's grouping options are nightly-only, so keep them out of `rustfmt.toml`.

### Tests

- White-box tests go inline in a `#[cfg(test)] mod tests` at the bottom of the file they test. Public-API tests go in the crate's `tests/` directory. Never make a `src/tests/` directory.
- Helpers that several files' inline tests share live in `#[cfg(test)] mod fixtures;`, declared in `lib.rs` among the other modules. `test_util` is only for the feature-gated helpers other crates' tests use.
- A crate's integration tests are one binary. `tests/it/main.rs` declares a module per file, and shared helpers live in `tests/it/common/mod.rs`. Declare `mod common;`, never `pub mod common;`, and give its items `pub(crate)`. A `pub` module hides a helper nothing calls from the dead-code lint.
- A test that rewrites a committed file is `#[ignore]`d and named `regenerate_*`, so it runs only when asked for with `--ignored`.

## Frontend

- Use Tailwind utilities directly in TSX for ordinary styling, and reuse UI components and their variants rather than copying control styles. Global design tokens, base styles, and shared utilities live in `styles/`. A component stylesheet is only for substantial custom CSS that is clearer than utilities, never a wrapper around `@apply`.
- Compose default classes and caller overrides with `appendClassName` in `lib/class-name.ts`, which merges Tailwind conflicts and recognizes the project design tokens.
- Name class-string constants and variant maps with a `_CLASSES` suffix so Tailwind linting checks their contents. `just lint-fix` applies canonical class suggestions; Oxfmt owns class ordering.
- `ui/` has one primitive per file, and a primitive's own internals live in its file. Don't merge primitives with each other. A primitive knows nothing about the app, and lint stops `ui/` from importing app modules.
- Everywhere else, a file is one screen, one dialog, one panel, or one group of closely related pieces, named for the part of the product it holds. Split a file only past roughly 700 lines, and only along a real seam.
- Export only what another file imports. Everything else stays private to its file.
- `apps/wallet/src/query/` has one file per command group in `apps/wallet/src-tauri/commands.rs`, with the same name, plus `mutation.ts` for `useIpcMutation`.
- Routes are declared in code in `router.tsx`. It holds only route definitions, and every view it mounts lives in `features/` or `shell/`. Only `main.tsx` imports `router.tsx` and `session.ts`.
- Generated TypeScript source files end with `.gen.ts`.
- Brand SVG filenames use the canonical asset display name from `jelly_core` in kebab-case, not its ticker or wire ID.
- Accessibility is not a focus. Use ARIA attributes only when ordinary functionality (like keyboard navigation and more) or a component needs them, never just to make something accessible.
- oxc's React Compiler turns a BigInt literal inside a component or hook into undefined, so put it elsewhere or do it elsehow.
