#!/usr/bin/env python3
"""Install checksum-pinned HashiCorp Terraform and terraform-ls distributions."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

TOOLS = Path(__file__).resolve().parent / 'terraform-language-server'
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_BINARY_BYTES = 512 * 1024 * 1024


def download(url, destination):
    with urllib.request.urlopen(url, timeout=60) as response, destination.open('wb') as output:
        remaining = MAX_ARCHIVE_BYTES
        while chunk := response.read(min(1024 * 1024, remaining + 1)):
            remaining -= len(chunk)
            if remaining < 0:
                raise RuntimeError('Terraform distribution exceeds its download limit')
            output.write(chunk)


def verify(destination, pins, suffix):
    environment = {**os.environ, 'CHECKPOINT_DISABLE': '1', 'TF_IN_AUTOMATION': '1'}
    cli = subprocess.check_output([str(destination / 'bin' / ('terraform' + suffix)), 'version', '-json'], text=True, env=environment, timeout=15)
    if json.loads(cli)['terraform_version'] != pins['terraform']['version']:
        raise RuntimeError('Installed Terraform version differs from the pin')
    server = subprocess.check_output([str(destination / 'bin' / ('terraform-ls' + suffix)), '--version'], text=True, env=environment, timeout=15)
    if server.strip() != pins['terraform-ls']['version']:
        raise RuntimeError(f'Installed terraform-ls version differs from the pin: {server.strip()}')
    return f'Terraform {pins["terraform"]["version"]}; terraform-ls {server.strip()}'


def install(destination, cache=None):
    pins = json.loads((TOOLS / 'versions.json').read_text())
    system = {'Darwin': 'darwin', 'Linux': 'linux', 'Windows': 'windows'}.get(platform.system())
    arch = {'x86_64': 'amd64', 'AMD64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64', 'ARM64': 'arm64'}.get(platform.machine())
    key = f'{system}-{arch}'
    release = {name: {**pin, 'archive': pin['platforms'].get(key)} for name, pin in pins.items()}
    if any(pin['archive'] is None for pin in release.values()):
        raise RuntimeError(f'No pinned Terraform distributions for {key}')
    destination = destination.resolve()
    marker = destination / '.openwebide-release.json'
    suffix = '.exe' if system == 'windows' else ''
    if destination.exists():
        if not marker.is_file() or json.loads(marker.read_text()) != release:
            raise RuntimeError('Existing Terraform tool directory differs from the pins; choose a new directory when updating')
        reported = verify(destination, pins, suffix)
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.terraform-', dir=destination.parent) as directory:
            staging = Path(directory)
            installed = staging / 'installed'
            (installed / 'bin').mkdir(parents=True)
            (installed / 'licenses').mkdir()
            for name, pin in release.items():
                archive = pin['archive']
                downloaded = staging / archive['filename']
                cached = cache / archive['filename'] if cache else None
                if cached is not None and cached.is_file():
                    if cached.stat().st_size > MAX_ARCHIVE_BYTES:
                        raise RuntimeError('Cached Terraform archive exceeds its size limit')
                    shutil.copyfile(cached, downloaded)
                else:
                    download(archive['url'], downloaded)
                with downloaded.open('rb') as stream:
                    if hashlib.file_digest(stream, 'sha256').hexdigest() != archive['sha256']:
                        raise RuntimeError(f'{name} archive checksum differs from the pinned official release')
                binary_name = name + suffix
                with zipfile.ZipFile(downloaded) as bundle:
                    if len(bundle.infolist()) > 32:
                        raise RuntimeError('Unexpected Terraform distribution inventory')
                    matches = [item for item in bundle.infolist() if item.filename == binary_name]
                    if len(matches) != 1:
                        raise RuntimeError(f'Expected one {binary_name} in the upstream archive')
                    binary = matches[0]
                    if binary.file_size > MAX_BINARY_BYTES or (binary.external_attr >> 16) & 0o170000 == 0o120000:
                        raise RuntimeError('Unexpected Terraform binary size or symlink')
                    with bundle.open(binary) as source, (installed / 'bin' / binary_name).open('wb') as output:
                        shutil.copyfileobj(source, output)
                    for item in bundle.infolist():
                        if item.filename.lower() in {'license', 'license.txt'}:
                            if item.file_size > 1024 * 1024:
                                raise RuntimeError('Unexpected Terraform license size')
                            (installed / 'licenses' / f'{name}.txt').write_bytes(bundle.read(item))
                if system != 'windows':
                    (installed / 'bin' / binary_name).chmod(0o755)
            reported = verify(installed, pins, suffix)
            (installed / marker.name).write_text(json.dumps(release, indent=2) + '\n')
            installed.rename(destination)
    print(reported)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tool-path', type=Path, default=TOOLS / 'installed')
    parser.add_argument('--cache', type=Path, help='Read checksum-verified upstream archives from this directory')
    args = parser.parse_args()
    try:
        install(args.tool_path, args.cache)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        print(f'Terraform setup failed: {error}', file=sys.stderr)
        sys.exit(1)
