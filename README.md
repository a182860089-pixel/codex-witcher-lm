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
Their metadata lives in a keyless `profiles.json`; API keys stay in macOS
Keychain or Windows Credential Manager. Applying a model edits the user's
existing Codex configuration through the hash-checked backup transaction and
affects new threads after Codex is fully restarted.

The app never modifies the signed Codex Desktop package. A narrowly scoped CDP
adapter exists only as an opt-in compatibility layer for reviewed Desktop
builds; unknown builds fail closed.

## Status

This repository contains the novice connection, model-discovery, saved-profile,
quick-switch, recovery, and native package foundations. The current source has
passed TypeScript checking, four runtime tests, the production Web build, Rust
formatting, 42 core tests, one credential test, one launcher test, a native
Windows desktop `cargo check`, the refreshed Tauri/NSIS build, and an isolated
NSIS install/remove smoke.

It is not release-ready: native macOS package evidence, interactive packaged
behavior, native validation of credential-helper caller checks, Windows
security metadata, lifecycle cleanup, signing, and live Codex integration
still require the gates in `docs/release.md`.

## What happens when you connect

1. The app tries the standard `GET /v1/models` endpoint, then `GET /models`
   when the supplied Base URL does not already identify a versioned API path.
2. The successful endpoint determines the resolved API base that will be saved.
3. You check the models to keep, or add a model ID manually.
4. The app moves the temporary API Key into the operating-system keyring and
   writes only keyless shortcut metadata to `profiles.json`.
5. Save-and-switch writes the explicit `model_provider` and `model` selection
   while preserving unrelated Codex configuration.

Discovery uses the operating system's proxy settings, rejects remote plain
HTTP, follows no redirects, and limits responses to 2 MiB and 500 models. API
keys retained between discovery and save live in a zeroizing in-process vault
for no more than ten minutes, with at most eight sessions.

The Switcher does not add `model_catalog_json` and preserves any value the user
already owns. Its internal `models.json` participates in safe apply/restore;
Codex configuration is not pointed at that file.

## Development

Prerequisites: Node.js 22+, pnpm 10+, stable Rust, and the Tauri 2 platform
prerequisites for the current operating system.

```bash
pnpm install
pnpm check
pnpm test
cargo test -p codex-provider-switcher-core
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
