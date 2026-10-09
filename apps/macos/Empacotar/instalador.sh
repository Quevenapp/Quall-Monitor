#!/bin/bash
# Creates a drag-to-Applications DMG from the built app. Does not install or notarize.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
APP="${1:-$ROOT/dist/macos/Quall Monitor.app}"
APP="${APP%/}"
[ -d "$APP" ] || { echo "App não encontrado: $APP" >&2; exit 1; }
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
OUT="$(dirname "$APP")"
IDENTITY="${QUALL_MONITOR_IDENTIDADE:--}"
codesign --verify --deep --strict "$APP"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ARCHS="$(lipo -archs "$APP/Contents/MacOS/quall-monitor-app")"
case "$ARCHS" in
    arm64) ARCH=arm64 ;;
    x86_64) ARCH=x64 ;;
    'arm64 x86_64'|'x86_64 arm64') ARCH=universal ;;
    *) echo "Arquiteturas não suportadas: $ARCHS" >&2; exit 1 ;;
esac
DMG="$OUT/Quall-Monitor-$VERSION-macos-$ARCH.dmg"
TEMP="$(mktemp -d "$OUT/.instalador.XXXXXX")"
trap 'rm -rf "$TEMP"' EXIT
mkdir "$TEMP/conteudo"
ditto "$APP" "$TEMP/conteudo/Quall Monitor.app"
ln -s /Applications "$TEMP/conteudo/Applications"
cat > "$TEMP/conteudo/Instalar - Install.txt" <<'INSTRUCTIONS'
Português
Arraste Quall Monitor.app para Applications (Aplicativos).
Abra o app em Aplicativos. Ao usar Estender tela, permita Gravação da Tela e Rede Local quando o macOS pedir.
Use PT | EN para escolher o idioma. Desconecte os monitores e feche o app antes de atualizar.

English
Drag Quall Monitor.app to Applications.
Open the app from Applications. When using Extend display, allow Screen Recording and Local Network when macOS asks.
Use PT | EN to choose your language. Disconnect displays and quit the app before updating.
INSTRUCTIONS
hdiutil create -quiet -volname 'Quall Monitor' -srcfolder "$TEMP/conteudo" -format UDZO -fs HFS+ "$TEMP/instalador.dmg"
if [ "$IDENTITY" != - ]; then
    codesign --force --sign "$IDENTITY" --timestamp "$TEMP/instalador.dmg"
    codesign --verify --strict "$TEMP/instalador.dmg"
fi
hdiutil verify -quiet "$TEMP/instalador.dmg"
mv -f "$TEMP/instalador.dmg" "$DMG"
(cd "$OUT" && shasum -a 256 "$(basename "$DMG")") > "$DMG.sha256"
printf 'Instalador: %s\n' "$DMG"
