#!/usr/bin/env python3
"""Exercise each compiled default language package through the native WASI runtime.

Build the release `check_language` example and compile the default bundle first.
The component fixtures check every declared language ID, including dialects,
source reconstruction, embedded delegation, folds and Unicode/CRLF handling.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def check(bundle):
    snapshot = json.loads((ROOT / 'bridge/bundled/plugins.json').read_text())
    artifacts = json.loads(bundle.read_text())['artifacts']
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).expanduser()
    checker = target / 'release/examples/check_language'
    assert checker.is_file(), 'Build cargo build -p openwebide-plugin-runtime --example check_language --release first'
    languages = set()
    packages = 0
    with tempfile.TemporaryDirectory(prefix='openwebide-language-components-') as directory:
        temporary = Path(directory)
        for package in snapshot['plugins']:
            manifest = json.loads(next(file['content'] for file in package['files']
                                       if file['path'] == 'plugin.json'))
            definitions = manifest.get('contributions', {}).get('languages', [])
            if not definitions:
                continue
            source = {'repository': snapshot['repository'], 'commit': package.get('commit', snapshot['commit']),
                      'path': package['path']}
            matches = [artifact for artifact in artifacts if artifact['source'] == source]
            assert len(matches) == 1, f'Expected one compiled component for {package["path"]}'
            ids = {definition['id'] for definition in definitions}
            assert not languages.intersection(ids), f'Duplicate default language IDs: {ids & languages}'
            languages.update(ids)
            component = temporary / 'component.wasm'
            component.write_bytes(base64.b64decode(matches[0]['component'], validate=True))
            declaration = temporary / 'plugin.json'
            declaration.write_text(json.dumps(manifest))
            subprocess.run([str(checker), str(component), str(declaration), '--family-matrix'],
                           cwd=ROOT, check=True, timeout=120)
            packages += 1
    assert packages, 'No language defaults were checked'
    print(f'Validated {packages} compiled default language packages covering {len(languages)} IDs')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=Path, help='Output of openwebide-bundle-plugins')
    check(parser.parse_args().bundle)
