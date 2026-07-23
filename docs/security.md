# Security

## Protected data

Provider API keys are stored under the service
`dev.codex-provider-switcher.credentials` in macOS Keychain or Windows
Credential Manager. Each entry is bound to a SHA-256 fingerprint of the
provider ID and normalized base URL, so changing the endpoint cannot silently
reuse an old bearer token through the Switcher UI. The stable helper verifies
that the fingerprint still matches exactly one managed provider table,
its own command/working-directory binding, and an official Codex parent
process/package before returning a token. The API key is copied out of and then
immediately cleared from the password field; owned Rust secret buffers are
zeroized. The Switcher-managed key is never written to TOML, `profiles.json`,
`models.json`, backups, logs, command arguments, renderer injection payloads,
or CDP state.

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

## Configuration threats

- Symlink and non-file targets are rejected.
- The backend, not the WebView, resolves all file paths.
- A content digest rejects stale previews and is rechecked immediately before
  replacement.
- Exact originals are captured before either managed file is replaced.
- Restore uses applied digests as a second compare-and-swap boundary.
- Unix transaction directories use `0700`; backup and manifest files use
  `0600`.
- Windows transaction directories use a protected current-user DACL, and
  existing targets use `ReplaceFileW` so ACL merge failures fail closed.
- Absolute content CAS against an unrelated writer and Windows power-failure
  durability still require a native implementation/test before release.

The manager does not edit `auth.json`.

The manager also does not add `model_catalog_json`. A user-owned value already
present in `config.toml` is left unchanged. The Switcher-managed `models.json`
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
- install, switch, restart, restore, upgrade, and uninstall pass on real target
  machines.
- backup retention and uninstall choices are implemented without destroying a
  conflicted recovery chain.

Automatic updates remain disabled until the signing pipeline is stable.
