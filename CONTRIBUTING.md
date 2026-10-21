# Contributing to pimbar

Thanks for helping. Bug reports, ideas and pull requests are all welcome: for
anything big, open an issue first so we can talk it through.

## Build and run

You need macOS on Apple silicon, [Rust](https://rustup.rs), and Xcode's command
line tools (`xcode-select --install`).

```sh
cargo install tauri-cli --version "^2" --locked   # once
cargo tauri dev                                    # run it
cargo tauri build                                  # the .app and .dmg
```

Builds go to `target.noindex/` rather than `target/`: Spotlight skips folders
ending in `.noindex`, so a build doesn't show up next to the installed app.

Before sending a pull request:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

By contributing, you agree your work is licensed under the Apache License 2.0,
like the rest of pimbar.

## How it works

pimbar is a [Tauri](https://tauri.app) app: Rust behind, a plain HTML/JS panel
(`ui/`) in front, no frontend build step.

- **Signing in:** pimbar borrows an access token from the Azure CLI
  (`az account get-access-token`), so there's no app registration and it can do
  exactly what the user can. That's also why Entra ID roles and PIM for Groups
  aren't supported: they live in Microsoft Graph, where the Azure CLI's token
  has no PIM permissions.
- **Azure:** it talks to the Azure Resource Manager PIM API directly
  (`src/pim.rs`).
- **Polling:** PIM has no webhooks, so pimbar polls: two small calls a minute
  for requests, approvals and active roles, and the larger eligible-role list
  every 15 minutes, about 1% of what Azure allows one user. Both can be changed
  in Settings, or set to manual. While a task runs it checks every few seconds.
- **Updates:** once a day it reads `latest.json` from the latest GitHub
  release with Tauri's updater (a download, not GitHub's API, whose 60 calls an
  hour per IP a shared network uses up). It updates through Homebrew if
  Homebrew installed it; otherwise the updater installs the update, but only
  if it's signed with the release key.
- **Data:** settings, task history and a cache of role policies live in
  `~/Library/Application Support/dev.pimbar.desktop/`.

### Layout

| | |
|---|---|
| `src/main.rs` | Starting up: menu bar icon, panel window, window events |
| `src/commands.rs` | What the panel can ask: activate, stop, review, settings |
| `src/sync.rs` | The polling loop and the daily update check |
| `src/pim.rs` | The Azure PIM API |
| `src/tasks.rs` | Activations, stops and reviews as tasks, and their history |
| `src/view.rs` | The menu bar icon, notifications, the panel's state |
| `src/icon.rs` | Draws the menu bar icon |
| `src/policies.rs` | Which roles need approval, cached |
| `src/state.rs`, `src/panel.rs`, `src/settings.rs`, `src/login.rs`, `src/az.rs`, `src/update.rs` | State, the panel window, settings, open at login, the Azure CLI token, updates |
| `ui/` | The panel: `index.html`, `app.js`, `style.css` (and `rounded.css` for the rounded look) |

To try a build as an installed app, quit pimbar and copy
`target.noindex/release/bundle/macos/pimbar.app` to `/Applications`. The panel's
files are built into the app, so rebuild after changing `ui/`.

The README's GIFs (`docs/`) show the real panel on made-up data. They're
recorded with tooling that isn't in the repository.

## Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push. The tag has to be
signed with a key in `.github/allowed_signers`:

```sh
git tag -s v0.2.0 -m "pimbar 0.2.0" && git push origin v0.2.0
```

`.github/workflows/release.yml` checks the version, builds the .dmg, publishes
the release and points the Homebrew cask at it. The workflow pushes that cask
change to the default branch, so if that branch is protected, let GitHub
Actions push to it.
