#!/bin/bash
# Notarizes an already signed .app using an existing Keychain profile, then replaces the ZIP.
# No credentials are created or read by this script.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
APP="${1:-$ROOT/dist/macos/Quall Monitor.app}"
APP="${APP%/}"
PROFILE="${QUALL_MONITOR_PERFIL_NOTARIZACAO:?Defina QUALL_MONITOR_PERFIL_NOTARIZACAO com o nome de um perfil já salvo no Keychain}"
case "$APP" in *.app) ;; *) echo "Informe o caminho de Quall Monitor.app" >&2; exit 1 ;; esac
[ -d "$APP" ] || { echo "App não encontrado: $APP" >&2; exit 1; }
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
OUT="$(dirname "$APP")"
MAIN="$APP/Contents/MacOS/quall-monitor-app"
HELPER="$APP/Contents/MacOS/quall-monitor-display"
[ -x "$MAIN" ] && [ -x "$HELPER" ] || { echo "Executável principal ou helper ausente" >&2; exit 1; }
codesign --verify --deep --strict "$APP"

verify_distribution_signature() {
    local TARGET="$1"
    local DETAILS
    DETAILS="$(codesign -dv --verbose=4 "$TARGET" 2>&1)"
    printf '%s\n' "$DETAILS" | grep -q '^Authority=Developer ID Application:' || {
        echo "É necessária assinatura Developer ID Application: $TARGET" >&2; exit 1;
    }
    printf '%s\n' "$DETAILS" | grep -q 'flags=.*runtime' || {
        echo "Hardened runtime ausente: $TARGET" >&2; exit 1;
    }
    printf '%s\n' "$DETAILS" | grep -q '^Timestamp=' || {
        echo "Timestamp seguro ausente: $TARGET" >&2; exit 1;
    }
    printf '%s\n' "$DETAILS" | sed -n 's/^TeamIdentifier=//p'
}
APP_TEAM="$(verify_distribution_signature "$APP")"
HELPER_TEAM="$(verify_distribution_signature "$HELPER")"
[ -n "$APP_TEAM" ] && [ "$APP_TEAM" = "$HELPER_TEAM" ] || {
    echo "App e helper precisam ser assinados pelo mesmo titular" >&2; exit 1;
}
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ARCHS="$(lipo -archs "$MAIN")"
case "$ARCHS" in
    arm64) ARCH=arm64 ;;
    x86_64) ARCH=x64 ;;
    'arm64 x86_64'|'x86_64 arm64') ARCH=universal ;;
    *) echo "Arquiteturas não suportadas no app: $ARCHS" >&2; exit 1 ;;
esac
ZIP="$OUT/Quall-Monitor-$VERSION-macos-$ARCH.zip"
TEMP="$(mktemp -d "$OUT/.notarizacao.XXXXXX")"
trap 'rm -rf "$TEMP"' EXIT
RESULT="$OUT/notarizacao-$VERSION-$ARCH-$(date -u +%Y%m%dT%H%M%SZ).json"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$TEMP/submissao.zip"
echo "Enviando o app assinado para notarização pelo perfil salvo."
xcrun notarytool submit "$TEMP/submissao.zip" --keychain-profile "$PROFILE" --wait \
    --output-format json > "$RESULT"
STATUS="$(plutil -extract status raw -o - "$RESULT")"
[ "$STATUS" = Accepted ] || {
    echo "Notarização não aceita (status=$STATUS). Relato: $RESULT" >&2; exit 1;
}
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
codesign --verify --deep --strict "$APP"
# Only publish the replacement after acceptance, stapling, validation, and complete ZIP creation.
ditto -c -k --sequesterRsrc --keepParent "$APP" "$TEMP/final.zip"
mv -f "$TEMP/final.zip" "$ZIP"
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")") > "$TEMP/final.sha256"
mv -f "$TEMP/final.sha256" "$ZIP.sha256"
printf 'Notarização: Accepted\nApp com ticket: %s\nDownload: %s\nRelato: %s\n' "$APP" "$ZIP" "$RESULT"
