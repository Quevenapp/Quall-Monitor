#!/usr/bin/env python3
"""Stage actual packages for queven.com.br/quall-monitor without deploying."""
import argparse, hashlib, json, pathlib, shutil, subprocess
ROOT = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--version', required=True)
parser.add_argument('--mac', type=pathlib.Path, action='append', default=[])
parser.add_argument('--windows', type=pathlib.Path, action='append', default=[])
args = parser.parse_args()
revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip()
site = ROOT / 'site/quall-monitor'
out = ROOT / 'dist/site/quall-monitor'
files = []
packages = [('mac', x) for x in args.mac] + [('windows', x) for x in args.windows]
for platform, path in packages:
    if not path.is_file(): parser.error(f'Package missing: {path}')
    if not all(c.isalnum() or c in '._-' for c in path.name): parser.error(f'Unsafe filename: {path.name}')
    if platform == 'mac' and path.suffix not in ('.zip', '.dmg'): parser.error('Mac package must be .zip or .dmg')
    if platform == 'windows' and path.suffix != '.msi': parser.error('Windows package must be .msi')
    label = 'Windows x64' if platform == 'windows' else ('Mac Intel' if any(x in path.name.lower() for x in ('x64', 'x86_64', 'intel')) else 'Mac Apple Silicon')
    files.append({'platform': platform, 'label': label, 'url': './downloads/' + path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
if not files: parser.error('Pass actual packages with --mac and/or --windows')
out.mkdir(parents=True, exist_ok=True)
shutil.copy2(site / 'index.html', out / 'index.html')
(out / 'downloads').mkdir(exist_ok=True)
for _, path in packages: shutil.copy2(path, out / 'downloads' / path.name)
(out / 'downloads.json').write_text(json.dumps({'version': args.version, 'source': f'https://github.com/Quevenapp/Quall-Monitor/tree/{revision}', 'files': files}, indent=2) + '\n')
print(out)
