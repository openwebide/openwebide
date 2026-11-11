#!/usr/bin/env python3
"""Install the pinned HTML/CSS/JSON verification servers without user npm config."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lock-only', action='store_true')
    args = parser.parse_args()
    node = subprocess.check_output(['node', '-p', 'process.versions.node'], text=True).strip()
    if tuple(map(int, node.split('.'))) < (22, 22, 2):
        raise SystemExit('Language-server verification requires Node.js 22.22.2 or newer')
    root = Path(__file__).resolve().parent / 'web-language-servers'
    with tempfile.TemporaryDirectory(prefix='openwebide-web-npm-') as config:
        config = Path(config)
        for name in ['user', 'global']:
            (config / name).touch()
        env = {**os.environ, 'NPM_CONFIG_USERCONFIG': str(config / 'user'),
               'NPM_CONFIG_GLOBALCONFIG': str(config / 'global'),
               'NPM_CONFIG_CACHE': str(config / 'cache')}
        command = ['npm', 'install', '--package-lock-only'] if args.lock_only else ['npm', 'ci']
        subprocess.run(command + ['--prefix', str(root), '--registry=https://registry.npmjs.org',
                                 '--ignore-scripts', '--no-audit', '--no-fund'], env=env, check=True)
    if not args.lock_only:
        package = json.loads((root / 'node_modules/vscode-langservers-extracted/package.json').read_text())
        print('HTML/CSS/JSON language servers: ' + package['version'])


if __name__ == '__main__':
    main()
