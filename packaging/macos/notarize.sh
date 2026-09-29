#!/usr/bin/env bash
# Notarizes a signed Malgel.app or .dmg with Apple and staples the ticket.
#
#   packaging/macos/notarize.sh <Malgel.app | Malgel.dmg>
#
# Credentials come from the environment, one of:
#   APPLE_API_KEY (base64 .p8), APPLE_API_KEY_ID, APPLE_API_ISSUER
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_SPECIFIC_PASSWORD
set -euo pipefail

if (($# != 1)); then
  echo "usage: $0 <Malgel.app | Malgel.dmg>" >&2
  exit 2
fi
target="$1"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER:-}" ]]; then
  key="$work/AuthKey.p8"
  printf '%s' "$APPLE_API_KEY" | base64 --decode >"$key"
  auth=(--key "$key" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER")
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]]; then
  auth=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD")
else
  echo "notarize.sh: no notarization credentials in the environment" >&2
  exit 1
fi

# The notary service takes zip, dmg and pkg files; an app goes up as a zip.
upload="$target"
if [[ -d "$target" ]]; then
  upload="$work/$(basename "$target").zip"
  ditto -c -k --keepParent "$target" "$upload"
fi

result="$work/result.json"
xcrun notarytool submit "$upload" "${auth[@]}" --wait --timeout 1h \
  --output-format json >"$result" || true
status="$(jq -r '.status // empty' "$result")"
id="$(jq -r '.id // empty' "$result")"
echo "Notarization of $(basename "$target"): ${status:-no status} (${id:-no submission id})"
if [[ "$status" != "Accepted" ]]; then
  cat "$result" >&2
  if [[ -n "$id" ]]; then
    xcrun notarytool log "$id" "${auth[@]}" >&2 || true
  fi
  exit 1
fi

xcrun stapler staple "$target"
xcrun stapler validate "$target"
