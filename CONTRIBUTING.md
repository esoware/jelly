# Contributing

## Commits and pull requests

Every PR is squash merged. The PR title becomes the commit subject and the PR description becomes the commit body, so both follow the format below. Commits on a branch follow it too, so `gh pr create --fill` produces a valid PR.

```
fix(electrum): retry a request after the server drops the connection

A server that closed the socket mid-request failed the whole scan, and
the wallet showed the chain as offline until the next refresh. The
client now reconnects once and resends the request before it reports
an error.

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

The subject starts with a lowercase imperative verb and has no period. Write `add`, not `Add`, `added`, or `adds`.

### Body

The body is required, and one sentence is enough for a dependency bump. Write prose wrapped at 72 columns that says why the change was needed and what behaves differently now. No Markdown headings, lists, file lists, step narration, or "this PR".

Trailers come last, `Closes #N` before `Co-authored-by:`, and nothing follows them.

### Merging

Squash with the PR description as the exact commit body:

```
gh pr merge <number> --squash --body "$(gh pr view <number> --json body --jq .body)"
```

GitHub's default message adds a `---------` line and its own `Co-authored-by:` trailers after the description.
