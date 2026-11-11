"""Docker primitives for the shared production language-service measurements.

The normal final app image runs Spin, bridge and language servers inside one
2 CPU / 2 GiB cgroup. The browser is a separate client. Only owned fixture paths
are mounted; language policy, installation and UI behavior stay in the app.
"""
import json
import os
from pathlib import Path
from language_process_observation import process_residency
import queue
import runpy
import re
import secrets
import socket
import subprocess
import threading
import time
from types import SimpleNamespace
import urllib.error

from language_host_support import ExecutionHost

ROOT = Path(__file__).resolve().parent.parent
Runtime = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))['Runtime']


def docker(*arguments, timeout=30):
    result = subprocess.run(['docker', *arguments], text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError('Docker fixture command failed: ' + result.stderr.strip()[:1200])
    return result.stdout


def free_port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


class ContainerRuntime(Runtime):
    def __init__(self, environment):
        super().__init__(environment.state / 'spin', frontend=True)
        self.environment = environment
        self.state.mkdir()

    def start(self):
        self.environment.start()
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            self.environment.assert_running()
            try:
                self.request('GET', '/api/health', authenticated=False)
                return
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                time.sleep(.1)
        raise AssertionError('Whole app container did not become ready')

    def stop(self):
        self.environment.stop()


class ContainerHost(ExecutionHost):
    def __init__(self, environment):
        self.environment = environment
        self.runtime = environment.runtime
        self.url = f'http://127.0.0.1:{environment.bridge_port}'
        self.pairing = environment.pairing
        self.process = None

    def start(self):
        self.environment.start()
        self.process = SimpleNamespace(pid=self.environment.bridge_pid)

    def stop(self):
        self.environment.stop()


class ContainerEnvironment:
    def __init__(self, state, parent, image, log_output=None):
        self.state, self.parent, self.image = state, parent, image
        self.log_output = log_output
        self.name = 'openwebide-language-' + secrets.token_hex(6)
        self.pairing, self.secret = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        self.bridge_port = free_port()
        self.runtime = ContainerRuntime(self)
        self.host = ContainerHost(self)
        self.run_id=os.environ.get('OPENWEBIDE_MEASUREMENT_RUN',self.name)
        self.metadata = {'kind': 'whole_app_container', 'container_name':self.name, 'run_id':self.run_id, 'browser_outside_server_cgroup': True,
                         'sampling_interval_seconds': 1, 'status': 'not_started'}
        self.ready, self.created, self.stopped = False, False, False
        self.bridge_pid = None
        self.envfile = state / 'container.env'

    def start(self):
        if self.ready:
            return
        details = json.loads(docker('image', 'inspect', self.image))[0]
        assert details['Os'] == 'linux', 'The benchmark requires a Linux app image'
        self.metadata.update(image_id=details['Id'], image_architecture=details['Architecture'],
            runtime_source_sha256=(details['Config'].get('Labels') or {}).get('org.openwebide.verification.source-sha256'))
        values = {'OPENWEBIDE_BRIDGE_SECRET': self.secret, 'SPIN_VARIABLE_BRIDGE_SECRET': self.secret,
                  'OPENWEBIDE_BRIDGE_TOKEN': self.pairing,
                  'OPENWEBIDE_BRIDGE_ALLOWED_ORIGINS': self.runtime.url}
        with os.fdopen(os.open(self.envfile, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as output:
            output.write(''.join(f'{key}={value}\n' for key, value in values.items()))
        relative = self.parent.relative_to(ROOT.parent.parent).as_posix()
        self.created = True
        docker('run', '--detach', '--init', '--name', self.name, '--label', 'org.openwebide.measurement_run='+self.run_id, '--cpus', '2',
               '--memory', '2147483648', '--memory-swap', '2147483648',
               '--publish', f'127.0.0.1:{self.runtime.port}:3000',
               '--publish', f'127.0.0.1:{self.bridge_port}:3001',
               '--mount', f'type=bind,src={self.runtime.state},dst=/app/.spin',
               '--mount', f'type=bind,src={self.parent},dst=/workspace/{relative}',
               '--env-file', str(self.envfile), self.image, timeout=60)
        details = json.loads(docker('inspect', self.name))[0]
        config = details['HostConfig']
        assert config['NanoCpus'] == 2_000_000_000, 'CPU quota was not applied'
        assert config['Memory'] == config['MemorySwap'] == 2_147_483_648, 'Memory/swap quota was not applied'
        self.metadata['limits'] = {'nano_cpus': config['NanoCpus'], 'memory_bytes': config['Memory'],
                                   'memory_plus_swap_bytes': config['MemorySwap']}
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            self.assert_running()
            roots = [pid for pid, (_, _, name) in self.processes().items() if name.startswith('openwebide-brid')]
            if len(roots) == 1:
                self.bridge_pid = roots[0]
                break
            time.sleep(.1)
        else:
            raise AssertionError('Container bridge process was not observed')
        cgroup = self.cgroup()
        quota, period = map(int, cgroup['cpu.max'].split())
        assert quota == 2 * period and cgroup['memory.max'] == '2147483648', 'Cgroup limits differ from the requested quotas'
        assert cgroup['memory.swap.max'] == '0', 'The benchmark must not use swap'
        self.metadata.update(status='running', initial_cgroup=cgroup)
        self.ready = True

    def assert_running(self):
        state = json.loads(docker('inspect', self.name))[0]['State']
        assert state['Running'] and not state.get('OOMKilled'), 'Whole app container exited or was OOM killed'

    def processes(self):
        rows = {}
        for line in docker('top', self.name, '-eo', 'pid,ppid,rss,args').splitlines():
            fields = line.strip().split(None, 3)
            if len(fields) == 4 and fields[0].isdigit():
                pid, parent, kib, program = fields
                # Node may name its main thread independently of its executable.
                # This owned-container adapter retains only argv[0]'s basename.
                rows[int(pid)] = (int(parent), int(kib) * 1024, Path(program.split()[0]).name)
        return rows

    def cgroup(self):
        source = '''import json,pathlib
root=pathlib.Path('/sys/fs/cgroup')
names=['cpu.max','memory.max','memory.swap.max','memory.current','memory.peak','memory.events']
print(json.dumps({name:(root/name).read_text().strip() for name in names}))'''
        return json.loads(docker('exec', self.name, 'python3', '-c', source))

    def describe(self):
        source = '''import glob,hashlib,json,pathlib,platform
paths=['/usr/local/bin/openwebide-bridge','/app/target/wasm32-wasip2/release/openwebide_backend.wasm',*sorted(glob.glob('/app/frontend/dist/*.wasm'))]
print(json.dumps({'os':platform.system(),'release':platform.release(),'machine':platform.machine(),'hashes':{path:hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest() for path in paths}}))'''
        observed = json.loads(docker('exec', self.name, 'python3', '-c', source))
        self.metadata['runtime_platform'] = {key: observed[key] for key in ('os', 'release', 'machine')}
        return {'bridge_sha256': observed['hashes'].pop('/usr/local/bin/openwebide-bridge'),
                'artifacts': observed['hashes'],
                'node_version': docker('exec', self.name, 'node', '--version').strip(),
                'server_version': docker('exec', self.name, 'typescript-language-server', '--version').strip()}

    def residency(self, pid):
        return ContainerResidency(self, pid)

    def evict_under_pressure(self, roots, residency):
        """Exercise the app's default reserve policy, without changing its limits.

        The browser workflow freezes both clients before calling this primitive.
        Only this disposable container receives pressure; its allocator has its
        own deadline and keeps 96 MiB headroom below the asserted 2 GiB limit.
        """
        roots = set(roots)
        assert roots, 'Pressure check needs an observed native server'
        record = {'original_roots': sorted(roots), 'observations': []}
        self.metadata.setdefault('pressure_events', []).append(record)
        source = (ROOT / 'tools/fixtures/language_memory_pressure.py').read_text()
        process = subprocess.Popen(['docker', 'exec', '-i', self.name, 'python3', '-u', '-c', source],
            text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        messages = queue.Queue()
        def receive():
            for line in process.stdout:
                messages.put(line)
        reader = threading.Thread(target=receive, daemon=True)
        reader.start()
        def response():
            try:
                message = json.loads(messages.get(timeout=25))
            except queue.Empty as error:
                raise AssertionError('Owned pressure allocator did not respond') from error
            record['observations'].append(message)
            events = dict(line.split() for line in message['memory_events'].splitlines())
            assert all(int(events[name]) == 0 for name in ('max', 'oom', 'oom_kill')), 'Pressure fixture reached the cgroup memory limit'
            return message
        start = time.monotonic()
        try:
            assert response()['state'] == 'ready'
            for _ in range(4):
                process.stdin.write('fill\n'); process.stdin.flush()
                assert response()['state'] == 'holding'
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    sample = residency.sample()
                    # Reparenting can remove a live process from the bridge's
                    # descendants. Check the entire owned container before
                    # claiming that the original process tree has exited.
                    current = set(self.processes())
                    if not roots.intersection(current):
                        record.update(evicted_residency=sample,
                                      evicted_container_process_pids=sorted(current),
                                      eviction_seconds=time.monotonic()-start)
                        return record
                    time.sleep(.25)
            raise AssertionError('Native service survived repeated reserve pressure')
        finally:
            if process.poll() is None:
                process.stdin.write('release\n'); process.stdin.flush()
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                # Removing the owned container also terminates its allocator;
                # killing only the Docker client would not establish cleanup.
                self.stop()
                process.wait(timeout=10)
                raise AssertionError('Owned pressure allocator did not release promptly')
            reader.join(timeout=5)
            while not messages.empty():
                record['observations'].append(json.loads(messages.get_nowait()))
            record['allocator_returncode'] = process.returncode
            record['released'] = any(item['state'] == 'released' for item in record['observations'])
            assert process.returncode == 0 and record['released'], 'Owned pressure allocator failed: ' + process.stderr.read()[:1200]
            record['after_release_cgroup'] = self.cgroup()

    def stop(self):
        if self.stopped:
            return
        self.stopped = True
        try:
            if self.created:
                try:
                    docker('stop', '--time', '15', self.name, timeout=30)
                finally:
                    try:
                        details = json.loads(docker('inspect', self.name))[0]['State']
                        self.metadata.update(status='stopped', container_exit_code=details['ExitCode'], oom_killed=details.get('OOMKilled', False))
                        logs=subprocess.run(['docker','logs',self.name],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
                        (self.state / 'container.log').write_text(logs.stdout)
                        if self.log_output is not None:
                            self.log_output.write_text(logs.stdout)
                            self.metadata['log_file']=self.log_output.name
                        assert not details.get('OOMKilled'), 'Whole app container was OOM killed'
                    finally:
                        docker('rm', '--force', '--volumes', self.name)
        finally:
            self.envfile.unlink(missing_ok=True)


class ContainerResidency:
    def __init__(self, environment, pid):
        self.environment, self.pid = environment, pid
        self.samples, self.errors, self.stats = [], [], []
        self.stopping = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.stats_thread = None
        self.stats_process = None
        self.latest_stats = None

    def sample(self):
        rows = self.environment.processes()
        value = process_residency(rows, self.pid, time.monotonic())
        value['container_stats'] = self.latest_stats
        self.samples.append(value)
        return value

    def run(self):
        while not self.stopping.is_set():
            try:
                self.sample()
            except (OSError, ValueError, AssertionError, RuntimeError, subprocess.SubprocessError) as error:
                self.errors.append(str(error))
            self.stopping.wait(1)

    def read_stats(self):
        for line in self.stats_process.stdout:
            if self.stopping.is_set():
                break
            try:
                text=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',line).strip()
                if not text:continue
                entry = json.loads(text)
                # Store only resource measurements; no inspect environment or credentials.
                record = {key: entry[key] for key in ('CPUPerc','MemUsage','MemPerc','PIDs','BlockIO')}
                record['time'] = time.monotonic()
                self.latest_stats = record; self.stats.append(record)
            except (ValueError, KeyError) as error:
                self.errors.append('Docker resource stream: ' + str(error))

    def __enter__(self):
        self.stats_process = subprocess.Popen(['docker','stats','--format','{{json .}}','--no-trunc',self.environment.name],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.stats_thread = threading.Thread(target=self.read_stats, daemon=True)
        self.stats_thread.start(); self.thread.start()
        return self

    def __exit__(self, exc_type, _error, _traceback):
        self.stopping.set(); self.thread.join(timeout=5)
        self.stats_process.terminate()
        try:self.stats_process.wait(timeout=5)
        except subprocess.TimeoutExpired:self.stats_process.kill();self.stats_process.wait(timeout=5)
        self.stats_thread.join(timeout=5)
        assert not self.thread.is_alive() and not self.stats_thread.is_alive(), 'Container sampler did not stop'
        self.environment.metadata['resource_samples'] = self.stats
        if self.environment.ready:
            final = self.environment.cgroup()
            self.environment.metadata['final_cgroup'] = final
            events = dict(line.split() for line in final['memory.events'].splitlines())
            assert int(events.get('oom_kill', '0')) == 0, 'Container had an OOM kill'
        if exc_type is None:
            assert self.stats, 'No whole-container resource samples were collected'
