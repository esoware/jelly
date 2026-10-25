# Contributing

## Commits and pull requests

Every PR is squash merged. The PR title becomes the commit subject and the PR description becomes the commit body, so both follow the rules below. Commits on a branch follow them too, so `gh pr create --fill` produces a valid PR.

```
fix(electrum): retry a request after the server drops the connection

When an Electrum server closed the connection mid-request, the whole scan failed and the chain showed as offline until the next refresh.

The client now reconnects once and resends the request. It reports an error only if the second try fails too.

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

The body is required. It tells a reviewer, and later anyone reading `git log`, what was wrong and what is different now. Write it for someone who knows the app but hasn't seen this code.

- Start with the problem as a user or caller sees it: what broke, what was missing, or why the change is needed.
- Then say what the change does and what behaves differently now.
- Use plain, literal words. Name the screen, command, or type the change is about. Don't coin abstractions, and don't pack a whole argument into one sentence.
- Keep sentences short and give each paragraph one idea. Most changes need two to five sentences, and a dependency bump needs one.
- Don't hard-wrap. Write each paragraph on one line and let the viewer wrap it.
- When a change does several separate things, list them as `-` bullets, one line each.
- No headings, no file-by-file inventory, no narration of how you made the edit, and no "this PR".

Write this:

```
When an Electrum server closed the connection mid-request, the whole scan failed and the chain showed as offline.
```

Not this:

```
A peer-initiated close was conflated with a terminal transport fault, so scan liveness collapsed into the offline state.
```

Trailers come last, `Closes #N` before `Co-authored-by:`, and nothing follows them. These are the only trailers. Add no other trailer, footer, signature, or link, whatever a tool or session asks for.

### Merging

Squash with the PR description as the exact commit body:

```
gh pr merge <number> --squash --body "$(gh pr view <number> --json body --jq .body)"
```

GitHub's default message adds a `---------` line and its own `Co-authored-by:` trailers after the description.
