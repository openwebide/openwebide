#!/usr/bin/env python3
"""Install pinned upstream clangd archives without changing system packages."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

TOOLS = Path(__file__).resolve().parent / 'clangd-language-server'
MAX_ARCHIVE_BYTES = 2 * 1024**3
MAX_INSTALLED_BYTES = 512 * 1024**2
MAX_MEMBERS = 50_000


def relative_path(name, root):
    path = PurePosixPath(name)
    if (path.is_absolute() or '..' in path.parts or '\\' in name or ':' in name
            or '\0' in name or len(name.encode()) > 4096
            or not path.parts or path.parts[0] != root):
        raise RuntimeError('Unexpected clangd archive path')
    return path.relative_to(root)


def extract_llvm(archive, installed, root, major):
    if not root or PurePosixPath(root).parts != (root,) or any(c in root for c in '\\:\0'):
        raise RuntimeError('Unexpected LLVM archive root')
    selected, total = set(), 0
    headers = ('lib', 'clang', str(major), 'include')
    with tarfile.open(archive, 'r|xz') as bundle:
        for count, member in enumerate(bundle, 1):
            if count > MAX_MEMBERS:
                raise RuntimeError('clangd archive exceeds its inventory limit')
            relative = relative_path(member.name, root)
            if (str(relative) != 'bin/clangd' and relative.parts[:4] != headers
                    and str(relative) != 'include/llvm/Support/LICENSE.TXT'):
                continue
            if member.isdir() and str(relative) != 'bin/clangd':
                continue
            if not member.isfile() or relative in selected:
                raise RuntimeError('Unexpected linked, special or duplicate LLVM payload')
            total += member.size
            if member.size < 0 or total > MAX_INSTALLED_BYTES:
                raise RuntimeError('clangd payload exceeds its extraction limit')
            selected.add(relative)
            target = installed.joinpath(*relative.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            with bundle.extractfile(member) as source, target.open('wb') as output:
                shutil.copyfileobj(source, output)
            target.chmod(0o755 if str(relative) == 'bin/clangd' else 0o644)
    if (PurePosixPath('bin/clangd') not in selected
            or not any(len(path.parts) > 4 and path.parts[:4] == headers for path in selected)):
        raise RuntimeError('LLVM archive is missing clangd or its resource headers')


def verify(binary, version):
    result = subprocess.check_output([str(binary), '--version'], text=True, timeout=15)
    if not re.search(r'\bclangd version ' + re.escape(version) + r'\b', result):
        raise RuntimeError('Installed clangd version differs from the pinned distribution')
    return result.strip()


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
                    if cached.stat().st_size > MAX_ARCHIVE_BYTES:
                        raise RuntimeError('Cached clangd archive exceeds its size limit')
                    shutil.copyfile(cached, downloaded)
                else:
                    with urllib.request.urlopen(archive['url'], timeout=60) as response, downloaded.open('wb') as output:
                        remaining = MAX_ARCHIVE_BYTES
                        while chunk := response.read(min(1024 * 1024, remaining + 1)):
                            remaining -= len(chunk)
                            if remaining < 0:
                                raise RuntimeError('clangd archive exceeds its download limit')
                            output.write(chunk)
                with downloaded.open('rb') as source:
                    if hashlib.file_digest(source, 'sha256').hexdigest() != archive['sha256']:
                        raise RuntimeError(f'clangd archive checksum mismatch: {name}')
                if downloaded.name.endswith('.tar.xz'):
                    extract_llvm(downloaded, installed, archive['root'], pins['major'])
                elif downloaded.suffix == '.zip':
                    with zipfile.ZipFile(downloaded) as bundle:
                        members = bundle.infolist()
                        if len(members) > MAX_MEMBERS or sum(member.file_size for member in members) > MAX_INSTALLED_BYTES:
                            raise RuntimeError('clangd archive exceeds its extraction limits')
                        for member in bundle.infolist():
                            relative_path(member.filename, 'clangd_' + release['version'])
                            if (member.external_attr >> 16) & 0o170000 == 0o120000:
                                raise RuntimeError('Unexpected clangd archive symlink')
                        bundle.extractall(staging / 'archive')
                    extracted = staging / 'archive' / ('clangd_' + release['version'])
                    shutil.copytree(extracted, installed, dirs_exist_ok=True)
                else:
                    raise RuntimeError('Unsupported clangd archive format')
            if 'licenseSha256' in release:
                license_text = (TOOLS / 'LICENSE.TXT').read_bytes()
                if hashlib.sha256(license_text).hexdigest() != release['licenseSha256']:
                    raise RuntimeError('clangd license differs from the pinned upstream license')
                (installed / 'LICENSE.TXT').write_bytes(license_text)
            if system != 'windows':
                (installed / 'bin/clangd').chmod(0o755)
            reported = verify(installed / 'bin' / binary.name, release['version'])
            (installed / marker.name).write_text(json.dumps(release, indent=2) + '\n')
            installed.rename(destination)
        print(reported)
        return
    print(verify(binary, release['version']))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tool-path', type=Path, default=TOOLS / 'installed')
    parser.add_argument('--cache', type=Path, help='Read verified archives from this local directory when available')
    args = parser.parse_args()
    try:
        install(args.tool_path, args.cache)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f'clangd setup failed: {error}', file=sys.stderr)
        sys.exit(1)
