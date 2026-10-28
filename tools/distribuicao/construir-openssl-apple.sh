#!/bin/bash
# SPDX-License-Identifier: MPL-2.0
# Uses the Cargo.lock OpenSSL source without file-based configuration or entropy.
# DTLS/SRTP keys and certificates remain generated in memory with Apple's CSRNG.
set -euo pipefail
SOURCE="${1:?OpenSSL source}"; OUT="${2:?build output}"; TARGET="${3:?Rust target}"
JOBS="${NUM_JOBS:-2}"
RECIPES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "$TARGET" in
  aarch64-apple-ios) CONFIG=ios64-xcrun; SDK=iphoneos; MIN=-miphoneos-version-min=15.0 ;;
  aarch64-apple-ios-sim) CONFIG=iossimulator-arm64-xcrun; SDK=iphonesimulator; MIN=-mios-simulator-version-min=15.0 ;;
  x86_64-apple-ios) CONFIG=iossimulator-x86_64-xcrun; SDK=iphonesimulator; MIN=-mios-simulator-version-min=15.0 ;;
  aarch64-apple-darwin) CONFIG=darwin64-arm64-cc; SDK=macosx; MIN=-mmacosx-version-min=13.0 ;;
  x86_64-apple-darwin) CONFIG=darwin64-x86_64-cc; SDK=macosx; MIN=-mmacosx-version-min=13.0 ;;
  *) echo "Unsupported Apple target: $TARGET" >&2; exit 64 ;;
esac
case "$TARGET" in aarch64-*) ARCH=arm64 ;; x86_64-*) ARCH=x86_64 ;; esac
case "$OUT" in /*) ;; *) echo "Build output must be absolute" >&2; exit 64 ;; esac
BUILD="$OUT/openssl-apple/build"; INSTALL="$OUT/openssl-apple/install"
# Fresh source/configuration every time: never mix an older OpenSSL source or
# installed archive with a new Cargo.lock or a changed privacy recipe.
case "$SOURCE" in "$BUILD"|"$BUILD"/*|"$INSTALL"|"$INSTALL"/*)
  echo "OpenSSL source overlaps build output" >&2; exit 64 ;;
esac
rm -rf "$BUILD" "$INSTALL"
mkdir -p "$BUILD" "$INSTALL"
cp -R "$SOURCE/." "$BUILD/"
cd "$BUILD"
patch --batch -p1 < "$RECIPES/openssl-apple-sem-arquivos.patch"
export CC="$(xcrun --sdk "$SDK" --find clang)"
export AR="$(xcrun --sdk "$SDK" --find ar)"
export RANLIB="$(xcrun --sdk "$SDK" --find ranlib)"
unset CROSS_COMPILE
perl Configure "$CONFIG" no-shared no-module no-tests no-comp no-zlib no-zlib-dynamic \
  no-stdio no-posix-io no-ui-console no-autoload-config no-dso --with-rand-seed=getrandom \
  --prefix="$INSTALL" --openssldir="$INSTALL/etc/ssl" \
  -isysroot "$(xcrun --sdk "$SDK" --show-sdk-path)" "$MIN"
make -j "$JOBS" build_libs
make install_dev
# A compile-time gate: no /dev/{u,}random fallback, config autoload or file APIs.
"$CC" -arch "$ARCH" -isysroot "$(xcrun --sdk "$SDK" --show-sdk-path)" "$MIN" \
  -I"$BUILD/include" -I"$INSTALL/include" -dM -E -x c \
  -include crypto/rand.h /dev/null > "$OUT/apple-crypto-macros.txt"
python3 - "$INSTALL/include/openssl/configuration.h" "$OUT/apple-crypto-macros.txt" <<'PY'
import pathlib, re, sys
s = pathlib.Path(sys.argv[1]).read_text()
required = ['OPENSSL_NO_STDIO', 'OPENSSL_NO_POSIX_IO', 'OPENSSL_NO_UI_CONSOLE', 'OPENSSL_NO_AUTOLOAD_CONFIG',
            'OPENSSL_RAND_SEED_GETRANDOM']
missing = [m for m in required if not re.search(r'^\s*#\s*define\s+' + m + r'\b', s, re.M)]
macros = pathlib.Path(sys.argv[2]).read_text()
if not re.search(r'^\s*#\s*define\s+OPENSSL_APPLE_CRYPTO_RANDOM\b', macros, re.M):
    missing.append('OPENSSL_APPLE_CRYPTO_RANDOM')
forbidden = ['OPENSSL_RAND_SEED_DEVRANDOM', 'OPENSSL_RAND_SEED_OS']
present = [m for m in forbidden if re.search(r'^\s*#\s*define\s+' + m + r'\b', s, re.M)]
if missing or present:
    raise SystemExit(f'Invalid Apple crypto configuration: missing={missing}, forbidden={present}')
PY
python3 - "$INSTALL/lib/libcrypto.a" <<'PY'
import subprocess, sys
result = subprocess.run(['nm', '-u', sys.argv[1]], capture_output=True, text=True, check=True)
imports = {line.split()[-1] for line in result.stdout.splitlines() if line.split()}
forbidden = {'_stat', '_fstat', '_fstatat', '_lstat', '_statfs', '_fstatfs',
             '_statvfs', '_fstatvfs', '_getattrlist', '_getattrlistbulk', '_getattrlistat', '_fgetattrlist',
             '_mach_absolute_time', '_OBJC_CLASS_$_NSUserDefaults', '_OBJC_CLASS_$_UITextInputMode',
             '_open', '_fopen'}
unexpected = sorted(imports & forbidden)
if unexpected:
    raise SystemExit(f'Unexpected file APIs in Apple crypto archive: {unexpected}')
if '_CCRandomGenerateBytes' not in imports:
    raise SystemExit('Apple CSRNG missing from crypto archive')
PY
