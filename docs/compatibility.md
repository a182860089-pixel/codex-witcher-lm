# Compatibility

## Current matrix

| Surface | Architecture | Current evidence | Release gate |
| --- | --- | --- | --- |
| macOS | Apple silicon | Keychain/Launch Services code and package CI target declared; current native run pending | Run native DMG and signed Codex switch/restart/restore smoke test |
| macOS | Intel | Keychain/Launch Services code and package CI target declared; current native run pending | Run the same signed-host smoke test |
| Windows | x86_64 | Current source passes native checks, desktop `cargo check`, Tauri/NSIS build, and isolated install/remove smoke | Run interactive install, live Store Codex, DACL/durability, switch/restore, and lifecycle-cleanup tests |
| Windows | ARM64 | Not claimed in MVP | Add a native runner and signed artifact before support |
| Linux | x86_64 | Web checks and Chromium visual review run here | No Codex Desktop product target |

The supported operating-system floor should follow the current signed Codex
Desktop and Tauri 2 requirements. It must be recorded from real release hosts;
this repository does not freeze a guessed minimum version.

## Provider contract

The MVP supports providers that expose an OpenAI Responses-compatible base URL.
Remote endpoints must use HTTPS. Plain HTTP is accepted only for loopback
development endpoints.

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
override.

## Desktop behavior

Native Codex configuration is the primary path. After applying a profile, fully
quit and reopen Codex before creating a new thread. Existing threads may keep a
previous provider; the helper validates any matching managed provider table,
not only the current global default.

At launch, the app reads the active Codex model, provider, Base URL, and
credential method. Saved connections in the keyless `profiles.json` then offer
a small model picker for the normal quick-switch path. Applying a saved choice
updates `model_provider`, `model`, and the managed provider table in the user's
current configuration. Any user-owned `model_catalog_json` remains untouched;
the internal `models.json` is not installed as its value.

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
cargo check -p codex-provider-switcher-desktop
pnpm tauri build
```

The last command must run natively on macOS Apple silicon, macOS Intel, and
Windows x86_64.

## Native Windows evidence

On 2026-07-23, the current source on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build, Rust
formatting check, 42 core tests, one Credential Manager test, one Windows
launcher test, desktop `cargo check`, the full Tauri/NSIS build, and isolated
NSIS install/remove smoke.

This is native validation and isolated package-smoke evidence, not a release
claim. No artifact hash is frozen yet. The unsigned MVP still needs an
interactive GUI run against the target account's Store Codex package, the
complete inspect/discover/select/save/switch/restart/new-thread/restore flow,
native security-descriptor and crash-durability tests, lifecycle cleanup tests,
and release signing.
