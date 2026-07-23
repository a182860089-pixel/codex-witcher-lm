# Architecture

## Product surface

Codex Provider Switcher is an unofficial standalone Tauri 2 companion. It is
not a Codex plugin and does not modify the signed Codex Desktop installation.
Its primary surface is a novice-facing connection and model picker. The
configuration product remains separate from the version-sensitive Desktop
compatibility layer.

## Control flow

```text
Vanilla TypeScript UI
        |
        | typed Tauri commands
        v
Rust application boundary
        |
        +-- inspect current Codex provider/model/access method
        +-- HTTPS model discovery through the system proxy
        +-- keyless saved-connection validation
        +-- native Keychain / Credential Manager
        +-- config + internal-state transaction
        +-- exact backup / conflict-aware restore
        |
        +-- ~/.codex/config.toml
        +-- ~/.codex/provider-switcher/profiles.json
        `-- ~/.codex/provider-switcher/models.json
```

On launch, `inspect_state` resolves `CODEX_HOME`, reads `config.toml`, and
reports the active provider name and ID, model, Base URL, and credential kind.
It does not return an inline token or resolve a provider's environment
variable. The renderer cannot choose filesystem targets or submit rendered
TOML.

## Connection and discovery flow

The normal UI sequence is:

1. Enter an optional connection name, Base URL, and API Key.
2. Ask the Rust backend to fetch the standard model list.
3. Check only the models that should appear in the saved connection. Search,
   select-visible, and clear controls keep large responses manageable.
4. If discovery is unavailable but the service is known to be compatible, add
   one or more model IDs manually.
5. Choose the default model, then save or save-and-switch.

For an unversioned Base URL, discovery tries `GET /v1/models` followed by
`GET /models`. A versioned API path uses its adjacent `models` endpoint; a
supplied `/responses` suffix resolves back to the corresponding API base. The
first successful response determines the normalized Base URL stored in the
profile, so a root URL that succeeds at `/v1/models` is saved with `/v1`.
Discovery expects the standard `{ "data": [{ "id": ... }] }` shape, removes
invalid and duplicate IDs, sorts the result, and exposes at most 500 models.

The HTTP client uses system proxy settings, follows no redirects, permits
remote HTTPS and loopback HTTP only, and enforces an eight-second connection
timeout, a 15-second request timeout, and a 2 MiB response limit. A model-list
response is UI discovery data, not a Codex rich model catalog. The app writes
only the model IDs explicitly selected by the user.

The API Key is cleared from the WebView after discovery. A zeroizing Rust vault
holds it until save, cancel, eviction, or expiry. The vault allows at most
eight live discovery sessions; each expires after ten minutes. Save moves the
credential to the endpoint-bound operating-system keyring entry.

## Saved connections and quick switching

`profiles.json` is a versioned, private file containing display names,
normalized Base URLs, and selected model metadata. It never contains API keys.
The store accepts at most 64 profiles and is serialized under an exclusive
lock with a private atomic write.

Each saved connection card has its own model picker. Switching reloads the
saved profile server-side and passes its selected model through the same
validation and transaction path as first-time save-and-switch. Removing a
shortcut removes only the `profiles.json` entry; it does not silently alter the
current Codex configuration or delete an endpoint credential.

## Native configuration layer

The manager writes only documented Codex settings:

- `model_provider`
- `model`
- `model_providers.<id>` with `wire_api = "responses"`
- `model_providers.<id>.auth` pointing back to this signed executable's
  `credential get <endpoint-fingerprint>` mode

The manager does not add `model_catalog_json`. If the user's configuration
already has a `model_catalog_json` value, `toml_edit` preserves it unchanged.
The Switcher-managed `models.json` is internal transaction companion data for
the active saved model set and exact restore; it is not installed as a Codex
catalog pointer.

The auth argument is an `endpoint-v1-...` fingerprint over the provider ID and
normalized base URL. Reusing a provider ID with a different endpoint therefore
requires a new Keychain/Credential Manager authorization.

On apply, the app installs a content-addressed copy of its command helper under
`$CODEX_HOME/provider-switcher/helpers/<sha256>/`. Configuration points to that
stable copy instead of an App Translocation or installer path. Old helper
versions remain available so an exact backup can still authenticate or run
`recovery restore-latest` after the main app is moved or removed.

A provider must implement the OpenAI Responses API contract. Translating
arbitrary chat-completions APIs is outside the MVP.

The optional Open Codex action uses macOS Launch Services with the official
bundle identifier. On Windows it uses `windows-rs` to require one registered,
non-development, Store-signed `OpenAI.Codex` package and one valid AppUserModel
entry before activating it. It does not close a running process or pass CDP
arguments.

## Transaction layer

The configuration transaction follows this sequence:

1. Reject symlinked or non-regular targets.
2. Acquire a per-config exclusive file lock.
3. Compare current bytes to the plan SHA-256.
4. Write exact originals and a `Prepared` manifest under a private unique
   backup folder.
5. Stage and sync same-directory temporary files, then perform a final
   expected-hash check immediately before each replacement.
6. Replace internal model state and configuration, then mark the manifest
   `Applied`.

Restore first verifies both applied hashes. It refuses to overwrite any edit
made after the switch. It journals `Restoring` before the first target change,
so startup recovery can finish an interrupted restore. Existing files are
restored byte-for-byte after their backup hashes pass; transaction-created
files are removed.

Windows uses `ReplaceFileW` for existing files without the ACL/merge-ignore
flags; new-file moves use `MOVEFILE_WRITE_THROUGH`. Unix replacements preserve
mode bits. Portable filesystem APIs still leave a very small interval between
the last content check and replacement/removal, so absolute no-overwrite under
an uncooperative concurrent writer remains a release blocker rather than an
MVP guarantee.

## Desktop compatibility layer

The `runtime/` module contains pure transforms for three documented App Server
methods:

- `model/list`: request hidden catalog entries.
- `thread/list`: use an empty provider filter to include all providers.
- `thread/start`: set both `model` and `modelProvider` for a new thread.

Resume, fork, and turn methods pass through unchanged. The module wraps only an
explicitly supplied App Server client and never patches `Response.prototype`,
Statsig, React Fiber, or a global module loader.

`compat/desktop-builds.json` is intentionally empty. A future adapter may be
enabled only after a specific Desktop version, signature/package identity,
bridge contract, and rollback test are reviewed. Unknown builds remain in safe
mode, so the current release never injects into Codex Desktop.

## CDP supervisor boundary

If native configuration is overridden by a supported Desktop build, the next
layer is a Rust-only supervisor:

1. Allocate a random loopback port.
2. Verify official process/package identity and listener ownership.
3. Read `/json/version`, validate the browser WebSocket URL, and keep a Browser
   ID anchor connection open.
4. Accept only `app://codex` page targets whose page ID matches the strict
   `ws://127.0.0.1:<port>/devtools/page/<id>` endpoint.
5. Re-verify listener ownership and Browser ID before installing an adapter.
6. Stop immediately when the identity anchor closes.

The supervisor accepts only reviewed adapter objects with synchronous `apply`
and `cleanup` functions. It installs an origin guard for the top-level
`app://codex` document, treats `Runtime.evaluate` exception details as failure,
revalidates the target after apply, and calls cleanup for the current document
when installation fails or the hook is removed.

Platform process identity and a reviewed bridge adapter are intentionally not
guessed on a Linux development host.
