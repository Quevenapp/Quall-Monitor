#!/bin/bash
# Direct-download bundle and ZIP. This script never installs, uploads, or creates store packages.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MACOS="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$MACOS/../.." && pwd)"
OUT="$ROOT/dist/macos"
APP="$OUT/Quall Monitor.app"
JOBS="${QUALL_MONITOR_JOBS:-2}"
ARCHITECTURES="${QUALL_MONITOR_ARQUITETURAS:-host}"
IDENTITY="${QUALL_MONITOR_IDENTIDADE:--}"
VERSION="${QUALL_MONITOR_VERSAO:-0.1.0}"
BUILD="${QUALL_MONITOR_BUILD:-1}"
case "$ARCHITECTURES" in host|universal) ;; *) echo "QUALL_MONITOR_ARQUITETURAS deve ser host ou universal" >&2; exit 1 ;; esac

# Never ship an override from another repository. Explicit ready mode only reuses this repo's
# nucleus after the operator has built it; CI and the default invocation always run Cargo.
if [ "${QUALL_MONITOR_NUCLEO_PRONTO:-nao}" != sim ]; then
    if [ "$ARCHITECTURES" = universal ]; then
        SLICES=()
        for TARGET in aarch64-apple-darwin x86_64-apple-darwin; do
            (cd "$ROOT" && MACOSX_DEPLOYMENT_TARGET=13.0 CARGO_PROFILE_RELEASE_LTO=false \
                cargo build -p quall-ffi --release --locked --target "$TARGET" --jobs "$JOBS")
            SLICES+=("$ROOT/target/$TARGET/release/libquall.a")
        done
        mkdir -p "$ROOT/target/release"
        lipo -create "${SLICES[@]}" -output "$ROOT/target/release/libquall.a"
    else
        (cd "$ROOT" && MACOSX_DEPLOYMENT_TARGET=13.0 CARGO_PROFILE_RELEASE_LTO=false \
            cargo build -p quall-ffi --release --locked --jobs "$JOBS")
    fi
fi
[ -s "$ROOT/target/release/libquall.a" ] || { echo "Núcleo ausente neste repositório" >&2; exit 1; }
ARGS=()
if [ "$ARCHITECTURES" = universal ]; then ARGS=(--arch arm64 --arch x86_64); fi
export QUALL_MONITOR_LIBQUALL="$ROOT/target/release/libquall.a"
(cd "$MACOS" && xcrun swift build -c release --jobs "$JOBS" ${ARGS[@]+"${ARGS[@]}"} --product quall-monitor-app)
(cd "$MACOS" && xcrun swift build -c release --jobs "$JOBS" ${ARGS[@]+"${ARGS[@]}"} --product quall-monitor-display)
BIN="$(cd "$MACOS" && xcrun swift build -c release ${ARGS[@]+"${ARGS[@]}"} --show-bin-path)"

mkdir -p "$OUT"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN/quall-monitor-app" "$BIN/quall-monitor-display" "$APP/Contents/MacOS/"
cp "$HERE/Info.plist" "$APP/Contents/Info.plist"
cp "$HERE/QuallMonitor.icns" "$APP/Contents/Resources/"
cp -R "$BIN/QuallMonitor_QuallIdiomaKit.bundle" "$APP/Contents/Resources/"
for FILE in LICENSE LICENSE-SCOPE.md NOTICE.txt THIRD_PARTY_NOTICES.txt; do
    [ -s "$ROOT/$FILE" ] || { echo "Aviso/licença ausente: $FILE" >&2; exit 1; }
    cp "$ROOT/$FILE" "$APP/Contents/Resources/"
done
if [ -f "$ROOT/SOURCE-REVISION.txt" ]; then
    cp "$ROOT/SOURCE-REVISION.txt" "$APP/Contents/Resources/SOURCE-REVISION.txt"
else
    {
        printf 'Repository: https://github.com/Quevenapp/Quall-Monitor\n'
        printf 'Revision: '
        git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf 'uncommitted\n'
        [ -z "$(git -C "$ROOT" status --porcelain)" ] || printf 'Working tree contains uncommitted changes.\n'
    } > "$APP/Contents/Resources/SOURCE-REVISION.txt"
fi
printf 'APPL????' > "$APP/Contents/PkgInfo"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD" "$APP/Contents/Info.plist"
SIGN=()
if [ "$IDENTITY" != - ]; then SIGN=(--options runtime --timestamp); fi
codesign --force --sign "$IDENTITY" ${SIGN[@]+"${SIGN[@]}"} "$APP/Contents/MacOS/quall-monitor-display"
codesign --force --sign "$IDENTITY" ${SIGN[@]+"${SIGN[@]}"} "$APP"
codesign --verify --deep --strict "$APP"
plutil -lint "$APP/Contents/Info.plist"
if [ "$ARCHITECTURES" = universal ]; then
    DOWNLOAD_ARCH=universal
else
    case "$(uname -m)" in
        arm64|aarch64) DOWNLOAD_ARCH=arm64 ;;
        x86_64) DOWNLOAD_ARCH=x64 ;;
        *) echo "Arquitetura de download não reconhecida" >&2; exit 1 ;;
    esac
fi
ZIP="$OUT/Quall-Monitor-$VERSION-macos-$DOWNLOAD_ARCH.zip"
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")") > "$ZIP.sha256"
printf 'App: %s\nDownload: %s\n' "$APP" "$ZIP"
