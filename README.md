# Codex Provider Switcher

An unofficial, clean-room companion for selecting an OpenAI
Responses-compatible provider and model for Codex on macOS and Windows.

It is designed for people who do not want to edit TOML. The app reads the
current Codex provider, model, service address, and credential method on
launch. To add a connection, enter a Base URL and API Key, fetch the available
models, check the models you want to keep, then save or save-and-switch. A
manual model ID remains available for compatible services that do not return a
model list.

Saved connections provide a small model picker for later one-click switching.
The default 0.2.0 mode runs a local provider on `127.0.0.1`: the first enable
transactionally points Codex at that provider and may require one full Codex
restart. After it is active, selecting another saved connection or model
atomically changes the upstream route for the next turn without rewriting
Codex configuration. When both identifiers are available, requests from the
same thread and turn stay on the route with which they started.

The Switcher remains open in the system tray while fast switching is enabled
and registers itself for background login startup. Direct configuration remains
available as a fallback; direct switches still require a full Codex restart and
a new thread.

Saved metadata lives in a keyless `profiles.json`; API keys and the separate
local-proxy entry token stay in macOS Keychain or Windows Credential Manager.

The app never modifies the signed Codex Desktop package. A narrowly scoped CDP
adapter exists only as an opt-in compatibility layer for reviewed Desktop
builds; unknown builds fail closed.

## Status

Version 0.2.0 adds the loopback proxy, local model catalog, atomic route
switching, turn pinning, tray lifecycle, and background startup implementation,
with portable proxy unit and integration tests in the source tree.

The native Windows checks and NSIS install/remove evidence recorded in the
documentation belong to the earlier phase-2 source snapshot; they do not prove
the new 0.2.0 proxy lifecycle. This version is not release-ready until the
complete checks in `docs/release.md` pass on current macOS and Windows builds,
including packaged proxy startup, real Codex traffic, disable/restore, upgrade,
uninstall cleanup, signing, and notarization.

## What happens when you connect

1. The app tries the standard `GET /v1/models` endpoint, then `GET /models`
   when the supplied Base URL does not already identify a versioned API path.
2. The successful endpoint determines the resolved API base that will be saved.
3. You check the models to keep, or add a model ID manually.
4. The app moves the temporary API Key into the operating-system keyring and
   writes only keyless shortcut metadata to `profiles.json`.
5. In the default fast-switch mode, the app starts an authenticated loopback
   proxy and transactionally configures the managed `cps-local` provider the
   first time. Restart Codex when the app asks.
   That activation receives its own recovery point; closing fast switching
   restores that validated point exactly when neither managed file changed.
   If only unrelated valid TOML changed, it removes the Switcher-owned proxy
   fields with a compare-and-swap write and preserves those later edits.
   Changes to Switcher-owned fields or unreadable recovery data fail closed
   for manual review.
6. Later choices replace the active in-memory route. The proxy overwrites the
   request model, injects the selected upstream bearer, and streams the
   Responses API result back to Codex. A new choice applies on the next turn;
   start a new thread when changing providers if their histories are not
   compatible.

Discovery uses the operating system's proxy settings, rejects remote plain
HTTP, follows no redirects, and limits responses to 2 MiB and 500 models. API
keys retained between discovery and save live in a zeroizing in-process vault
for no more than ten minutes, with at most eight sessions.

The local proxy serves authenticated `/models` and `/v1/models` responses from
the checked models, but the Switcher does not add `model_catalog_json` and
preserves any value the user already owns. Its internal `models.json`
participates in safe apply/restore; Codex configuration is not pointed at that
file.

The fallback direct mode writes the explicit upstream `model_provider` and
`model` through the same hash-checked backup transaction. It does not provide
hot switching.

## Development

Prerequisites: Node.js 22+, pnpm 10+, stable Rust, and the Tauri 2 platform
prerequisites for the current operating system.

```bash
pnpm install
pnpm check
pnpm test
cargo fmt --all -- --check
cargo test -p codex-provider-switcher-core \
  -p codex-provider-switcher-credentials \
  -p codex-provider-switcher-launcher \
  -p codex-provider-switcher-local-proxy \
  -p codex-provider-switcher-desktop
pnpm tauri dev
```

See [docs/architecture.md](docs/architecture.md),
[docs/security.md](docs/security.md), and
[docs/compatibility.md](docs/compatibility.md) before changing integration
behavior.

CC Switch is an MIT-licensed behavioral reference for provider and switching
UX. This repository does not wrap CC Switch and contains no copied CC Switch
code or assets; see [NOTICE.md](NOTICE.md).

## Non-affiliation

Codex and OpenAI are trademarks of their respective owners. This project is
not sponsored, endorsed, or supported by OpenAI.
