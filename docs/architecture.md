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
        +-- tray + background-login lifecycle
        |
        +-- atomic active Route
        |        |
        |        `-- authenticated http://127.0.0.1:15722/v1
        |                 `-- selected HTTPS Responses provider
        |
        +-- ~/.codex/config.toml
        +-- ~/.codex/provider-switcher/profiles.json
        +-- ~/.codex/provider-switcher/official-profile.json
        +-- ~/.codex/provider-switcher/proxy.json
        `-- ~/.codex/provider-switcher/models.json
```

On launch, `inspect_state` resolves `CODEX_HOME`, reads `config.toml`, and
reports the active provider name and ID, model, Base URL, and credential kind.
It does not return an inline token or resolve a provider's environment
variable. The renderer cannot choose filesystem targets or submit rendered
TOML. `proxy_status` separately reads the keyless proxy state and starts the
loopback listener when fast switching was previously enabled. Before restoring
the Route, it installs the current content-addressed credential helper and
atomically refreshes the managed `cps-local` helper binding when an upgrade
left the configuration pointing at an older helper. Background login startup
uses the same path without showing the main window.

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
only the model IDs explicitly selected by the user. This upstream discovery
shape is separate from the rich Codex catalog served later by the local proxy.

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
saved profile server-side and validates that the selected model still belongs
to it. In the default local-proxy mode, a running proxy swaps the complete
immutable route without rewriting Codex configuration. In direct mode, the
choice passes through the original configuration transaction.

Opening the editor resolves the selected saved profile server-side, derives
its endpoint-bound keyring account, and returns that credential only to the
local Tauri WebView. The Key is shown in the editor for the current user and
cleared when the editor closes or saving finishes. An unchanged Base URL and
unchanged Key reuse the existing keyring entry without rewriting it.

Removing a shortcut removes only the `profiles.json` entry; it does not
silently alter the current Codex configuration or delete an endpoint
credential. The UI must not remove the route currently used by an enabled
proxy.

## Official account profile

The official-account flow deliberately does not model ChatGPT login as an API
provider credential. `prepare_official_login` creates a normal configuration
transaction that removes `model_provider`, `openai_base_url`, and a shadowing
`model_providers.openai` table, then leaves `model` unset so Codex can choose
its own default. It preserves all unrelated settings, including MCP servers,
hooks, other provider tables, and a user-owned `model_catalog_json`.

After Codex completes its native login, `save_current_official_profile`
accepts only the built-in `openai` route with no overridden Base URL. It writes
schema-v2 `official-profile.json` containing a user-chosen display name and
optional model ID. Existing schema-v1 files with the original fixed name remain
readable and upgrade to v2 on the next save. `activate_official_profile`
reapplies that route through the same hash-checked transaction. Neither command
reads or writes `auth.json`, queries Codex's token cache, or claims that a saved
route proves login state. Because Codex exposes one active login cache, this is
one named bookmark rather than a set of independently bound OAuth identities.

Switching from fast proxy mode to the official profile first performs the
validated proxy disable/restore flow. Switching back to an API profile can
enable the proxy again and create a new activation restore point. The boundary
between official and API profiles requires a full Codex restart; hot switching
continues only among API routes.

## Local proxy data plane

Fast switching uses a managed provider named `cps-local` at
`http://127.0.0.1:15722/v1`. The host, scheme, path, and fixed port are
validated; `localhost`, IPv6 loopback, and remote listeners are not accepted.
The listener exposes:

- authenticated `POST /responses` and `/v1/responses`
- authenticated `POST /responses/compact` and `/v1/responses/compact`
- authenticated `GET /models`, `/v1/models`, and `/health`

The entry bearer is a random 32-byte value encoded for header use and stored in
the OS credential manager under a loopback endpoint fingerprint. Codex obtains
that entry bearer through the stable command helper. It is distinct from every
upstream provider key.

For each request, the proxy validates exactly one entry bearer by comparing
SHA-256 digests in constant time. It removes the client authorization,
credential-like, forwarding, and hop-by-hop headers, including header names
declared by `Connection`; it then injects the selected upstream bearer. The
request body must be a bounded JSON object, and its `model` field is replaced
with the active route's selected model.

The active Route contains the provider Base URL, upstream bearer, selected
model, and checked model metadata. It is immutable and replaced atomically.
When both identifiers are available, requests with the same `thread_id` and
`turn_id` use one pinned Route across Responses and compaction even if the user
switches meanwhile. A later turn resolves the then-active Route. The bounded
pin table evicts old entries; callers may explicitly release a completed turn.

The upstream client accepts HTTPS plus loopback HTTP, follows no redirects, and
uses a retry policy that never replays a request automatically. Response status
and safe headers are preserved, while hop-by-hop response headers and
`Set-Cookie` are removed. Response bodies, including SSE, are forwarded as byte
streams rather than buffered to completion.

The local `/models` response is generated from the checked models on the active
Route and includes an ETag. It is a wire response, not a
`model_catalog_json` file. `/health` returns only listener, route summary,
pin-count, and request counters; neither endpoint returns credentials or the
upstream URL.

`proxy.json` schema v2 persists only enabled state, selected profile/model IDs,
fixed port, revision, restart notice, and a credential-free
`activationTransactionId`. A new activation preallocates this non-nil UUID
before applying configuration; hot Route changes preserve it and disabled
state clears it. Enabling fast switching also enables background login startup.
Closing the window hides it in the tray. Choosing Exit stops the current
process without changing the saved proxy activation or Codex configuration;
the registered background launch restores that Route at the next login or app
start. Windows writes the current-user `Run` entry as an exactly quoted
executable path plus `--background`, records `StartupApproved`, and reads both
values back before reporting success; disable removes both values idempotently.
macOS uses its LaunchAgent integration. Disabling resolves
`backups/<activationTransactionId>/manifest.json` and restores only that
validated Applied `cps-local` activation with matching paths and transaction
identity. Unchanged config and catalog files are restored exactly. If their
whole-file hashes differ only because unrelated valid TOML changed, disable
revalidates the managed loopback binding and helper, journals `Detaching`, and
restores only `model_provider`, `model`, and
`model_providers.cps-local` from the original config while preserving unrelated
edits. A changed managed field, malformed TOML, missing backup, or mismatched
recovery point fails closed for manual review. The backup manifest remains
schema v1.

## Native configuration layer

Direct mode writes only the selected upstream provider's documented Codex
settings:

- `model_provider`
- `model`
- `model_providers.<id>` with `wire_api = "responses"`
- `model_providers.<id>.auth` pointing back to this signed executable's
  `credential get <endpoint-fingerprint>` mode

On the first local-proxy enable, the same transaction instead writes
`model_provider = "cps-local"`, the chosen model, `wire_api = "responses"`,
`supports_websockets = false`, the strict loopback Base URL, and a helper
account bound to the proxy entry token. It does not put the upstream Base URL or
upstream key in Codex configuration. If Codex was not already using that exact
managed provider, the UI requires a full Codex restart. Once active, later
Route changes take effect on the next turn without another config edit.

The manager does not add `model_catalog_json`. If the user's configuration
already has a `model_catalog_json` value, `toml_edit` preserves it unchanged.
The Switcher-managed `models.json` is internal transaction companion data for
the active saved model set and exact restore; it is not installed as a Codex
catalog pointer.

Official activation uses the same transaction layer with an optional manifest
model ID. Existing schema-v1 manifests containing a string model deserialize
as `Some(model)`; proxy validation still requires that value. An official
manifest may omit it so Codex can select its default.

The auth argument is an `endpoint-v1-...` fingerprint over the provider ID and
normalized base URL. Reusing a provider ID with a different endpoint therefore
requires a new Keychain/Credential Manager authorization.

The local proxy uses a separate `proxy-client-v1-...` account derived from the
strict loopback URL. The helper verifies that this account is bound to exactly
one managed `cps-local` provider table before returning its token.

On apply, the app installs a content-addressed copy of its command helper under
`$CODEX_HOME/provider-switcher/helpers/<sha256>/`. Configuration points to that
stable copy instead of an App Translocation or installer path. Old helper
versions remain available so an exact backup can still authenticate or run
`recovery restore-latest` after the main app is moved or removed.
When an enabled installation starts after an upgrade, a locked compare-and-swap
write changes only the managed helper `command` and `cwd` to the current
content-addressed copy. The original activation backup is not rewritten.

On Windows, credential-helper caller verification accepts the registered
Store Codex package or the canonical official npm Codex x64 layout when
WinTrust validates its Authenticode signature and the signer name is OpenAI.

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

Restore first verifies both applied hashes. Exact restore journals `Restoring`
before the first target change, so startup recovery can finish an interrupted
restore. Existing files are restored byte-for-byte after their backup hashes
pass; transaction-created files are removed. Local-proxy disable has a narrower
conflict fallback: after revalidating every Switcher-owned proxy field, it
journals `Detaching` and compare-and-swap replaces only those owned fields with
their original values. It does not rewrite the internal catalog in that
fallback because Codex never points at it.

Windows uses `ReplaceFileW` for existing files without the ACL/merge-ignore
flags; new-file moves use `MOVEFILE_WRITE_THROUGH`. Unix replacements preserve
mode bits. Portable filesystem APIs still leave a very small interval between
the last content check and replacement/removal, so absolute no-overwrite under
an uncooperative concurrent writer remains a release blocker rather than an
MVP guarantee.

The first proxy enable preallocates its transaction UUID, starts and validates
the listener, enables background startup, persists schema-v2 keyless state with
that UUID, then applies the config plan under the same backup directory.
Failure rolls the runtime and state back toward the previous selection. If the
files were fully applied but the manifest stayed `Prepared`, exact startup
recovery finalizes it as `Applied`; known partial states roll back and unknown
states fail closed. A hot Route change updates `proxy.json`; if that write
fails, the previous Route is restored or the listener is stopped. Disabling
uses only the recorded activation. It preserves unrelated valid config edits,
but reports managed-field or unreadable-state conflicts instead of silently
replacing them.

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

The local proxy is independent of this CDP layer. It uses documented provider
configuration and the Responses wire API; it does not expose or patch the
Desktop renderer.

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
