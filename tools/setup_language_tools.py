#!/usr/bin/env python3
"""Install pinned verification tools in a shared cache and print their PATH directories."""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

from build_lock import build_lock

TOOLS = Path(__file__).resolve().parent
NPM_TOOLS = {
    'typescript': 'typescript-language-server',
    'python': 'python-language-server',
    'web': 'web-language-servers',
}
NATIVE_PATHS = {
    'csharp': ('.',),
    'go': ('bin', 'go/bin'),
    'clangd': ('bin',),
    'terraform': ('bin',),
}


def require_node_version():
    node = subprocess.check_output(['node', '-p', 'process.versions.node'], text=True).strip()
    if tuple(map(int, node.split('.'))) < (22, 22, 2):
        raise RuntimeError('Language-server verification requires Node.js 22.22.2 or newer')


def install_npm(root, *, lock_only=False):
    # The lockfiles cover public packages. Keep user config and install scripts out.
    with tempfile.TemporaryDirectory(prefix='openwebide-npm-') as directory:
        config = Path(directory)
        for name in ('user', 'global'):
            (config / name).touch()
        env = {**os.environ, 'NPM_CONFIG_USERCONFIG': str(config / 'user'),
               'NPM_CONFIG_GLOBALCONFIG': str(config / 'global'),
               'NPM_CONFIG_CACHE': str(config / 'cache')}
        command = ['npm', 'install', '--package-lock-only'] if lock_only else ['npm', 'ci']
        subprocess.run(command + ['--prefix', str(root), '--registry=https://registry.npmjs.org',
                                  '--ignore-scripts', '--no-audit', '--no-fund'],
                       env=env, stdout=sys.stderr, check=True)


def setup_tool(name, installer=None):
    npm_project = NPM_TOOLS.get(name)
    source = TOOLS / (npm_project or f'{name}-language-server')
    if npm_project:
        require_node_version()
        inputs = ('package.json', 'package-lock.json')
        directories = ('node_modules/.bin',)
    else:
        inputs = ('versions.json',)
        if name == 'go':
            inputs += ('dependencies/go.mod', 'dependencies/go.sum')
        directories = NATIVE_PATHS[name]

    pins = {entry: (source / entry).read_bytes() for entry in inputs}
    digest = hashlib.sha256()
    for entry, contents in pins.items():
        digest.update(entry.encode() + b'\0' + hashlib.sha256(contents).digest())
    platform_key = f'{platform.system().lower()}-{platform.machine().lower()}'
    cache_home = Path(os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache')
    root = Path(os.environ.get('OPENWEBIDE_TOOL_CACHE') or cache_home / 'openwebide/language-tools')
    destination = root.expanduser().resolve() / name / f'{platform_key}-{digest.hexdigest()}'
    paths = [destination / directory for directory in directories]
    with build_lock(destination.with_suffix('.lock')):
        marker = '.openwebide-install-complete'
        if destination.exists():
            if not (destination / marker).is_file() or not all(path.is_dir() for path in paths):
                raise RuntimeError(f'Remove the incomplete {name} installation at {destination} and retry')
            return paths
        with tempfile.TemporaryDirectory(prefix='.install-', dir=destination.parent) as directory:
            staging = Path(directory) / 'tool'
            if npm_project:
                staging.mkdir()
                for entry, contents in pins.items():
                    (staging / entry).write_bytes(contents)
                install_npm(staging)
            elif installer is None:
                subprocess.run([sys.executable, str(TOOLS / f'setup-{name}-language-server.py'),
                                '--tool-path', str(staging)], stdout=sys.stderr, check=True)
            else:
                installer(staging)
            if not all((staging / directory).is_dir() for directory in directories):
                raise RuntimeError(f'{name} setup did not produce the expected installation')
            if any((source / entry).read_bytes() != contents for entry, contents in pins.items()):
                raise RuntimeError(f'Retry the installation because the {name} pins changed during setup')
            (staging / marker).touch()
            staging.rename(destination)
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tools', nargs='+', choices=(*NPM_TOOLS, *NATIVE_PATHS))
    args = parser.parse_args()
    for name in args.tools:
        for path in setup_tool(name):
            print(path)


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f'Language tool setup failed: {error}')
