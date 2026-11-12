#!/usr/bin/env python3
"""Materialize the pinned core plugins from Git; never follow a moving branch."""
import argparse
import json
import re
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / 'plugins/bundled.json'
OUTPUT = ROOT / 'bridge/bundled/plugins.json'
LICENSE = ROOT / 'bridge/bundled/LICENSE'


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args])


def materialize(repo, lock):
    commit = lock['commit']
    commits = lock.get('commits', {})
    assert set(commits).issubset(lock['paths']), 'Commit overrides must name selected packages'
    for reference in {commit, *commits.values()}:
        assert re.fullmatch(r'[0-9a-f]{40}', reference), 'Pin immutable Git commits'
        assert git(repo, 'rev-parse', '--verify', reference + '^{commit}').decode().strip() == reference
    plugins = []
    for directory in lock['paths']:
        reference = commits.get(directory, commit)
        files = []
        for entry in git(repo, 'ls-tree', '-r', '-z', f'{reference}:{directory}').split(b'\0'):
            if not entry:
                continue
            metadata, name = entry.split(b'\t', 1)
            mode, kind, object_id = metadata.split()
            assert mode in (b'100644', b'100755') and kind == b'blob', 'Unsupported bundled file'
            files.append({'path': name.decode(), 'content': git(repo, 'cat-file', 'blob', object_id.decode()).decode(), 'executable': mode == b'100755'})
        package = {'path': directory, 'files': files}
        if reference != commit:
            package['commit'] = reference
        plugins.append(package)
    return json.dumps({'repository': lock['repository'], 'commit': commit, 'plugins': plugins}, indent=2) + '\n', git(repo, 'show', f'{commit}:LICENSE').decode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', type=Path, help='Existing upstream clone for offline regeneration')
    parser.add_argument('--check', action='store_true', help='Verify checked-in snapshots against pinned upstream Git objects')
    args = parser.parse_args()
    lock = json.loads(LOCK.read_text())
    with tempfile.TemporaryDirectory(prefix='openwebide-bundled-plugins-') as temp:
        repo = args.repository or Path(temp)
        if args.repository is None:
            git(repo, 'init', '--bare', '--quiet')
            references = sorted({lock['commit'], *lock.get('commits', {}).values()})
            git(repo, 'fetch', '--quiet', '--depth=1', '--no-tags', '--', lock['repository'], *references)
        snapshot, license_text = materialize(repo, lock)
        if args.check:
            assert OUTPUT.read_text() == snapshot, 'Bundled plugins differ; run tools/bundle_plugins.py'
            assert LICENSE.read_text() == license_text, 'Bundled plugin license differs'
        else:
            OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            OUTPUT.write_text(snapshot)
            LICENSE.write_text(license_text)
    print('Pinned core plugin snapshots verified' if args.check else 'Pinned core plugin snapshots generated')


if __name__ == '__main__':
    main()
