#!/bin/bash
set -euo pipefail

shopt -s nullglob
dmgs=(target/release/bundle/dmg/*.dmg)
if [[ ${#dmgs[@]} -ne 1 ]]; then
  echo "expected exactly one macOS DMG, found ${#dmgs[@]}" >&2
  exit 1
fi

mount_root="$(mktemp -d)"
install_root="$(mktemp -d)"
mounted=0
cleanup() {
  if [[ "$mounted" -eq 1 ]]; then
    /usr/bin/hdiutil detach "$mount_root" -quiet || true
  fi
  /bin/rm -rf "$mount_root" "$install_root"
}
trap cleanup EXIT

/usr/bin/hdiutil attach -readonly -nobrowse -mountpoint "$mount_root" "${dmgs[0]}" >/dev/null
mounted=1
apps=("$mount_root"/*.app)
if [[ ${#apps[@]} -ne 1 ]]; then
  echo "expected exactly one application in the DMG" >&2
  exit 1
fi

installed_app="$install_root/Codex Provider Switcher.app"
/usr/bin/ditto "${apps[0]}" "$installed_app"
identifier="$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - "$installed_app/Contents/Info.plist")"
if [[ "$identifier" != "dev.codex-provider-switcher.desktop" ]]; then
  echo "unexpected installed bundle identifier" >&2
  exit 1
fi
if [[ ! -x "$installed_app/Contents/MacOS/codex-provider-switcher" ]]; then
  echo "installed app executable is missing" >&2
  exit 1
fi

/bin/rm -rf "$installed_app"
if [[ -e "$installed_app" ]]; then
  echo "macOS uninstall simulation left the app behind" >&2
  exit 1
fi
