Codex Provider Switcher {{VERSION}} adds independently verified Codex account
status plus Codex-owned browser login and logout to the downloadable native
desktop packages.

> **Preview release:** the Windows installer is unsigned. The macOS DMGs use
> an ad-hoc signature but are not Developer ID signed or notarized. Read the
> first-launch notes below before installing.

## What's new

- Adds bounded upstream retries for connection failures, timeouts, HTTP 429,
  502, 503, and 504 responses, with exponential backoff and a configurable
  retry limit in Advanced Settings.
- Adds request diagnostics for retry count, time to first byte, response
  bytes, stream completion, and mid-stream failure reasons. Requests are not
  replayed after SSE output has started.
- Distinguishes the configured provider route from the active Codex
  authentication mode through App Server `account/read`.
- Adds Codex-owned ChatGPT browser login, explicit logout, and optional
  email/plan display without reading or storing OAuth tokens.
- Makes the local fast-switch path the default for API connections and
  separates official login from API setup in Add Connection.

## Choose your download

| Computer | Download |
| --- | --- |
| Windows x64 (tested on Windows 11) | `Codex.Provider.Switcher_{{VERSION}}_Windows-x64-Setup.exe` |
| Mac with Apple silicon (M1/M2/M3/M4…) | `Codex.Provider.Switcher_{{VERSION}}_macOS-arm64.dmg` |
| Mac with an Intel processor | `Codex.Provider.Switcher_{{VERSION}}_macOS-x64.dmg` |

`SHA256SUMS.txt` contains the checksum for every installer. The individual
`.sha256` files can be used to verify one download.

## Install

### Windows

1. Download the `Windows-x64-Setup.exe` file and run it.
2. Because this preview is unsigned, Microsoft Defender SmartScreen may
   appear. Choose **More info → Run anyway** only after confirming that the
   checksum matches this release.
3. The installer uses the current Windows user and does not require a
   machine-wide installation.

### macOS

1. Choose the Apple silicon or Intel DMG for your Mac.
2. Open the DMG and copy **Codex Provider Switcher** into Applications.
3. Because this preview is not notarized, the first launch may require
   **System Settings → Privacy & Security → Open Anyway**.

## First use

1. Open the Switcher; it automatically reads the current non-secret Codex
   provider/model and independently checks the active Codex authentication
   mode through App Server.
2. Choose **Add connection**, then select **Official login** or
   **API connection**.
3. For an API connection, enter a friendly name, Base URL, and API Key, fetch
   and select models, then choose **Save and use**. The first fast-switch
   activation may ask you to reopen Codex once; later saved-model changes apply
   to the next turn while the Switcher is running.
4. For official login, complete the Codex-owned browser flow. The Switcher
   displays optional email/plan metadata returned by App Server and requires a
   full Codex restart; it never stores an OAuth token.

API Keys stay in macOS Keychain or Windows Credential Manager. The Switcher
does not copy Codex's official OAuth token or modify the signed Codex Desktop
package.

This is a desktop companion for Codex on macOS and Windows. It is not an iOS
application and cannot manage an iPhone or iPad Codex configuration.
