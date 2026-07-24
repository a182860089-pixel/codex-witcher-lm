# Security

## Protected data

Provider API keys are stored under the service
`dev.codex-provider-switcher.credentials` in macOS Keychain or Windows
Credential Manager. Each entry is bound to a SHA-256 fingerprint of the
provider ID and normalized base URL, so changing the endpoint cannot silently
reuse an old bearer token through the Switcher UI. The stable helper verifies
that the fingerprint still matches exactly one managed provider table, its own
command/working-directory binding, and an official Codex parent
process/package before returning a token. The API key is copied out of and then
immediately cleared from the password field; owned Rust secret buffers are
zeroized.

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
  providers. The client honors normal system proxy policy, follows no
  redirects, and disables automatic request retries.
- Response bodies are streamed without accumulating a full SSE response.
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

The manager does not edit `auth.json`.

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
