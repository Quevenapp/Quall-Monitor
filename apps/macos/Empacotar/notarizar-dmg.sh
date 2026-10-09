#!/bin/bash
# Input must be a signed DMG containing the previously stapled app.
set -euo pipefail
DMG="${1:?Informe o caminho do instalador .dmg assinado}"
PROFILE="${QUALL_MONITOR_PERFIL_NOTARIZACAO:?Defina QUALL_MONITOR_PERFIL_NOTARIZACAO com um perfil existente no Keychain}"
[ -f "$DMG" ] || { echo "DMG não encontrado: $DMG" >&2; exit 1; }
DMG="$(cd "$(dirname "$DMG")" && pwd)/$(basename "$DMG")"
OUT="$(dirname "$DMG")"
codesign --verify --strict "$DMG"
DETAILS="$(codesign -dv --verbose=4 "$DMG" 2>&1)"
printf '%s\n' "$DETAILS" | grep -q '^Authority=Developer ID Application:' || { echo 'DMG precisa de Developer ID Application' >&2; exit 1; }
printf '%s\n' "$DETAILS" | grep -q '^Timestamp=' || { echo 'DMG precisa de timestamp seguro' >&2; exit 1; }
DMG_TEAM="$(printf '%s\n' "$DETAILS" | sed -n 's/^TeamIdentifier=//p')"
TEMP="$(mktemp -d "$OUT/.notarizacao-dmg.XXXXXX")"
MOUNT="$TEMP/volume"
MOUNTED=nao
cleanup() {
    if [ "$MOUNTED" = sim ]; then hdiutil detach -quiet "$MOUNT" || true; fi
    rm -rf "$TEMP"
}
trap cleanup EXIT
mkdir "$MOUNT"
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$MOUNT" "$DMG"
MOUNTED=sim
APP="$MOUNT/Quall Monitor.app"
[ -d "$APP" ] && [ -L "$MOUNT/Applications" ] || { echo 'Conteúdo do instalador incompleto' >&2; exit 1; }
codesign --verify --deep --strict "$APP"
xcrun stapler validate "$APP"
APP_TEAM="$(codesign -dv --verbose=4 "$APP" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
[ -n "$DMG_TEAM" ] && [ "$DMG_TEAM" = "$APP_TEAM" ] || { echo 'App e DMG precisam do mesmo titular' >&2; exit 1; }
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")" = br.com.queven.quall.monitor ] || {
    echo 'Identidade do app inesperada' >&2; exit 1;
}
hdiutil detach -quiet "$MOUNT"
MOUNTED=nao
RESULT="$OUT/notarizacao-dmg-$(basename "$DMG" .dmg)-$(date -u +%Y%m%dT%H%M%SZ).json"
xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait --output-format json > "$RESULT"
STATUS="$(plutil -extract status raw -o - "$RESULT")"
[ "$STATUS" = Accepted ] || { echo "Notarização não aceita: $STATUS. Relato: $RESULT" >&2; exit 1; }
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
codesign --verify --strict "$DMG"
(cd "$OUT" && shasum -a 256 "$(basename "$DMG")") > "$DMG.sha256"
printf 'Notarização DMG: Accepted\nInstalador com ticket: %s\nRelato: %s\n' "$DMG" "$RESULT"
