#!/usr/bin/env python3
"""Install pinned upstream clangd archives without changing system packages."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

TOOLS = Path(__file__).resolve().parent / 'clangd-language-server'


def install(destination, cache=None):
    pins = json.loads((TOOLS / 'versions.json').read_text())
    system = {'Darwin': 'darwin', 'Linux': 'linux', 'Windows': 'windows'}.get(platform.system())
    arch = {'x86_64': 'amd64', 'AMD64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64'}.get(platform.machine())
    key = f'{system}-{arch}'
    if key not in pins['platforms']:
        raise RuntimeError(f'No pinned clangd distribution for {key}; install clangd separately')
    release = pins['platforms'][key]
    destination = destination.resolve()
    marker = destination / '.openwebide-release.json'
    binary = destination / 'bin' / ('clangd.exe' if system == 'windows' else 'clangd')
    if destination.exists():
        if not marker.is_file() or json.loads(marker.read_text()) != release:
            raise RuntimeError('Existing clangd directory differs from the pin; remove it before updating')
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.clangd-', dir=destination.parent) as directory:
            staging = Path(directory)
            installed = staging / 'installed'
            installed.mkdir()
            for archive in release['archives']:
                name = archive['url'].rsplit('/', 1)[1]
                downloaded = staging / name
                cached = cache / name if cache is not None else None
                if cached is not None and cached.is_file():
                    shutil.copyfile(cached, downloaded)
                else:
                    with urllib.request.urlopen(archive['url'], timeout=60) as response, downloaded.open('wb') as output:
                        shutil.copyfileobj(response, output)
                with downloaded.open('rb') as source:
                    if hashlib.file_digest(source, 'sha256').hexdigest() != archive['sha256']:
                        raise RuntimeError(f'clangd archive checksum mismatch: {name}')
                if downloaded.suffix == '.deb':
                    subprocess.run(['dpkg-deb', '--extract', str(downloaded), str(installed)], check=True)
                else:
                    with zipfile.ZipFile(downloaded) as bundle:
                        for member in bundle.infolist():
                            path = Path(member.filename)
                            if path.is_absolute() or '..' in path.parts or path.parts[0] != 'clangd_' + release['version']:
                                raise RuntimeError('Unexpected clangd archive path')
                            if (member.external_attr >> 16) & 0o170000 == 0o120000:
                                raise RuntimeError('Unexpected clangd archive symlink')
                        bundle.extractall(staging / 'archive')
                    extracted = staging / 'archive' / ('clangd_' + release['version'])
                    shutil.copytree(extracted, installed, dirs_exist_ok=True)
            if system == 'linux':
                # Debian's binary and resource headers stay together. Keep shared
                # libraries private to this invocation, including arbitrary UIDs.
                triplet = {'amd64': 'x86_64-linux-gnu', 'arm64': 'aarch64-linux-gnu'}[arch]
                (installed / 'bin').mkdir(exist_ok=True)
                (installed / 'bin/clangd').write_text(
                    '#!/bin/sh\n'
                    'root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)\n'
                    f'export LD_LIBRARY_PATH="$root/usr/lib/{triplet}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}"\n'
                    f'exec "$root/usr/lib/llvm-{pins["major"]}/bin/clangd" "$@"\n')
            if system != 'windows':
                (installed / 'bin/clangd').chmod(0o755)
            (installed / marker.name).write_text(json.dumps(release, indent=2) + '\n')
            installed.rename(destination)
    result = subprocess.check_output([str(binary), '--version'], text=True)
    if not re.search(r'\bclangd version ' + re.escape(release['version']) + r'\b', result):
        raise RuntimeError('Installed clangd version differs from the pinned distribution')
    print(result.strip())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tool-path', type=Path, default=TOOLS / 'installed')
    parser.add_argument('--cache', type=Path, help='Read verified archives from this local directory when available')
    args = parser.parse_args()
    try:
        install(args.tool_path, args.cache)
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f'clangd setup failed: {error}', file=sys.stderr)
        sys.exit(1)
