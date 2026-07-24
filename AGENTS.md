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
  API keys.
- The default switching mode is the loopback local proxy. Its first enable
  transactionally points Codex at the managed `cps-local` provider and may
  require a full Codex restart. Once that provider is active, changing a saved
  connection or model atomically updates the proxy route and applies to the
  next turn; when both identifiers are available, requests sharing the same
  `thread_id` and `turn_id` remain pinned to one route.
- Keep direct configuration as an explicit fallback. Direct switches edit the
  selected provider/model transactionally and require a full Codex restart and
  a new thread.
- Do not expose internal hashes, catalog metadata, proxy tokens, CDP details,
  or file-system paths in the normal novice flow.

## Safety invariants

- Prefer documented Codex configuration and App Server requests.
- Never write API keys to `config.toml`, `auth.json`, logs, state, or command
  arguments. Use macOS Keychain or Windows Credential Manager.
- Model discovery uses the system proxy, permits HTTPS plus loopback HTTP,
  follows no redirects, and bounds response size and model count.
- Discovery credentials may exist only in the zeroizing in-process vault:
  at most eight live sessions and ten minutes per session.
- The local proxy binds only to `127.0.0.1` on its managed fixed port. Codex
  authenticates with a separate random entry bearer stored in the OS credential
  manager; this token is not an upstream provider key.
- Validate the entry bearer using a constant-time digest comparison. Strip the
  incoming authorization, credential-like, forwarding, and hop-by-hop headers,
  including names declared by `Connection`, before injecting the selected
  upstream bearer.
- Proxy only `POST /responses`, `POST /responses/compact`, and their `/v1`
  aliases. Serve authenticated `/models`, `/v1/models`, and `/health`. Overwrite
  the request model with the active route, forward response bodies as byte
  streams, and do not follow redirects or automatically retry requests.
- Atomically swap immutable routes. Pin a route for each bounded
  `(thread_id, turn_id)` pair so a switch cannot split one turn across
  providers. Keep the pin table bounded.
- `proxy.json` schema v2 contains only enabled state, selected profile/model,
  fixed port, revision, restart notice, and the credential-free
  `activationTransactionId` naming the exact proxy-activation backup. Hot Route
  switches preserve that ID; disabled state clears it. It must not contain
  either bearer.
- Do not add `model_catalog_json`. Preserve a user-owned value unchanged.
  The Switcher-managed `models.json` is internal transaction companion data,
  not a Codex catalog pointer.
- Every config edit must be planned against a content hash, written atomically,
  and backed up exactly. Restore exact bytes when the applied hashes still
  match. Proxy disable may otherwise remove only the verified Switcher-owned
  provider/model fields while preserving unrelated valid TOML, using the same
  compare-and-swap boundary and a `Detaching` journal state.
- First proxy activation must preallocate a non-nil transaction UUID and apply
  the config plan under exactly that backup directory. Disable must restore only
  the validated Applied `cps-local` manifest named by that activation, never an
  unrelated latest backup. A change to any Switcher-owned proxy field requires
  manual review.
- CDP is opt-in, loopback-only, bound to a verified app/process/browser
  identity, and disabled for unknown Desktop builds.
- Do not add broad renderer monkey patches. A build adapter must identify the
  exact App Server bridge it transforms.
- The local proxy may select a new upstream route at the next turn of an
  existing thread. Do not claim that cross-provider thread history is portable;
  recommend a new thread when changing providers. CDP `thread/start`
  transforms, if ever enabled for a reviewed build, still apply only to new
  threads.

## Repository layout

- `crates/switcher-core/`: provider/model validation, discovery, saved-profile
  storage, config transactions, CDP validation, and App Server transforms.
- `crates/local-proxy/`: loopback Responses proxy, route pinning, local model
  catalog, entry authentication, header sanitation, and SSE forwarding.
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
cargo test -p codex-provider-switcher-core \
  -p codex-provider-switcher-credentials \
  -p codex-provider-switcher-launcher \
  -p codex-provider-switcher-local-proxy \
  -p codex-provider-switcher-desktop
```

Packaging must also pass unsigned test builds on both macOS and Windows.
Release artifacts require platform signing and target-host smoke tests.
