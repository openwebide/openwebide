#!/usr/bin/python3
"""Controlled deployment tools for the isolated SSH transport fixture."""
import json
import os
from pathlib import Path
import subprocess
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == 'skopeo':
    print(json.dumps({'Digest': 'sha256:' + 'b'*64, 'Architecture': 'amd64', 'Os': 'linux', 'Labels': {
        'org.opencontainers.image.version': '1.0.0-alpha.6', 'org.opencontainers.image.revision': 'c'*40,
        'io.openwebide.database-schema': '58'}}))
elif name == 'systemd-run':
    start = next(index for index, arg in enumerate(args) if arg.endswith('python3'))
    subprocess.Popen(args[start:], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True)
elif name == 'systemctl':
    print('inactive' if 'show' in args else 'active')
elif name == 'docker' and 'config' in args:
    print(json.dumps({'services': {'openwebide': {'image': os.environ['OPENWEBIDE_IMAGE']}}}))
elif name in ['docker', 'podman']:
    with Path('/home/admin/update-commands.jsonl').open('a') as stream:
        stream.write(json.dumps([name] + args) + '\n')
else:
    sys.exit(1)
