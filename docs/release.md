# Release Gates

## Required checks

- Node typecheck, runtime tests, and production WebView build.
- Rust formatting and all core/credential unit tests.
- Launch inspection fixtures for official and custom providers, including
  configs with a user-owned `model_catalog_json`.
- Model discovery fixtures for `/v1/models`, `/models`, versioned and
  `/responses` Base URLs, proxy behavior, redirect refusal, timeout, 2 MiB
  limit, malformed data, and the 500-model cap.
- Discovery-vault expiry, eight-session eviction, consume/cancel, endpoint
  mismatch, and zeroization-sensitive error paths.
- Keyless `profiles.json` create, replace, remove, lock, permission, schema, and
  atomic-write behavior.
- Native macOS and Windows Tauri package builds.
- macOS Apple silicon and Intel DMG install/remove plus application launch.
- Windows x86_64 per-user NSIS install/uninstall.
- Real Codex flow: auto-inspect, discover or manually enter models, select,
  save, quick-switch, full restart, start a new thread, exact restore, full
  restart.
- Keychain/Credential Manager flow: first-access prompt, locked store, missing
  entry, replacement, and deletion.
- Config fixtures: CRLF, non-ASCII, comments, dotted keys, concurrent edits,
  missing files, read-only files, symlinks, and interrupted writes.
- Credential endpoint rebinding: changing host or base path must require a new
  Keychain/Credential Manager entry.
- Applying a profile must not add `model_catalog_json`; a pre-existing
  user-owned value must remain byte-semantically unchanged.
- Any reviewed Desktop adapter must pass apply, exception cleanup, explicit
  removal, navigation, iframe, reload, and target-replacement tests.
- Existing Windows config/internal-model-state files must retain their complete
  security descriptor through apply and restore; new files must receive a
  private DACL.
- Crash/power-failure tests must prove Windows target replacement and journal
  durability.
- Uninstall must restore repeatedly until no managed helper is referenced,
  prune only unreferenced helpers/internal state/backups, and ask whether to
  delete endpoint-bound credentials. Conflicts must preserve recovery assets.
- Define and test backup retention because exact config backups can contain
  pre-existing non-Switcher secrets.

## Signing

macOS release artifacts require Developer ID Application signing, notarization,
and stapling. Windows requires Authenticode or Azure Artifact Signing for both
the executable and installer, with timestamping. Signing credentials belong in
the CI secret manager and must never be added to this repository.

The release workflow should use `tauri-apps/tauri-action@v1` pinned to an exact
commit. It is not enabled until both platform signing identities exist.

## No-go conditions

- Unknown Desktop build requires renderer injection.
- Any API key appears in a file, log, command line, crash report, or CDP payload.
- A redirect can forward a discovery credential, remote plain HTTP is accepted,
  or discovery exceeds its response/session bounds.
- `profiles.json` contains a credential or switching adds/replaces a
  user-owned `model_catalog_json`.
- Restore can overwrite a concurrent user edit.
- The final content-check/replacement interval can overwrite an uncooperative
  concurrent writer.
- Windows DACL or durable replacement behavior is unverified on a native host.
- Uninstall leaves a managed helper referenced by active or backed-up config,
  or silently deletes a conflicted recovery chain.
- A renderer adapter uses global Response/fetch, feature-flag, React Fiber, or
  private-module scanning fallbacks.
- A release is unsigned, unnotarized on macOS, or lacks real target-host smoke
  tests.

## 2026-07-23 Windows phase-2 evidence

The current source on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 42 core tests, one Credential Manager test, and one Windows
  launcher test.
- Desktop `cargo check` and the full Tauri/NSIS installer build.
- Isolated NSIS install/remove smoke testing.

Hashes are intentionally not recorded here until the release-candidate source
and installer are frozen. The generated installer and executable are unsigned
development artifacts. The isolated package smoke does not replace an
interactive user-session test, so GUI launch, Credential Manager interaction,
the Base URL/API Key discovery flow, provider apply, full Codex restart,
new-thread verification, exact restore, lifecycle cleanup, and release signing
remain open gates.
