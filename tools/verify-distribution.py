#!/usr/bin/env python3
"""Check the public source snapshot and driver provenance before packaging."""
import hashlib
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
HASHES = {
    "SudoVDA.cat": "2f9189de5604bec9d86f51640cc540639e394d9ad0f8e689129375e95f2d22f8",
    "SudoVDA.cer": "6accdcd519f6179d967db4eaa20ecf25a732ba30e87f4cffebc768b2c13c9007",
    "SudoVDA.dll": "47ee263cb5de9382c6630a2d7f3dafec4a49419f953beec869ca5dd0c460ff63",
    "SudoVDA.inf": "ad69ac682756f0cf339b081fac7e6e8159fdf2ca01ca69df8945c7246c286925",
}
errors = []
for name in ("LICENSE", "LICENSE-SCOPE.md", "NOTICE.txt", "THIRD_PARTY_NOTICES.txt", "docs/SudoVDA-NOTICES.txt"):
    if not (ROOT / name).is_file():
        errors.append(f"Missing notice: {name}")
for name, expected in HASHES.items():
    path = ROOT / "apps/windows/terceiros/sudovda" / name
    if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        errors.append(f"Driver payload missing or differs from audited version: {name}")
try:
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
except subprocess.CalledProcessError:
    tracked = []
for item in filter(None, tracked):
    path = pathlib.PurePosixPath(item)
    if path.suffix.lower() in {".p12", ".pfx", ".p8", ".jks", ".keystore", ".mobileprovision"} or path.name.startswith("Signing.local"):
        errors.append(f"Private signing material tracked: {item}")
    if item.startswith(("apps/android/", "apps/ios/", "plugins/obs/")):
        errors.append(f"Outside Monitor scope: {item}")
if errors:
    print("\n".join(errors), file=sys.stderr)
    raise SystemExit(1)
print("Public source scope, notices and four audited SudoVDA payload hashes verified.")
