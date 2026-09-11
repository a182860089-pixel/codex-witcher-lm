# Security

## Protected data

Provider API keys are stored under the service
`dev.codex-provider-switcher.credentials` in macOS Keychain or Windows
Credential Manager. Each entry is bound to a SHA-256 fingerprint of the
provider ID and normalized base URL, so changing the endpoint cannot silently
reuse an old bearer token through the Switcher UI. The stable helper verifies
that the fingerprint still matches exactly one managed provider table, its own
command/working-directory binding, and an official Codex parent
process/package before returning a token. On Windows, accepted callers are a
process inside the registered Store package, the canonical official npm
package layout, or the official Desktop AppData sidecar
(`%LOCALAPPDATA%\OpenAI\Codex\bin\[<hex>\]codex.exe`); the latter two also
require a valid Authenticode signature whose signer is OpenAI. The helper
walks a bounded parent-process chain so a Store-packaged ChatGPT.exe that
launches that sidecar still qualifies. The API key is copied out
of and then cleared from the setup field after discovery. When the current
user explicitly opens a saved connection for editing, the backend resolves
only that saved profile's endpoint-bound account and returns its Key to the
local Tauri WebView. The editor shows it until save or close, never writes it
to application files or logs, and reuses the existing keyring entry when the
endpoint and Key are unchanged. Owned Rust secret buffers used by discovery
and proxy routing are zeroized.

On Windows, new and updated entries use Credential Manager `Local` persistence
instead of the provider library's default `Enterprise` persistence. The target
name remains unchanged for compatibility. A missing entry is treated as
missing user data, not as a generic keyring failure: switching stops before any
Codex configuration write and the UI asks the user to enter that connection's
Key again.

Fast switching adds a second credential class: a random local-proxy entry
bearer stored in the same OS credential manager under a
`proxy-client-v1-...` loopback fingerprint. Codex receives this entry bearer
through the stable helper and uses it only against
`http://127.0.0.1:15722/v1`. The proxy uses the entry bearer to authenticate the
client, then independently loads the selected provider key for the upstream
request. The two bearers are never interchangeable.

Neither credential is written to TOML, `profiles.json`, `proxy.json`,
`models.json`, backups, logs, command arguments, renderer injection payloads,
or CDP state. `proxy.json` schema v2 stores only the enabled flag,
profile/model IDs, credential-free `activationTransactionId`, fixed port,
revision, and restart notice.

Between model discovery and profile save, the key lives only in a zeroizing
in-process Rust vault. A session is consumed by save, removed by cancel, or
expires after at most ten minutes. The vault retains at most eight sessions;
when full, insertion evicts the oldest remaining session. The session ID is
not a credential, and an expired ID requires the user to enter the API Key
again.

`profiles.json` stores only keyless shortcut metadata: profile ID and name,
resolved Base URL, and the explicit models selected by the user. Removing a
shortcut does not delete its keyring entry; credential deletion is an explicit
lifecycle concern.

This is not a security boundary against every process running as the same OS
user, process injection/parent spoofing, or Codex effective-config overrides
from profiles, CLI `-c`, managed configuration, or trusted project layers. The
upstream command-auth contract does not pass the effective request endpoint to
the helper, so those layers cannot be cryptographically bound here. Treat
trusted workspace/config execution as able to use the configured provider.

The command-backed auth helper writes only the secret to stdout. Errors are
redacted and never contain the keyring backend's detailed payload.

## Application update checks

- Update metadata is fetched only from the GitHub Releases API for this
  repository over HTTPS, with redirects disabled and a 512 KiB response cap.
- Opening Update is limited to HTTPS GitHub release links for the owner/repo
  configured in `src-tauri/update-source.json`. Skip records only the skipped
  version in `~/.codex/provider-switcher/update-preference.json`.

## Model discovery threats

- Remote endpoints must use HTTPS. Plain HTTP is permitted only for loopback
  development addresses.
- The HTTP client follows no redirects, preventing a model-list request from
  forwarding the bearer credential to a different redirect target.
- System proxy settings are honored. A user- or administrator-configured proxy
  can therefore observe destination metadata and, when its trust root is
  installed, may terminate TLS; this follows normal system networking policy.
- Connect and total request timeouts are eight and 15 seconds respectively.
- Both declared and streamed response size are bounded to 2 MiB.
- Only the first 500 valid, unique model IDs are retained.
- HTTP status and generic connection/parse failures may be shown, but response
  bodies, proxy details, and secrets are not copied into UI errors.
- A manually entered model ID bypasses model-list discovery only. It does not
  relax Base URL validation or the requirement that the provider implement the
  Responses API when Codex uses it.

## Local proxy threats

- The listener binds only to IPv4 `127.0.0.1` on the managed fixed port.
  `localhost`, IPv6 loopback, wildcard, and remote bind addresses are rejected.
- `/health`, `/models`, `/v1/models`, `/responses`, `/v1/responses`, and both
  compact paths require exactly one valid entry bearer.
- The proxy stores only the SHA-256 digest needed to verify the entry bearer.
  Candidate and expected digests are compared in constant time. Invalid auth
  is rejected before the request body is read or an upstream route is used.
- Client `Authorization`, cookie, API-key-like, forwarding, proxy-auth, and
  hop-by-hop headers are removed. Header names nominated by `Connection` are
  also removed. Only then is the active provider bearer injected.
- The selected Route is immutable and replaced atomically. When both
  identifiers are present, a bounded `(thread_id, turn_id)` pin prevents
  `/responses` and `/responses/compact` for one turn from being split across
  providers.
- The proxy overwrites the JSON `model` field with the active selection and
  bounds request bodies to 16 MiB. Non-object or malformed JSON is rejected.
- Remote upstreams require HTTPS; plain HTTP is allowed only for loopback
  providers. The client honors normal system proxy policy, uses HTTP/1.1
  only, follows no redirects, and disables automatic request retries.
  Loopback destinations are excluded from that proxy. Starting the listener
  also merges loopback hosts into the user `NO_PROXY` environment so Codex
  itself does not send `127.0.0.1` through a local HTTP proxy.
- Successful response bodies are streamed without accumulating a full SSE
  response. 4xx/5xx bodies are bounded and rewritten into a JSON error so
  Codex can display the provider failure instead of "Unknown error".
  Hop-by-hop response headers and `Set-Cookie` are removed.
- The local model catalog and health body contain no bearer or upstream Base
  URL. Route and bearer debug output is redacted.
- The pin table is bounded rather than permanent. The proxy has no authority
  over another process running as the same OS user; possession of the entry
  bearer remains the local access boundary.

## Configuration threats

- Symlink and non-file targets are rejected.
- The backend, not the WebView, resolves all file paths.
- A content digest rejects stale previews and is rechecked immediately before
  replacement.
- Exact originals are captured before either managed file is replaced.
- Exact restore uses applied digests as a second compare-and-swap boundary.
- First activation preallocates a non-nil transaction ID, persists it in proxy
  state, and uses that exact ID for the backup directory and manifest.
- Disable accepts only the named Applied `cps-local` manifest with exact
  config/catalog paths, layout, and transaction ID. It restores exact bytes
  when applied hashes match. Otherwise it may detach only after revalidating
  the managed loopback provider, selected model, helper integrity, and original
  backup; the compare-and-swap merge restores only Switcher-owned fields and
  preserves unrelated valid TOML. Missing or mismatched evidence fails closed.
- Unix transaction directories use `0700`; backup and manifest files use
  `0600`.
- Windows transaction directories use a protected current-user DACL, and
  existing targets use `ReplaceFileW` so ACL merge failures fail closed.
- Absolute content CAS against an unrelated writer and Windows power-failure
  durability still require a native implementation/test before release.

The manager does not edit `auth.json`. Official authentication is accessed
only through the public Codex App Server account surface:

- `account/read` returns the authentication type and optional ChatGPT
  `email`/`planType` metadata without returning a credential.
- `account/login/start` with `type: "chatgpt"` lets Codex own the browser
  callback, token persistence, and token refresh.
- The login child removes inherited `OPENAI_API_KEY`, `CODEX_API_KEY`,
  `CODEX_ACCESS_TOKEN`, and App Server login overrides before starting.
- The returned browser URL must use HTTPS on `auth.openai.com`,
  `chatgpt.com`, or a nonempty `chatgpt.com` subdomain, with no URL userinfo.
- The Switcher waits for the matching `account/login/completed` notification
  and confirms `chatgpt` through a second `account/read`.
- Explicit removal first confirms the built-in route and a ChatGPT account,
  calls `account/logout`, confirms the signed-out result, and only then removes
  the local metadata file.

Removing all three credential variables from the child is deliberate process
isolation, not evidence that all three override normal interactive
authentication. In `rust-v0.145.0`, the TUI and App Server set
`enable_codex_api_key_env` to `false`, while `codex exec` sets it to `true`.
Thus `CODEX_API_KEY` environment authentication is limited to `codex exec`,
and `OPENAI_API_KEY` is not an implicit runtime override for the normal
TUI/App Server.

The official-account profile is a route and display-metadata cache, not an
authentication backup. Schema v3 contains a display name, an optional
validated model ID, and the current account's optional `email` and `planType`.
Those metadata fields are personal data, so the file uses the same private,
regular-file and atomic-write boundary as other state. Schema v1 and v2 remain
readable for migration but cannot contain account metadata.

The app never reads, parses, copies, exports, logs, or writes Codex ChatGPT
access or refresh tokens, whether Codex stores them in `auth.json` or the
operating-system keyring. The email and plan are taken only from
`account/read`; they are not decoded from a token. Login, expiry, and refresh
remain inside Codex.

The current public App Server method registry exposes one active
authentication result and no stable `account/sessions/*` RPC for persisting
and switching multiple OAuth sessions. Signing in again may replace Codex's
active account and overwrites the Switcher's one metadata cache. A cached email
must never be presented as an independently restorable OAuth identity.
Likewise, “退出并移除” logs Codex out of that one active account; it is not a
local-only profile deletion.

Official activation removes route fields that could redirect the built-in
`openai` provider, but it does so through the same content-hash transaction and
preserves unrelated TOML. It does not delete other saved API profiles or their
system credentials. Conversely, the built-in route is not evidence of a
ChatGPT login: UI state must combine route inspection with `account/read`.
The UI rejects a persisted-official-OAuth claim while the Switcher inherited
`CODEX_ACCESS_TOKEN`, labels that state as an external access-token conflict,
and does not treat the two API-key variables as the same condition. Switching
to official authentication first detaches the local proxy and requires a full
Codex restart; the Switcher and Codex must both be restarted without the
external token before persisted OAuth can be reported as active.

In local-proxy mode, the managed Codex provider contains only the strict
loopback Base URL and a helper reference for the entry bearer. The upstream
provider URL and key remain outside Codex configuration. Direct mode writes the
chosen upstream provider metadata but still obtains its key through the stable
helper.

The manager does not add `model_catalog_json`. A user-owned value already
present in `config.toml` is left unchanged. The proxy's authenticated
`/models` response is generated in memory; the Switcher-managed `models.json`
is internal transaction companion data and is not written into Codex
configuration as a catalog pointer.

Backups contain the exact pre-change `config.toml`. Although the
Switcher-managed provider key is not in that file, pre-existing MCP headers,
tokens, or other inline secrets may be. Backups are therefore secret-bearing.
The MVP keeps them for exact recovery and has no automatic retention/prune
policy; that policy is a release blocker.

## CDP threats

CDP is an unauthenticated high-privilege interface. The compatibility registry
is empty in the MVP, and the novice UI exposes no injector control.

Any future CDP implementation must require all of these at the same time:

- random port bound only to `127.0.0.1`
- verified official Codex process/bundle/Store identity
- listener-owner verification before and after discovery
- strict browser/page WebSocket URL validation
- long-lived Browser ID identity anchor
- `app://codex` target origin and exact page-ID match
- a top-frame `app://codex` runtime guard and post-apply target verification
- an adapter-owned synchronous cleanup function for current-document rollback
- `Runtime.evaluate` exception and result-state validation
- version-specific adapter allowlist
- no secret material in injected JavaScript
- explicit user authorization before closing or restarting Codex

No fallback may globally intercept fetch/Response, mutate feature flags, scan
React Fiber, or guess private modules.

## Release trust

Unsigned development builds are for local testing only. A public release is
blocked until:

- macOS artifacts are signed with Developer ID Application, notarized, and
  stapled.
- Windows executable and per-user NSIS installer are Authenticode-signed with
  timestamping.
- exact dependency locks and third-party notices are reviewed.
- install, first proxy enable, initial restart, next-turn route switch,
  background relaunch, disable/restore, upgrade, and uninstall pass on real
  target machines.
- backup retention and uninstall choices are implemented without destroying a
  conflicted recovery chain.
- uninstall removes or safely disables Switcher-owned background-startup state
  and never leaves an enabled proxy configuration without an available
  listener or recovery path.

Automatic updates remain disabled until the signing pipeline is stable.
