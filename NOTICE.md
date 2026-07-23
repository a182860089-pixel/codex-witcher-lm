# Notices and provenance

This repository is a clean-room implementation. No source code, art, branding,
or packaged assets were copied from CodexPlusPlus, codex-plusplus, or
Codex-Dream-Skin. CC Switch was reviewed as an MIT-licensed behavioral and UX
reference; no CC Switch code, assets, database schema, or packaged components
were copied or bundled.

Behavioral and protocol references:

- OpenAI Codex, Apache-2.0:
  https://github.com/openai/codex
- BigPizzaV3/CodexPlusPlus, AGPL-3.0-only, behavioral reference only:
  https://github.com/BigPizzaV3/CodexPlusPlus
- b-nnett/codex-plusplus, MIT, patch-loader comparison only:
  https://github.com/b-nnett/codex-plusplus
- Fei-Away/Codex-Dream-Skin, lifecycle/security reference only:
  https://github.com/Fei-Away/Codex-Dream-Skin
- farion1231/cc-switch, MIT, behavioral reference for provider import, model
  discovery, and quick-switch UX only:
  https://github.com/farion1231/cc-switch

Primary implementation libraries include Tauri 2, `toml_edit`, `keyring-rs`,
`reqwest`, `fs4`, `serde`, `tempfile`, `sha2`, and `url`. Their exact versions
are locked in `Cargo.lock` and `pnpm-lock.yaml`; a release must generate and
review a full third-party license report.
