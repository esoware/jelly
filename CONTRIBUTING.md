# Contributing

## Commits and pull requests

PRs are squash merged, so the PR title becomes the commit subject and the PR description becomes the commit body.

```
fix(electrum): retry a request after the server drops the connection

When an Electrum server closed the connection mid-request, the whole scan failed and the chain showed as offline until the next refresh.

The client now reconnects once and resends the request before it reports an error.

Closes #12
```

### Title

The title is `type(scope): subject` and fits in 72 characters.

| Type       | For                                                      |
| ---------- | -------------------------------------------------------- |
| `feat`     | Something new a user or a caller can do                  |
| `fix`      | A bug fix                                                |
| `perf`     | A speedup with no change in behavior                     |
| `refactor` | A code change with no change in behavior                 |
| `test`     | Tests only                                               |
| `docs`     | Docs, comments, and agent instructions only              |
| `build`    | Cargo, pnpm, Tauri bundling, and toolchain configuration |
| `ci`       | GitHub workflows and actions                             |
| `chore`    | Anything else, such as dependency bumps                  |
| `revert`   | Undoing an earlier commit                                |

The scope is required. It's a crate's directory name without `jelly_`, such as `core`, `engine`, `evm`, or `bitcoin_cash`, or one of these:

- `wallet` for `apps/wallet/src`
- `app` for `apps/wallet/src-tauri`
- `deps` for dependency bumps
- `repo` for root files and `.github`

Separate several scopes with commas and no spaces, as in `fix(utxo,bitcoin): ...`. A revert reuses the scope and subject of the commit it undoes.

Put `!` before the colon when existing installs have to migrate, such as for a new database schema, vault format, or persisted document: `feat(store)!: ...`.

The subject starts with a lowercase imperative verb, such as `add`, and has no period.

### Description

Say what was wrong or missing, then what the change does about it. A few plain sentences are usually enough, and a list works when a change does several separate things.

Trailers go at the end, `Closes #N` before `Co-authored-by:`.
