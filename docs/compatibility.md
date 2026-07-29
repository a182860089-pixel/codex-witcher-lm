# Compatibility

## Current matrix

| Surface | Architecture | Current evidence | Release gate |
| --- | --- | --- | --- |
| macOS | Apple silicon | Version 0.3.3 passed native Web/Rust/Tauri tests plus ad-hoc DMG build and install/remove smoke on `macos-15`; the published DMG was checksum-verified after download | Add Developer ID signing, notarization, stapling, and run the complete real-host switching lifecycle |
| macOS | Intel | Version 0.3.3 passed the same native suite plus ad-hoc DMG build and install/remove smoke on `macos-15-intel`; the published DMG was checksum-verified after download | Run the same signed and notarized real-host lifecycle |
| Windows | x86_64 | Version 0.3.3 passed native Web/Rust/Tauri tests, NSIS package smoke, a state-preserving upgrade on `ydy001`, and an interactive visual smoke of the redesigned light/dark interface | Complete interactive official OAuth switching, DACL/durability, cleanup/uninstall, signing, and a live turn on an upstream model with an available channel |
| Windows | ARM64 | Not claimed in MVP | Add a native runner and signed artifact before support |
| Linux | x86_64 | Development host only; portable proxy unit/integration tests exist in source | No Codex Desktop product target |

The supported operating-system floor should follow the current signed Codex
Desktop and Tauri 2 requirements. It must be recorded from real release hosts;
this repository does not freeze a guessed minimum version.

## Provider contract

The MVP supports providers that expose an OpenAI Responses-compatible base URL.
Remote endpoints must use HTTPS. Plain HTTP is accepted only for loopback
development endpoints. The 0.2.0 proxy forwards HTTP Responses and compact
requests, including SSE bodies; it deliberately advertises
`supports_websockets = false` and does not translate chat-completions or
Responses WebSocket protocols.

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
override. In fast-switch mode, the checked entries are rendered as an
authenticated Codex-compatible `/models` response by the loopback proxy.

## Desktop behavior

The default path is native Codex configuration plus the managed loopback
provider. On first enable, fully quit and reopen Codex if the Switcher reports
that it changed the active provider. Once Codex is using `cps-local`, switching
a saved connection or model replaces the proxy Route and applies to the next
turn. Requests already sharing a `thread_id` and `turn_id` remain on their
original Route.

A provider change during an existing thread is not guaranteed to carry
provider-specific response IDs or hidden state across services. Start a new
thread when changing providers unless that combination has been verified.
Direct configuration remains a fallback and always requires a full restart and
a new thread.

At launch, the app reads the active Codex model, provider, Base URL, and
credential method. Saved connections in the keyless `profiles.json` then offer
a small model picker. First proxy enable installs the strict
`http://127.0.0.1:15722/v1` provider through the reversible transaction; later
choices update only the keyless proxy state and in-memory Route. Disabling fast
switching uses the validated backup recorded for that activation, restores
exactly when possible, otherwise removes only verified Switcher-owned proxy
fields while preserving unrelated valid TOML, stops the listener, and disables
background startup. Any user-owned
`model_catalog_json` remains untouched; the internal `models.json` is not
installed as its value.

The closed-source Desktop UI may override or filter custom model values on a
particular release. No build-specific adapter is shipped yet, so this case is
reported as a compatibility gap rather than bypassed with a broad monkey
patch. The external manager still preserves a recovery point and can restore
the exact prior configuration.

The official login path uses only documented Codex configuration defaults. It
does not depend on Desktop injection: the Switcher restores the built-in
`openai` provider, the user restarts Codex, and Codex itself presents and
maintains ChatGPT OAuth. The Switcher cannot determine login success without
crossing the credential boundary, so the saved official card represents route
and model selection only.

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
cargo test -p codex-provider-switcher-local-proxy
cargo test -p codex-provider-switcher-desktop
pnpm tauri build
```

The last command must run natively on macOS Apple silicon, macOS Intel, and
Windows x86_64.

## Native macOS package evidence

On 2026-07-29, the v0.3.3 GitHub release workflow ran the native Web, Rust,
Tauri, and package-smoke suites on both `macos-15` Apple silicon and
`macos-15-intel`. Both jobs completed successfully and produced ad-hoc signed
DMGs:

- Apple silicon:
  `ef8c69d00b0ca5371fb794be13db7ad211e378eb6a10c6f8caff36b1aaaad188`
  (`5903173` bytes).
- Intel:
  `d05936e526a59117ef8ac2dec008d97856918ef8c59d49ac0e176c0113ca13e0`
  (`6300568` bytes).

The packages passed their CI install/remove smoke, were published in the
private v0.3.3 prerelease, downloaded again through the Releases API, and
matched both their individual and aggregate SHA-256 manifests. This is native
package evidence, not a claim of Developer ID signing, notarization, stapling,
or a complete interactive lifecycle on a maintained release Mac.

## Native Windows evidence

On 2026-07-29, version 0.3.3 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 60 Windows-applicable core tests, two Credential
Manager tests, two Windows launcher tests, six desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

Version 0.3.3 redesigns the interface as a focused two-page utility: Model
Switching remains the default task, Advanced Settings contains proxy and
recovery controls, and Add/Edit Connection uses a staged connection-to-model
flow. Semantic system colors, native fonts, visible keyboard focus, dark
appearance, and responsive cards provide the Apple-influenced visual language
without changing the provider, credential, or restore contracts.

The current-user upgrade from 0.3.2 preserved all measured Codex/Switcher
state hashes and Credential Manager metadata. The existing official route,
disabled proxy, absent port-15722 listener, and absent automatic-startup entry
were unchanged. An interactive Session 1 screenshot verified the complete dark
interface, navigation sidebar, saved-profile list, and visible version 0.3.3
label. The application remains running and responding in that user session;
the session was returned to its original disconnected state after capture.

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
- Pre-upgrade rollback snapshot:
  `D:\CodexProviderSwitcher\rollback-0.3.2-before-0.3.3-20260729T070316Z`

This deployment did not activate a provider, enable the proxy, read a
credential value, or perform a live upstream turn. Interactive official-login
and API-provider switching therefore remain release gates.

On 2026-07-26, version 0.3.2 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 60 Windows-applicable core tests, two Credential
Manager tests, two Windows launcher tests, six desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

The 0.3.1 behavior carried into this build gives the single official-login
bookmark a user-chosen display name, preserves schema-v1 bookmarks, and stores
new or replaced Windows API credentials with explicit Credential Manager
`Local` persistence. Because Codex owns one active OAuth cache, the Switcher
does not read `auth.json` to infer an email or username and does not present
several names as independently bound official accounts. A missing legacy API
credential now opens the affected connection editor and requests one
replacement Key without changing Codex configuration.

Version 0.3.2 additionally fixes normal-launch visibility: background gateway
startup remains hidden, while an ordinary launch shows and focuses the main
window after Tauri reaches `RunEvent::Ready`. An interactive Session 1
screenshot verified the rendered official-name editor and saved/current
official-route state. The current-user upgrade preserved Codex/Switcher state
and Credential Manager metadata.

- Source archive SHA-256:
  `cbc619f8f749f34f6aefcac4c034ec0dc6e76ba5183cbdc1ca91a8ae9ef14e74`
  (`399667` bytes)
- NSIS SHA-256:
  `6e86184062473a0174c52abfe5c1ad2c717154edd7c08e12ff542fbcb1b62188`
  (`3914685` bytes)
- Installed application SHA-256:
  `1392aab65b66b0b5ab911fffbd687415e0e89bc42fb547b87945f317ec334bbf`
- Authenticode status: `NotSigned`

This deployment deliberately did not rename or resave the user's official
bookmark and did not read an OAuth token or API Key. The existing API-provider
credentials were already absent from Credential Manager and cannot be
reconstructed; each affected connection must receive its Key once through the
local editor before it can be selected again.

On 2026-07-25, version 0.3.0 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 58 Windows-applicable core tests, one Credential
Manager test, two Windows launcher tests, five desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

Version 0.3.0 adds the credential-free official-account route. The Switcher
can prepare Codex's built-in `openai` provider, save only the optional current
official model choice, and later reactivate that route without reading,
copying, exporting, or rewriting Codex OAuth credentials. Native tests cover
official-route inspection, removal of route-shadowing fields, preservation of
MCP/hooks/other providers/user-owned catalog configuration, optional-model
manifests, and legacy backup compatibility.

The current-user upgrade from 0.2.4 preserved Codex/Switcher state and
Credential Manager metadata. Interactive Session 1 then started exactly one
version 0.3.0 process, with the same PID owning `127.0.0.1:15722`, restored the
quoted `--background` Run entry, removed the temporary deployment task, and
preserved the existing `claude-opus-4-8` route and proxy-state revision. The
active `cps-local` auth command references the new content-addressed helper,
whose SHA-256 equals the installed application SHA-256.

- Source archive SHA-256:
  `1f78e68d7c97d60fec9bf26f5c25baa8d912264621d0a8fcf5a373cfe3cf92c3`
- NSIS SHA-256:
  `2afba8700f6533150cb90a6ca6baced5cd52d8cf2d8621491cb12e46833d9ca5`
  (`4281055` bytes)
- Installed application and active helper SHA-256:
  `7a7cfae27580d310f443f2d0e0f342887cb07c26b5c592bef8fdb80b8dbb4d08`

The application and installer are unsigned internal artifacts. This run
deliberately did not activate the official route or change the user's current
Codex login. Interactive prepare/login/save/API-profile/official-profile
round-trip testing remains a release gate. It also did not repeat a live
upstream request, so the 0.2.2 request evidence below remains the latest proof
of the local credential-helper and proxy boundary.

On 2026-07-25, version 0.2.4 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 53 Windows-applicable core tests, one Credential
Manager test, two Windows launcher tests, four desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

Version 0.2.4 adds the saved-profile credential readback used by the local
editor. The backend accepts only an existing keyed profile ID, derives its
endpoint-bound credential account, and reads that entry from the current
user's Credential Manager. The editor shows the existing Key and can save
name/model changes without staging or rewriting an unchanged endpoint
credential. A changed Base URL or Key still requires a new staged binding.

The current-user upgrade from 0.2.3 preserved Codex/Switcher state and
Credential Manager metadata. Read-only post-install verification found one
version 0.2.4 process in interactive Session 1, that same PID owning
`127.0.0.1:15722`, the quoted `--background` Run entry, no remaining temporary
deployment task, and the active `gpt-5.6-sol` route. The `cps-local` auth
command points to the new content-addressed helper, whose SHA-256 equals the
installed application hash.

- Source archive SHA-256:
  `b2b2b455c07390c5c366ea5fc5386001b9086e17849e52fcaa8c89f4467c99c8`
- NSIS SHA-256:
  `7841590a41bbf5161cb818589629c7463cb82fdbb81109637eed10bc43eaf37a`
  (`4274546` bytes)
- Installed application and active helper SHA-256:
  `c2585f1ca16b571baca9e837c23aad78c7bb291f89a64c7e7af5095901907bb5`

The application and installer are unsigned internal artifacts. This run did
not repeat a live upstream request, so the 0.2.2 request evidence below
remains the latest proof of the local credential-helper and proxy boundary.

On 2026-07-24, version 0.2.3 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 53 Windows-applicable core tests, one Credential
Manager test, two Windows launcher tests, three desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

The current-user upgrade from 0.2.2 preserved Codex/Switcher state and
Credential Manager metadata. The installed application was restarted in
interactive Session 1, retained the selected `gpt-5.6-sol` route, owned
`127.0.0.1:15722`, retained the quoted `--background` Run entry, and refreshed
the content-addressed helper to the installed 0.2.3 executable. The saved
restart flag remains set so the new one-time customer dialog appears when the
main window is next opened.

- Source archive SHA-256:
  `52c18cdf231897acdce896d3ae58104bafe5ff80a980c96ff8146f457e1e7f5f`
- NSIS SHA-256:
  `7c8c4a781104da6d3007ad7e539668938ac734fcb4a3f9a56b7856d09f4bdac8`
  (`4270992` bytes)
- Installed application SHA-256:
  `d7e9e9bcf73ee7d5f2f7e25276051df8d4f2463a70202f00c48dce1419a2d9ea`

The application and installer are unsigned internal artifacts. This run did
not repeat the live upstream request, so the 0.2.2 request evidence below
remains the latest proof of the local credential-helper and proxy boundary.

On 2026-07-25, version 0.2.2 on Windows 11 x64 build 22631 on `ydy001`
passed the TypeScript check, four runtime tests, production WebView build,
Rust formatting check, 53 Windows-applicable core tests, one Credential
Manager test, two Windows launcher tests, two desktop tests, six local-proxy
unit tests, five local-proxy integration tests, the full Tauri/NSIS build, and
isolated NSIS install/remove smoke.

The upgrade preserved Codex/Switcher state and Credential Manager metadata,
installed the application as version 0.2.2, refreshed the managed helper to
the content-addressed 0.2.2 executable, and repaired a missing current-user
Run entry after the proxy started. A second `--background` launch in interactive
Session 1 restored the listener on `127.0.0.1:15722`.

An official npm `codex-cli 0.145.0` request executed in the same interactive
session no longer returned a local proxy 401 or credential-helper binding
error. It reached the configured upstream and returned HTTP 503 because the
selected `claude-opus-4-8` route had no available channel. That result proves
the local bearer injection path, not successful upstream model availability.

- Source archive SHA-256:
  `79834566f5572fcc6906e1cb00440636f6823847a8b515d58195e3e5b3f5863f`
- NSIS SHA-256:
  `e2aa9d9263e31ed1d8e16faae8cb7865bbfc1ea82a3314ca5f48a6e70621d987`
  (`4271429` bytes)
- Installed application SHA-256:
  `ba38a88deae96789fb694ab762240e289b380a49a499b24e7dabeab1adb371d7`

The application and installer are unsigned internal artifacts.

On 2026-07-23, the earlier phase-2 source snapshot on Windows 11 x64 build
22631 on `ydy001` passed the TypeScript check, four runtime tests, production
WebView build, Rust formatting check, 42 core tests, one Credential Manager
test, one Windows launcher test, desktop `cargo check`, the full Tauri/NSIS
build, and isolated NSIS install/remove smoke.

That evidence is retained as historical validation of the earlier
configuration path. The newer 0.2.2 evidence above supersedes it for helper
migration, proxy startup, upgrade, and background relaunch, but not for the
remaining release gates.
