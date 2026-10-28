#!/bin/bash
# Creates an optional PKG that places Quall Monitor.app in /Applications, plus the matching
# uninstaller as a separate .command file. Does not install, upload, or notarize.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
APP="${1:-$ROOT/dist/macos/Quall Monitor.app}"
APP="${APP%/}"
[ -d "$APP" ] || { echo "App não encontrado: $APP" >&2; exit 1; }
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
OUT="$(dirname "$APP")"
# Developer ID Installer identity, when available. Empty keeps the package unsigned.
INSTALLER_IDENTITY="${QUALL_MONITOR_IDENTIDADE_INSTALADOR:-}"
codesign --verify --deep --strict "$APP"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ARCHS="$(lipo -archs "$APP/Contents/MacOS/quall-monitor-app")"
case "$ARCHS" in
    arm64) ARCH=arm64; HOSTS=arm64 ;;
    x86_64) ARCH=x64; HOSTS=x86_64 ;;
    'arm64 x86_64'|'x86_64 arm64') ARCH=universal; HOSTS=arm64,x86_64 ;;
    *) echo "Arquiteturas não suportadas: $ARCHS" >&2; exit 1 ;;
esac
PKG="$OUT/Quall-Monitor-$VERSION-macos-$ARCH.pkg"
UNINSTALL="$OUT/Quall-Monitor-$VERSION-desinstalar.command"
TEMP="$(mktemp -d "$OUT/.pkg.XXXXXX")"
trap 'rm -rf "$TEMP"' EXIT

mkdir -p "$TEMP/raiz"
ditto "$APP" "$TEMP/raiz/Quall Monitor.app"
pkgbuild --analyze --root "$TEMP/raiz" "$TEMP/componentes.plist" >/dev/null
# Always install to /Applications: never relocate onto another copy found by Spotlight.
plutil -replace 0.BundleIsRelocatable -bool NO "$TEMP/componentes.plist"
plutil -replace 0.BundleIsVersionChecked -bool NO "$TEMP/componentes.plist"
pkgbuild --quiet --root "$TEMP/raiz" --component-plist "$TEMP/componentes.plist" \
    --identifier br.com.queven.quall.monitor.pkg --version "$VERSION" \
    --install-location /Applications "$TEMP/componente.pkg"
cat > "$TEMP/distribution.xml" <<DIST
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>Quall Monitor $VERSION</title>
    <options customize="never" require-scripts="false" hostArchitectures="$HOSTS"/>
    <domains enable_localSystem="true" enable_currentUserHome="false" enable_anywhere="false"/>
    <volume-check><allowed-os-versions><os-version min="13.0"/></allowed-os-versions></volume-check>
    <choices-outline><line choice="monitor"/></choices-outline>
    <choice id="monitor" visible="false"><pkg-ref id="br.com.queven.quall.monitor.pkg"/></choice>
    <pkg-ref id="br.com.queven.quall.monitor.pkg" version="$VERSION" onConclusion="none">componente.pkg</pkg-ref>
</installer-gui-script>
DIST
SIGN=()
if [ -n "$INSTALLER_IDENTITY" ]; then SIGN=(--sign "$INSTALLER_IDENTITY" --timestamp); fi
productbuild --quiet --distribution "$TEMP/distribution.xml" --package-path "$TEMP" \
    ${SIGN[@]+"${SIGN[@]}"} "$TEMP/instalador.pkg"
mv -f "$TEMP/instalador.pkg" "$PKG"
install -m 0755 "$HERE/desinstalar.sh" "$UNINSTALL"
(cd "$OUT" && shasum -a 256 "$(basename "$PKG")") > "$PKG.sha256"
(cd "$OUT" && shasum -a 256 "$(basename "$UNINSTALL")") > "$UNINSTALL.sha256"
printf 'PKG: %s\nDesinstalador: %s\n' "$PKG" "$UNINSTALL"
