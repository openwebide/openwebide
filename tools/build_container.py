#!/usr/bin/env python3
"""Build, run and measure OpenWebIDE with the shared local container builder."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import uuid

from build_lock import build_lock

REPO = Path(__file__).resolve().parent.parent
LOCAL_IMAGE = 'openwebide:local'
SOURCE_LABEL = 'io.openwebide.local-build=true'


def docker(*args, capture=False, check=True, quiet=False, log=None):
    output = log
    if capture:
        output = subprocess.PIPE
    elif quiet:
        output = subprocess.DEVNULL
    return subprocess.run(['docker', *args], text=True, check=check,
                          stdout=output,
                          stderr=subprocess.DEVNULL if quiet else None, cwd=REPO)


def builder_name():
    context = docker('context', 'show', capture=True).stdout.strip()
    endpoint = docker('context', 'inspect', context, '--format', '{{.Endpoints.docker.Host}}',
                      capture=True).stdout.strip()
    if context == 'default' and not os.environ.get('DOCKER_CONTEXT'):
        endpoint = os.environ.get('DOCKER_HOST') or endpoint
    key = hashlib.sha256(endpoint.encode()).hexdigest()[:12]
    return f'openwebide-{key}'


def ensure_builder(name):
    if docker('buildx', 'inspect', name, check=False, quiet=True).returncode:
        docker('buildx', 'create', '--name', name, '--driver', 'docker-container',
               '--buildkitd-config', str(REPO / 'docker/buildkit.dev.toml'), log=sys.stderr)


def app_build(compose_args):
    if compose_args is not None:
        compose = docker('compose', *compose_args, 'config', '--format', 'json', capture=True)
        service = json.loads(compose.stdout)['services'].get('openwebide', {})
        if service.get('image') != LOCAL_IMAGE:
            sys.exit(f'The openwebide Compose service must use {LOCAL_IMAGE} for source builds.')
    name = builder_name()
    lock = Path(tempfile.gettempdir()) / f'{name}.lock'
    with build_lock(lock):
        ensure_builder(name)
        revision = subprocess.check_output(['git', '-C', str(REPO), 'rev-parse', 'HEAD'], text=True).strip()
        docker('buildx', 'build', '--builder', name, '--load', '--tag', LOCAL_IMAGE,
               '--label', SOURCE_LABEL, '--build-arg', f'OPENWEBIDE_BUILD_COMMIT={revision}', str(REPO))
        if compose_args is not None:
            docker('compose', *compose_args, 'up', '--detach', '--no-build')
        # The daemon checks that images are both dangling and unused by containers.
        docker('image', 'prune', '--force', '--filter', f'label={SOURCE_LABEL}')


def measure(args, *, row_update=False):
    backend = REPO / 'target/wasm32-wasip2/release/openwebide_backend.wasm'
    if not backend.is_file() or not (REPO / 'frontend/dist/index.html').is_file():
        sys.exit('Build the current backend and frontend with NO_COLOR=true spin build first.')
    name = builder_name()
    container = f'openwebide-editor-view-{uuid.uuid4().hex}'
    tag = f'openwebide:{container.removeprefix("openwebide-")}'
    remove_container = False
    try:
        with tempfile.TemporaryDirectory(prefix='openwebide-editor-view-') as directory:
            shutil.copyfile(REPO / 'tools/editor-view.Dockerfile', Path(directory) / 'Dockerfile')
            with build_lock(Path(tempfile.gettempdir()) / f'{name}.lock'):
                ensure_builder(name)
            docker('buildx', 'build', '--builder', name, '--load', '--tag', tag, directory, log=sys.stderr)
        image = docker('image', 'inspect', '--format', '{{.Id}}', tag, capture=True).stdout.strip()
        entrypoint = ('--entrypoint', 'python3') if row_update else ()
        arguments = ('tools/measure-editor-row-update.py',) if row_update else ('--require-pss',)
        remove_container = True
        result = docker('run', '--rm', '--name', container, '--init', '--shm-size=1g',
                      '--memory=10g', '--cpus=4',
                      '--mount', f'type=bind,src={REPO},dst=/workspace/repos/openwebide',
                      '--env', f'EDITOR_VIEW_IMAGE={image}', *entrypoint, tag, *arguments, *args,
                      check=False).returncode
        remove_container = result != 0
        return result
    finally:
        if remove_container:
            docker('container', 'rm', '--force', '--volumes', container, quiet=True, check=False)
        if not docker('image', 'inspect', tag, quiet=True, check=False).returncode:
            docker('image', 'rm', '--no-prune', tag, log=sys.stderr, check=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('build', 'up', 'builder', 'measure', 'measure-row-update'))
    parser.add_argument('options', nargs=argparse.REMAINDER, help='Compose or measurement options')
    args = parser.parse_args()
    if args.command in ('measure', 'measure-row-update'):
        return measure(args.options, row_update=args.command == 'measure-row-update')
    if args.command == 'builder':
        print(builder_name())
        return 0
    if args.command == 'build' and args.options:
        parser.error('Use Compose options with up')
    app_build(args.options if args.command == 'up' else None)
    return 0


def terminate(signum, _frame):
    sys.exit(128 + signum)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, terminate)
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
    except KeyboardInterrupt:
        sys.exit(130)
