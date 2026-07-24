# Compatibility

## Current matrix

| Surface | Architecture | Current evidence | Release gate |
| --- | --- | --- | --- |
| macOS | Apple silicon | Keychain, Launch Services, tray, LaunchAgent, proxy source, and package CI target are present; no current 0.2.0 native package run | Run signed DMG install, first enable/restart, next-turn switch, background relaunch, disable/restore, and uninstall smoke |
| macOS | Intel | The same source and package target are present; no current 0.2.0 native package run | Run the same signed-host lifecycle |
| Windows | x86_64 | The earlier phase-2 source passed native checks and isolated NSIS smoke; that evidence predates the 0.2.0 proxy and autostart changes | Re-run native tests/build and exercise the full proxy, Store Codex, DACL/durability, upgrade, and cleanup lifecycle |
| Windows | ARM64 | Not claimed in MVP | Add a native runner and signed artifact before support |
| Linux | x86_64 | Development host only; portable proxy unit/integration tests exist in source | No Codex Desktop product target |

The supported operating-system floor should follow the current signed Codex
Desktop and Tauri 2 requirements. It must be recorded from real release hosts;
this repository does not freeze a guessed minimum version.

## Provider contract

The MVP supports providers that expose an OpenAI Responses-compatible base URL.
Remote endpoints must use HTTPS. Plain HTTP is accepted only for loopback
development endpoints. The 0.2.0 proxy forwards HTTP Responses and compact
requests, including SSE bodies; it deliberately advertises
`supports_websockets = false` and does not translate chat-completions or
Responses WebSocket protocols.

Given an unversioned Base URL, the app discovers models from `/v1/models` and
then `/models`; a versioned API path uses its adjacent model endpoint. The
successful endpoint resolves the API base saved into the profile. Discovery
uses system proxy settings, follows no redirects, and accepts at most 2 MiB and
500 model IDs. Users check the models they want in their shortcut, or enter a
model ID manually when a compatible service does not expose a list.

Each profile has one or more explicit model entries. Context window, reasoning
levels, parallel tool calls, and image input are advertised conservatively;
users should enable only capabilities the provider actually implements. A
standard model-list response does not become a Codex `model_catalog_json`
override. In fast-switch mode, the checked entries are rendered as an
authenticated Codex-compatible `/models` response by the loopback proxy.

## Desktop behavior

The default path is native Codex configuration plus the managed loopback
provider. On first enable, fully quit and reopen Codex if the Switcher reports
that it changed the active provider. Once Codex is using `cps-local`, switching
a saved connection or model replaces the proxy Route and applies to the next
turn. Requests already sharing a `thread_id` and `turn_id` remain on their
original Route.

A provider change during an existing thread is not guaranteed to carry
provider-specific response IDs or hidden state across services. Start a new
thread when changing providers unless that combination has been verified.
Direct configuration remains a fallback and always requires a full restart and
a new thread.

At launch, the app reads the active Codex model, provider, Base URL, and
credential method. Saved connections in the keyless `profiles.json` then offer
a small model picker. First proxy enable installs the strict
`http://127.0.0.1:15722/v1` provider through the reversible transaction; later
choices update only the keyless proxy state and in-memory Route. Disabling fast
switching uses the validated backup recorded for that activation, restores
exactly when possible, otherwise removes only verified Switcher-owned proxy
fields while preserving unrelated valid TOML, stops the listener, and disables
background startup. Any user-owned
`model_catalog_json` remains untouched; the internal `models.json` is not
installed as its value.

The closed-source Desktop UI may override or filter custom model values on a
particular release. No build-specific adapter is shipped yet, so this case is
reported as a compatibility gap rather than bypassed with a broad monkey
patch. The external manager still preserves a recovery point and can restore
the exact prior configuration.

## Validation commands

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm test
pnpm web:build
cargo fmt --all -- --check
cargo test -p codex-provider-switcher-core
cargo test -p codex-provider-switcher-credentials
cargo test -p codex-provider-switcher-launcher
cargo test -p codex-provider-switcher-local-proxy
cargo test -p codex-provider-switcher-desktop
pnpm tauri build
```

The last command must run natively on macOS Apple silicon, macOS Intel, and
Windows x86_64.

## Native Windows evidence

On 2026-07-23, the earlier phase-2 source snapshot on Windows 11 x64 build
22631 on `ydy001` passed the TypeScript check, four runtime tests, production
WebView build, Rust formatting check, 42 core tests, one Credential Manager
test, one Windows launcher test, desktop `cargo check`, the full Tauri/NSIS
build, and isolated NSIS install/remove smoke.

That evidence is retained as historical validation of the earlier
configuration path, not as validation of 0.2.0. The current proxy, local model
catalog, tray, automatic startup, first-enable migration, hot Route switching,
background relaunch, and disable/restore lifecycle still require a fresh native
run. No 0.2.0 artifact hash or signed release evidence is recorded.
