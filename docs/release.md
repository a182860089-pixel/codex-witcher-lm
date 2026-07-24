# Release Gates

## Required checks

- Node typecheck, runtime tests, and production WebView build.
- Rust formatting and all core, credential, launcher, local-proxy, and desktop
  tests.
- Launch inspection fixtures for official and custom providers, including
  configs with a user-owned `model_catalog_json`.
- Model discovery fixtures for `/v1/models`, `/models`, versioned and
  `/responses` Base URLs, proxy behavior, redirect refusal, timeout, 2 MiB
  limit, malformed data, and the 500-model cap.
- Discovery-vault expiry, eight-session eviction, consume/cancel, endpoint
  mismatch, and zeroization-sensitive error paths.
- Local-proxy protocol tests for strict IPv4 loopback binding, fixed-port
  collision, missing/duplicate/invalid entry auth, request-body limits,
  malformed JSON, no active Route, model overwrite, upstream bearer injection,
  sensitive and hop-by-hop header removal, redirect refusal, no retries,
  response-header filtering, SSE streaming/interruption, and redacted errors.
- Both `/models` paths must deserialize as the Codex `ModelsResponse` used by
  the target client. Test checked-model contents, ETag changes, authenticated
  health output, and the absence of credentials or upstream URLs.
- Route tests must cover concurrent atomic switching, the same
  `(thread_id, turn_id)` across Responses and compact, later-turn selection,
  explicit release, and bounded-pin eviction.
- Keyless `profiles.json` create, replace, remove, lock, permission, schema, and
  atomic-write behavior.
- Keyless `proxy.json` schema-v2, permission, corruption, revision, fixed-port,
  missing profile/model/activation ID, v1 migration, and write-failure rollback
  behavior. Test non-nil ID preallocation, exact manifest path/ID matching, ID
  preservation across hot switches, and clearing on disable.
- Proxy-entry credential tests for generation, keyring storage, helper binding,
  missing/replaced entries, separation from upstream keys, and redacted failure
  paths.
- Native macOS and Windows Tauri package builds.
- macOS Apple silicon and Intel DMG install/remove plus application, tray, and
  LaunchAgent lifecycle.
- Windows x86_64 per-user NSIS install/uninstall plus tray and automatic-startup
  lifecycle. The current-user Run command must quote a path containing spaces,
  round-trip exactly, and be removed together with its StartupApproved value.
- First enable must start the listener, enable background startup, persist
  keyless state, apply `cps-local`, and roll all of those steps back when any
  later step fails.
- Crash tests must cover the interval after schema-v2 state is written and
  before config apply, plus `Prepared` finalization, known-state rollback, and
  unknown-state refusal.
- Background launch must restore the selected Route without showing the main
  window. Closing the window must keep an enabled proxy alive; disabling must
  restore config, stop the listener, and disable background startup.
- Real Codex flow: auto-inspect, discover or manually enter models, select,
  enable the proxy, perform the requested initial restart, load `/v1/models`,
  complete an SSE Responses turn and compaction, switch model for the next
  turn, switch provider in a new thread, disable, exact restore, and restart.
- Direct-mode flow: apply an upstream provider, fully restart, start a new
  thread, and exactly restore.
- Keychain/Credential Manager flow: first-access prompt, locked store, missing
  entry, replacement, and deletion.
- Config fixtures: CRLF, non-ASCII, comments, dotted keys, concurrent edits,
  missing files, read-only files, symlinks, and interrupted writes.
- Proxy-detach fixtures must prove that unrelated valid TOML survives, changes
  to any Switcher-owned field fail closed, `Detaching` resumes after interruption,
  and a concurrent writer cannot be silently overwritten.
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
- The proxy entry bearer appears in config, profile/proxy state, logs, command
  arguments, model/health output, or an upstream request.
- A redirect can forward a discovery credential, remote plain HTTP is accepted,
  or discovery exceeds its response/session bounds.
- The proxy can bind outside `127.0.0.1`, accepts an unauthenticated endpoint,
  forwards the client bearer or another sensitive/hop-by-hop header, follows a
  redirect, or automatically replays a Responses request.
- One `(thread_id, turn_id)` can move between Routes, or a hot switch mutates an
  in-flight Route rather than replacing it atomically.
- `profiles.json` contains a credential or switching adds/replaces a
  user-owned `model_catalog_json`.
- Exact restore or semantic proxy detach can overwrite a concurrent user edit.
- Disable can restore an unrelated latest backup or a manifest whose
  provider/status/path/transaction ID does not match the recorded activation,
  or semantic detach proceeds after a Switcher-owned field changed.
- The final content-check/replacement interval can overwrite an uncooperative
  concurrent writer.
- Windows DACL or durable replacement behavior is unverified on a native host.
- Uninstall leaves a managed helper referenced by active or backed-up config,
  leaves Switcher-owned automatic startup active, leaves an enabled proxy
  configuration without a recovery path, or silently deletes a conflicted
  recovery chain.
- A renderer adapter uses global Response/fetch, feature-flag, React Fiber, or
  private-module scanning fallbacks.
- A release is unsigned, unnotarized on macOS, or lacks real target-host smoke
  tests.

## 2026-07-23 Windows phase-2 evidence

The pre-0.2.0 phase-2 source snapshot on the `ydy001` Windows 11 x64 host
passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 42 core tests, one Credential Manager test, and one Windows
  launcher test.
- Desktop `cargo check` and the full Tauri/NSIS installer build.
- Isolated NSIS install/remove smoke testing.

Hashes are intentionally not recorded here until the release-candidate source
and installer are frozen. The generated installer and executable are unsigned
development artifacts. The isolated package smoke does not replace an
interactive user-session test.

This evidence predates the local proxy, local model catalog, tray, background
startup, and hot Route switching implementation. It is not 0.2.0 evidence.
Current native macOS and Windows test results, package hashes, GUI lifecycle,
Credential Manager/Keychain interaction, real Codex traffic, disable/restore,
upgrade/uninstall cleanup, and release signing remain open gates.
