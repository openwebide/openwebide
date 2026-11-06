#!/usr/bin/env python3
"""Install checksum-pinned Go and build gopls using the committed module lock."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

from install_wasi_sdk import extract_sdk_archive

TOOLS = Path(__file__).resolve().parent / 'go-language-server'


def toolchain(destination, versions, archive=None):
    sdk = destination / 'go'
    suffix = '.exe' if os.name == 'nt' else ''
    binary = sdk / 'bin' / ('go' + suffix)
    expected = 'go' + versions['go']
    if binary.exists():
        reported = subprocess.check_output([str(binary), 'version'], text=True, env={
            **os.environ, 'GOENV': 'off', 'GOTOOLCHAIN': 'local', 'GOWORK': 'off', 'GOROOT': str(sdk),
        })
        if reported.split()[2] != expected:
            raise RuntimeError('Installed Go differs from the pinned SDK; clear the tool directory before updating')
        return binary
    system = {'Darwin': 'darwin', 'Linux': 'linux', 'Windows': 'windows'}.get(platform.system())
    machine = {'x86_64': 'amd64', 'AMD64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64', 'ARM64': 'arm64'}.get(platform.machine())
    release = versions['platforms'].get(f'{system}-{machine}')
    if release is None:
        raise RuntimeError('No pinned Go SDK for this operating system and architecture')
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='go-sdk-', dir=destination) as directory:
        staging = Path(directory)
        downloaded = staging / release['filename']
        if archive is not None:
            shutil.copyfile(archive, downloaded)
        else:
            with urllib.request.urlopen('https://go.dev/dl/' + release['filename'], timeout=60) as response:
                with downloaded.open('wb') as output:
                    shutil.copyfileobj(response, output)
        with downloaded.open('rb') as stream:
            actual = hashlib.file_digest(stream, 'sha256').hexdigest()
        if actual != release['sha256']:
            raise RuntimeError('Go SDK checksum differs from the pinned official release')
        if downloaded.suffix == '.zip':
            with zipfile.ZipFile(downloaded) as bundle:
                for member in bundle.namelist():
                    path = Path(member)
                    if path.is_absolute() or '..' in path.parts or path.parts[0] != 'go':
                        raise RuntimeError('Unexpected Go SDK archive path')
                bundle.extractall(staging)
        else:
            with tarfile.open(downloaded, 'r:gz') as bundle:
                extract_sdk_archive(bundle, staging, 'go')
        (staging / 'go').rename(sdk)
    return binary


def build_environment(state, sdk):
    # Ignore personal proxy/credential and Go configuration. The public module
    # proxy and checksum database verify every dependency in go.sum; a pinned
    # local SDK prevents implicit toolchain downloads during the build.
    return {
        **os.environ, 'GOENV': 'off', 'GOTOOLCHAIN': 'local', 'GOWORK': 'off',
        'GOROOT': str(sdk), 'GOOS': {'Darwin': 'darwin', 'Linux': 'linux', 'Windows': 'windows'}[platform.system()],
        'GOARCH': {'x86_64': 'amd64', 'AMD64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64', 'ARM64': 'arm64'}[platform.machine()],
        'GOPROXY': 'https://proxy.golang.org', 'GOSUMDB': 'sum.golang.org',
        'GOPRIVATE': '', 'GONOPROXY': '', 'GONOSUMDB': '', 'GOINSECURE': '',
        'GOAUTH': 'off', 'GOFLAGS': '', 'GOEXPERIMENT': '',
        'GOVCS': '*:off', 'CGO_ENABLED': '0', 'GOTELEMETRY': 'off',
        'GOPATH': str(state / 'path'), 'GOCACHE': str(state / 'cache'),
        'GOMODCACHE': str(state / 'modules'),
    }


def install(destination, archive=None):
    versions = json.loads((TOOLS / 'versions.json').read_text())
    destination = destination.resolve()
    go = toolchain(destination, versions, archive)
    binary = destination / 'bin' / ('gopls.exe' if os.name == 'nt' else 'gopls')
    binary.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='openwebide-go-build-') as directory:
        subprocess.run([
            str(go), 'build', '-mod=readonly', '-trimpath', '-buildvcs=false',
            '-o', str(binary), 'golang.org/x/tools/gopls',
        ], cwd=TOOLS / 'dependencies', env=build_environment(Path(directory), go.parent.parent), check=True)
    reported = subprocess.check_output([str(binary), 'version'], text=True)
    if reported.strip() != 'golang.org/x/tools/gopls ' + versions['server']:
        raise RuntimeError('Installed gopls reports a different version')
    print(subprocess.check_output([str(go), 'version'], text=True, env={
        **os.environ, 'GOENV': 'off', 'GOTOOLCHAIN': 'local', 'GOWORK': 'off', 'GOROOT': str(go.parent.parent),
    }).strip())
    print(reported.strip())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tool-path', type=Path, default=TOOLS / 'installed')
    parser.add_argument('--archive', type=Path, help='Use a cached official Go SDK archive')
    args = parser.parse_args()
    install(args.tool_path, args.archive)


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Go language-server setup failed: {error}', file=sys.stderr)
        sys.exit(1)
