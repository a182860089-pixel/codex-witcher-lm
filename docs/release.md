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
- Keyless `official-profile.json` schema, permission, corruption, model
  validation, user-chosen-name validation, v1 read compatibility, and
  credential-field exclusion. Capturing must accept only the built-in `openai`
  route without a Base URL and must never access `auth.json`.
- Official-route fixtures must remove `model_provider`, `openai_base_url`, and
  a shadowing `model_providers.openai` table while preserving MCP, hooks, other
  providers, and user-owned `model_catalog_json`. Test optional model manifests
  and legacy string-model manifest compatibility.
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
- Official-account flow: safely disable the proxy, prepare the built-in route,
  restart into Codex's native ChatGPT login, save the credential-free current
  route, switch to an API profile, switch back to official, and verify that
  Codex owns the same login cache throughout.
- Keychain/Credential Manager flow: first-access prompt, locked store, missing
  entry, saved-profile readback into the local editor, unchanged credential
  reuse, endpoint rebinding, replacement, and deletion. Windows tests must
  reopen the entry and verify `Local` persistence.
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

The tag-triggered `.github/workflows/release.yml` workflow may publish an
explicit GitHub **prerelease** before those identities exist. It runs the
portable and native test suites, builds on Windows x64, Apple silicon macOS,
and Intel macOS, smoke-tests each package, creates per-package and aggregate
SHA-256 manifests, and only then creates the prerelease. The macOS preview uses
the documented ad-hoc signing identity; the README and release notes must
continue to explain the Gatekeeper and SmartScreen limitations.

Do not promote that workflow to a normal public release until Windows signing
and macOS Developer ID signing, notarization, and stapling have been added and
verified. Signing secrets must remain in the GitHub Actions secret store.

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
- `official-profile.json` contains a credential, token, auth payload, Base URL,
  or any copied content from `auth.json`.
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

## 2026-07-29 Windows 0.3.3 UI evidence

The version 0.3.3 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 60 Windows-applicable core tests, two Credential Manager
  tests, two launcher tests, six desktop tests, six proxy unit tests, and five
  proxy integration tests.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke.
- Verified current-user upgrade without changing measured Codex/Switcher
  state hashes or Credential Manager metadata.
- Interactive Session 1 visual verification of the redesigned dark interface,
  navigation sidebar, saved-profile list, and visible version 0.3.3 label.

Artifact evidence:

- Source archive SHA-256:
  `072a2760903e5f85d92033aadd8c56822deb208ed144b455ccbe347c36b81082`
  (`392251` bytes)
- NSIS SHA-256:
  `a09ef59b4db69e7757248cebca205b13e43e6ff4d4419fa05380a0eed4a6dce4`
  (`3920148` bytes)
- Installed application SHA-256:
  `92567b3719bca1a1e0110b6e97c0e7a03ad69571849b48d9a7706aa21e58cdea`
- Interactive screenshot SHA-256:
  `6d89a894c58ce2e5869fe13aecbe9f0cecaebc88d0c1c87dc4a6610dfc300ac2`
- Authenticode status: `NotSigned`

The pre-upgrade installation and registration were retained at
`D:\CodexProviderSwitcher\rollback-0.3.2-before-0.3.3-20260729T070316Z`.
The official route remained selected, the proxy remained disabled, and neither
the port-15722 listener nor its automatic-startup entry was introduced. This
run did not perform a live provider turn or the interactive official-login/API
provider round trip.

## 2026-07-26 Windows 0.3.2 evidence

The version 0.3.2 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 60 Windows-applicable core tests, two Credential Manager
  tests, two launcher tests, six desktop tests, six proxy unit tests, and five
  proxy integration tests.
- Named schema-v2 official-profile validation and legacy schema-v1 read
  compatibility.
- Explicit Windows Credential Manager `Local` persistence round-trip coverage
  for newly stored credentials.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke.
- Verified current-user upgrade without changing Codex/Switcher state or
  Credential Manager metadata.
- Interactive Session 1 verification that an ordinary launch shows the main
  window, while `--background` remains suitable for a hidden gateway start.
- Visual verification of the official-profile name editor and current/saved
  official-route state without reading `auth.json`, OAuth tokens, or API Keys.

Artifact evidence:

- Source archive SHA-256:
  `cbc619f8f749f34f6aefcac4c034ec0dc6e76ba5183cbdc1ca91a8ae9ef14e74`
  (`399667` bytes)
- NSIS SHA-256:
  `6e86184062473a0174c52abfe5c1ad2c717154edd7c08e12ff542fbcb1b62188`
  (`3914685` bytes)
- Installed application SHA-256:
  `1392aab65b66b0b5ab911fffbd687415e0e89bc42fb547b87945f317ec334bbf`
- Authenticode status: `NotSigned`

The application and installer remain unsigned internal artifacts. This run
did not perform the user's interactive official-login/API-provider round trip.
The affected saved API connections had no recoverable Credential Manager
entries, so they require one local Key re-entry before selection; the product
now reports that specific condition without changing Codex configuration.

## 2026-07-25 Windows 0.3.0 evidence

The version 0.3.0 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 58 Windows-applicable core tests, one Credential Manager
  test, two launcher tests, five desktop tests, six proxy unit tests, and five
  proxy integration tests.
- Official-profile validation and native command coverage, including
  credential-free capture, built-in `openai` restoration, optional models,
  route-shadow removal, unrelated-config preservation, and legacy manifest
  compatibility.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke.
- Verified current-user upgrade from 0.2.4 to 0.3.0 without changing
  Codex/Switcher state or Credential Manager metadata.
- Live verification of one version 0.3.0 process in interactive Session 1,
  proxy ownership of `127.0.0.1:15722`, the quoted current-user Run
  registration, removal of the temporary deployment task, and preservation of
  the existing `claude-opus-4-8` Route and proxy revision.
- A `cps-local` auth command bound to the new content-addressed helper, with
  the helper SHA-256 equal to the installed application SHA-256.

Artifact evidence:

- Source archive SHA-256:
  `1f78e68d7c97d60fec9bf26f5c25baa8d912264621d0a8fcf5a373cfe3cf92c3`
- NSIS SHA-256:
  `2afba8700f6533150cb90a6ca6baced5cd52d8cf2d8621491cb12e46833d9ca5`
  (`4281055` bytes)
- Installed application and active helper SHA-256:
  `7a7cfae27580d310f443f2d0e0f342887cb07c26b5c592bef8fdb80b8dbb4d08`
- Authenticode status: `NotSigned`

This is internal deployment evidence, not a public release approval. The run
deliberately preserved the user's current API route and did not exercise the
interactive OpenAI prepare/login/save/switch-back flow. It also did not repeat
a live upstream model turn. Native Windows DACL/durability, disable/restore,
uninstall cleanup, signed release gates, and the complete official-account
round trip remain open.

## 2026-07-25 Windows 0.2.4 evidence

The version 0.2.4 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 53 Windows-applicable core tests, one Credential Manager
  test, two launcher tests, four desktop tests, six proxy unit tests, and five
  proxy integration tests.
- The new saved-profile credential lookup test, which limits Key readback to
  an existing profile that is marked as requiring a credential.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke.
- Verified current-user upgrade from 0.2.3 to 0.2.4 without changing
  Codex/Switcher state or Credential Manager metadata.
- Read-only live verification of one version 0.2.4 process in interactive
  Session 1, proxy ownership of `127.0.0.1:15722`, the quoted current-user Run
  registration, removal of the temporary deployment task, and the active
  `gpt-5.6-sol` Route.
- A `cps-local` auth command bound to the new content-addressed helper, with
  the helper SHA-256 equal to the installed application SHA-256.

Artifact evidence:

- Source archive SHA-256:
  `b2b2b455c07390c5c366ea5fc5386001b9086e17849e52fcaa8c89f4467c99c8`
- NSIS SHA-256:
  `7841590a41bbf5161cb818589629c7463cb82fdbb81109637eed10bc43eaf37a`
  (`4274546` bytes)
- Installed application and active helper SHA-256:
  `c2585f1ca16b571baca9e837c23aad78c7bb291f89a64c7e7af5095901907bb5`
- Authenticode status: `NotSigned`

This is internal deployment evidence, not a public release approval. The run
did not repeat a live upstream model turn. Native Windows DACL/durability,
  disable/restore, uninstall cleanup, signed release gates, and interactive
  OpenAI-login switching remain open.

## 2026-07-24 Windows 0.2.3 evidence

The version 0.2.3 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 53 Windows-applicable core tests, one Credential Manager
  test, two launcher tests, three desktop tests, six proxy unit tests, and five
  proxy integration tests.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke while preserving the existing uninstall
  registration, installer state, and shortcuts.
- Verified current-user upgrade from 0.2.2 to 0.2.3 without changing
  Codex/Switcher state or Credential Manager metadata.
- Interactive Session 1 background restart, proxy ownership of
  `127.0.0.1:15722`, quoted current-user Run registration, and
  content-addressed helper refresh to the installed 0.2.3 executable.
- Preserved active `gpt-5.6-sol` selection and the required first-restart flag
  for the new one-time customer notice.

Artifact evidence:

- Source archive SHA-256:
  `52c18cdf231897acdce896d3ae58104bafe5ff80a980c96ff8146f457e1e7f5f`
- NSIS SHA-256:
  `7c8c4a781104da6d3007ad7e539668938ac734fcb4a3f9a56b7856d09f4bdac8`
  (`4270992` bytes)
- Installed application SHA-256:
  `d7e9e9bcf73ee7d5f2f7e25276051df8d4f2463a70202f00c48dce1419a2d9ea`
- Authenticode status: `NotSigned`

This is internal deployment evidence, not a public release approval. The run
did not repeat a live upstream model turn. Native Windows DACL/durability,
disable/restore, uninstall cleanup, signed release gates, and official
OpenAI-login profile capture remain open.

## 2026-07-25 Windows 0.2.2 evidence

The version 0.2.2 source snapshot on the `ydy001` Windows 11 x64 host passed:

- TypeScript checking, four runtime tests, and the production WebView build.
- Rust formatting, 53 Windows-applicable core tests including normal versus
  `\\?\` helper path equivalence, one Credential Manager test, two launcher
  tests, two desktop tests, six proxy unit tests, and five proxy integration
  tests.
- Full Tauri release and NSIS packaging.
- Isolated NSIS install/remove smoke while preserving the existing uninstall
  registration, installer state, and two shortcuts.
- Verified current-user upgrade from 0.2.1 to 0.2.2 without changing
  Codex/Switcher state or Credential Manager metadata.
- Content-addressed helper refresh, proxy ownership of
  `127.0.0.1:15722`, missing-Run-entry repair, and a second interactive
  `--background` restart.
- An official npm `codex-cli 0.145.0` request from interactive Session 1. The
  request passed the credential helper and local proxy instead of returning
  401, then received upstream HTTP 503 because the selected
  `claude-opus-4-8` route had no available channel.

Artifact evidence:

- Source archive SHA-256:
  `79834566f5572fcc6906e1cb00440636f6823847a8b515d58195e3e5b3f5863f`
- NSIS SHA-256:
  `e2aa9d9263e31ed1d8e16faae8cb7865bbfc1ea82a3314ca5f48a6e70621d987`
  (`4271429` bytes)
- Installed application SHA-256:
  `ba38a88deae96789fb694ab762240e289b380a49a499b24e7dabeab1adb371d7`
- Authenticode status: `NotSigned`

This is internal deployment evidence, not a public release approval. Native
Windows DACL/durability, disable/restore, uninstall cleanup, and signed release
gates remain open. The observed upstream 503 also does not prove availability
of the selected provider/model route.

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
startup, and hot Route switching implementation. The 2026-07-25 evidence above
supersedes it for the tested 0.2.2 Windows paths.
