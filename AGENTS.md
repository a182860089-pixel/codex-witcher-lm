# Codex Provider Switcher Guidance

## Product boundary

This repository is a clean-room, unofficial standalone companion application.
It is not an OpenAI plugin, does not contain Codex Desktop source, and must not
modify `app.asar`, the signed macOS app bundle, or the Windows Store package.

## User flow

- Treat the primary audience as a non-developer. On launch, inspect the active
  Codex provider, model, Base URL, and credential method automatically.
- The normal setup path is Base URL + API Key, fetch models, select the models
  to keep, then save or save-and-switch.
- Support manual model IDs when a compatible service does not expose a
  standard model-list endpoint.
- Saved connections are shortcuts in `profiles.json`; they must never contain
  API keys. Quick switching chooses a saved model and updates the user's
  existing Codex configuration transactionally.
- Explain that a switch applies to new threads after Codex is fully restarted.
  Do not expose internal hashes, catalog metadata, CDP details, or file-system
  paths in the normal novice flow.

## Safety invariants

- Prefer documented Codex configuration and App Server requests.
- Never write API keys to `config.toml`, `auth.json`, logs, state, or command
  arguments. Use macOS Keychain or Windows Credential Manager.
- Model discovery uses the system proxy, permits HTTPS plus loopback HTTP,
  follows no redirects, and bounds response size and model count.
- Discovery credentials may exist only in the zeroizing in-process vault:
  at most eight live sessions and ten minutes per session.
- Do not add `model_catalog_json`. Preserve a user-owned value unchanged.
  The Switcher-managed `models.json` is internal transaction companion data,
  not a Codex catalog pointer.
- Every config edit must be planned against a content hash, written atomically,
  backed up exactly, and restored only when the applied hash still matches.
- CDP is opt-in, loopback-only, bound to a verified app/process/browser
  identity, and disabled for unknown Desktop builds.
- Do not add broad renderer monkey patches. A build adapter must identify the
  exact App Server bridge it transforms.
- Existing threads retain their provider. Provider/model selection applies only
  to new `thread/start` requests.

## Repository layout

- `crates/switcher-core/`: provider/model validation, discovery, saved-profile
  storage, config transactions, CDP validation, and App Server transforms.
- `src-tauri/`: Tauri 2 desktop shell and platform/keyring integration.
- `src/`: dependency-light TypeScript UI.
- `runtime/`: fail-closed renderer-side transform primitives.
- `compat/`: reviewed Desktop build adapter registry.
- `docs/`: architecture, compatibility, security, and release notes.

## Checks

Run the narrowest relevant checks:

```bash
pnpm check
pnpm test
cargo fmt --all -- --check
cargo test -p codex-provider-switcher-core
cargo check -p codex-provider-switcher-desktop
```

Packaging must also pass unsigned test builds on both macOS and Windows.
Release artifacts require platform signing and target-host smoke tests.
