#!/usr/bin/env python3
"""Stage packages tied to an explicit source commit, without deploying."""
import argparse
import hashlib
import json
import pathlib
import plistlib
import re
import shutil
import struct
import subprocess
import tempfile
import zipfile
from xml.parsers.expat import ExpatError

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = 'https://github.com/Quevenapp/Quall-Monitor'
parser = argparse.ArgumentParser()
parser.add_argument('--version', required=True)
parser.add_argument('--source-revision', required=True, help='Full SHA of the source used to build every package')
parser.add_argument('--channel', choices=('preview', 'stable'), default='preview')
parser.add_argument('--mac', type=pathlib.Path, action='append', default=[])
parser.add_argument('--windows', type=pathlib.Path, action='append', default=[])
args = parser.parse_args()
if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', args.version):
    parser.error('--version must be a canonical MAJOR.MINOR.PATCH version')
if not re.fullmatch(r'[a-fA-F0-9]{40}', args.source_revision):
    parser.error('--source-revision must be a full 40-character commit SHA')
revision = args.source_revision.lower()
commit = subprocess.run(['git', 'cat-file', '-t', revision], cwd=ROOT, capture_output=True, text=True)
if commit.returncode or commit.stdout.strip() != 'commit':
    parser.error(f'Source commit is not available locally: {revision}; fetch that revision first')


def macho_architectures(stream):
    """Read CPU types from thin/fat Mach-O headers without extracting the ZIP."""
    header = stream.read(8)
    if len(header) != 8:
        raise ValueError('truncated Mach-O header')
    thin = {b'\xcf\xfa\xed\xfe': '<', b'\xfe\xed\xfa\xcf': '>'}
    fat = {b'\xca\xfe\xba\xbe': ('>', 20), b'\xbe\xba\xfe\xca': ('<', 20),
           b'\xca\xfe\xba\xbf': ('>', 32), b'\xbf\xba\xfe\xca': ('<', 32)}
    if header[:4] in thin:
        cpus = [struct.unpack(thin[header[:4]] + 'I', header[4:])[0]]
    elif header[:4] in fat:
        endian, record_size = fat[header[:4]]
        count = struct.unpack(endian + 'I', header[4:])[0]
        if not 1 <= count <= 8:
            raise ValueError('invalid fat Mach-O architecture count')
        records = stream.read(count * record_size)
        if len(records) != count * record_size:
            raise ValueError('truncated fat Mach-O architecture table')
        cpus = [struct.unpack_from(endian + 'I', records, i * record_size)[0] for i in range(count)]
    else:
        raise ValueError('expected a 64-bit Mach-O executable')
    names = {0x01000007: 'x64', 0x0100000c: 'arm64'}
    if any(cpu not in names for cpu in cpus) or len(set(cpus)) != len(cpus):
        raise ValueError('unsupported or duplicate Mach-O architecture')
    return {names[cpu] for cpu in cpus}


def verify_mac_zip(path, architecture):
    prefix = 'Quall Monitor.app/Contents/'
    source_name = prefix + 'Resources/SOURCE-REVISION.txt'
    plist_name = prefix + 'Info.plist'
    executables = [prefix + 'MacOS/quall-monitor-app', prefix + 'MacOS/quall-monitor-display']
    expected = {'arm64', 'x64'} if architecture == 'universal' else {architecture}
    try:
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
            for name in [source_name, plist_name] + executables:
                if names.count(name) != 1:
                    raise ValueError(f'expected exactly one {name}')
            if archive.getinfo(source_name).file_size > 4096 or archive.getinfo(plist_name).file_size > 65536:
                raise ValueError('package metadata is unexpectedly large')
            lines = archive.read(source_name).decode('utf-8').strip().splitlines()
            expected_lines = [f'Repository: {REPOSITORY}', f'Revision: {revision}']
            if lines not in (expected_lines, expected_lines + ['Dirty: False']):
                raise ValueError('SOURCE-REVISION.txt does not identify the requested clean source revision')
            info = plistlib.loads(archive.read(plist_name))
            if not isinstance(info, dict) or info.get('CFBundleShortVersionString') != args.version:
                raise ValueError('app version does not match --version')
            if info.get('CFBundleIdentifier') != 'br.com.queven.quall.monitor' or info.get('CFBundleExecutable') != 'quall-monitor-app':
                raise ValueError('ZIP does not contain the expected Quall Monitor app')
            for name in executables:
                with archive.open(name) as binary:
                    if macho_architectures(binary) != expected:
                        raise ValueError(f'{name} does not match architecture {architecture}')
    except (OSError, ValueError, zipfile.BadZipFile, KeyError, RuntimeError, ExpatError) as error:
        parser.error(f'Invalid Mac package {path.name}: {error}')


def verify_mac_dmg(path, architecture):
    """Mount a distribution image read-only; never run its app or helper."""
    if not shutil.which('hdiutil'):
        parser.error('Mac DMG inspection requires hdiutil on macOS')
    expected = {'arm64', 'x64'} if architecture == 'universal' else {architecture}
    try:
        with tempfile.TemporaryDirectory(prefix='quall-monitor-inspect-') as directory:
            mount = pathlib.Path(directory) / 'volume'
            mount.mkdir()
            attached = False
            try:
                command = subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse',
                                          '-mountpoint', str(mount), '-plist', str(path.resolve())],
                                         capture_output=True, check=True)
                attached = True
                plistlib.loads(command.stdout)
                app = mount / 'Quall Monitor.app'
                applications = mount / 'Applications'
                if not applications.is_symlink() or str(applications.readlink()) != '/Applications':
                    raise ValueError('DMG must include an Applications installation shortcut')
                contents = app / 'Contents'
                source = contents / 'Resources/SOURCE-REVISION.txt'
                info_path = contents / 'Info.plist'
                if source.stat().st_size > 4096 or info_path.stat().st_size > 65536:
                    raise ValueError('package metadata is unexpectedly large')
                lines = source.read_text().strip().splitlines()
                expected_lines = [f'Repository: {REPOSITORY}', f'Revision: {revision}']
                if lines not in (expected_lines, expected_lines + ['Dirty: False']):
                    raise ValueError('DMG app source does not identify the requested clean revision')
                info = plistlib.loads(info_path.read_bytes())
                if info.get('CFBundleShortVersionString') != args.version:
                    raise ValueError('DMG app version does not match --version')
                if info.get('CFBundleIdentifier') != 'br.com.queven.quall.monitor' or info.get('CFBundleExecutable') != 'quall-monitor-app':
                    raise ValueError('DMG does not contain the expected Quall Monitor app')
                for name in ('quall-monitor-app', 'quall-monitor-display'):
                    with (contents / 'MacOS' / name).open('rb') as binary:
                        if macho_architectures(binary) != expected:
                            raise ValueError(f'{name} does not match architecture {architecture}')
                subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)],
                               capture_output=True, check=True)
            finally:
                if attached:
                    subprocess.run(['hdiutil', 'detach', str(mount)], capture_output=True, check=True)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, ExpatError) as error:
        parser.error(f'Invalid Mac installer {path.name}: {error}')


def verify_windows_report(path, digest, locale):
    """Bind the Windows CI cabinet inspection to the exact MSI being published."""
    report_path = path.with_name(path.name + '.validation.json')
    try:
        report = json.loads(report_path.read_text(encoding='utf-8-sig'))
        if report.get('ProductName') != 'Quall Monitor' or report.get('ProductVersion') != args.version:
            raise ValueError('MSI inspection has a different product or version')
        if report.get('MsiSha256') != digest:
            raise ValueError('MSI inspection SHA-256 does not match the installer')
        if report.get('Culture', locale) != locale:
            raise ValueError('MSI inspection language does not match the download label')
        source = report.get('SourceRevision', '').strip().splitlines()
        if source != [f'Repository: {REPOSITORY}', f'Revision: {revision}', 'Dirty: False']:
            raise ValueError('MSI payload inspection does not identify the requested clean revision')
        if report.get('DriverActionsInspected') is not True or report.get('DriverActionsExecuted') is not False:
            raise ValueError('Expected a read-only cabinet and driver action inspection')
        if args.channel == 'stable' and (report.get('MsiAuthenticode') != 'Valid' or report.get('ExeAuthenticode') != 'Valid'):
            raise ValueError('Stable Windows downloads require valid installer and executable signatures')
    except (OSError, ValueError, TypeError) as error:
        parser.error(f'Invalid Windows inspection {report_path.name}: {error}')


site = ROOT / 'site/quall-monitor'
out = ROOT / 'dist/site/quall-monitor'
files = []
packages = [('mac', x) for x in args.mac] + [('windows', x) for x in args.windows]
seen = set()
for platform, path in packages:
    if not path.is_file(): parser.error(f'Package missing: {path}')
    if out.resolve() in path.resolve().parents:
        parser.error(f'Input packages must be outside the generated site directory: {path}')
    pattern = (rf'Quall-Monitor-{re.escape(args.version)}-macos-(arm64|x64|universal)\.(zip|dmg)'
               if platform == 'mac' else rf'Quall-Monitor-{re.escape(args.version)}-windows-(x64)(?:-(pt-BR|en-US))?\.msi')
    match = re.fullmatch(pattern, path.name)
    if not match: parser.error(f'Package filename must match version {args.version} and a supported architecture: {path.name}')
    architecture = match.group(1)
    locale = (match.group(2) or 'pt-BR') if platform == 'windows' else None
    package_format = path.suffix[1:]
    key = (platform, architecture, locale, package_format)
    if key in seen: parser.error(f'Duplicate package for {platform} {architecture}')
    seen.add(key)
    if platform == 'mac':
        if package_format == 'dmg':
            verify_mac_dmg(path, architecture)
        else:
            verify_mac_zip(path, architecture)
        label = {'arm64': 'Mac Apple Silicon', 'x64': 'Mac Intel', 'universal': 'Mac Universal (Apple Silicon + Intel)'}[architecture]
    else:
        with path.open('rb') as package:
            if package.read(8) != b'\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1':
                parser.error(f'Windows package is not an MSI compound file: {path.name}')
        label = 'Windows x64 · ' + ('English' if locale == 'en-US' else 'Português')
    digest = hashlib.sha256()
    with path.open('rb') as package:
        for chunk in iter(lambda: package.read(1024 * 1024), b''):
            digest.update(chunk)
    if platform == 'windows':
        verify_windows_report(path, digest.hexdigest(), locale)
    files.append({'platform': platform, 'architecture': architecture, 'label': label,
                  'format': package_format, 'locale': locale,
                  'url': './downloads/' + path.name, 'sha256': digest.hexdigest(),
                  'source_revision': revision, 'provenance': 'embedded' if platform == 'mac' else 'external'})
if not files: parser.error('Pass actual packages with --mac and/or --windows')
# Validation finishes before replacing the generated directory, so rejected inputs preserve it.
if out.exists(): shutil.rmtree(out)
out.mkdir(parents=True, exist_ok=True)
shutil.copy2(site / 'index.html', out / 'index.html')
if (site / 'icon.png').is_file():
    shutil.copy2(site / 'icon.png', out / 'icon.png')
(out / 'downloads').mkdir(exist_ok=True)
for _, path in packages: shutil.copy2(path, out / 'downloads' / path.name)
(out / 'downloads.json').write_text(json.dumps({'version': args.version, 'channel': args.channel, 'source': f'{REPOSITORY}/tree/{revision}', 'files': files}, indent=2) + '\n')
print(out)
