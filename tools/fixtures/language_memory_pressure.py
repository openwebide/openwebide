"""Bounded allocator for an owned, disposable Linux cgroup only.

The controller sends `fill` to approach the cgroup limit with 96 MiB headroom,
then observes the app's real memory policy. `release`, EOF or a 60 second deadline
releases every allocation. This process is not a language-server descendant.
"""
import json
from pathlib import Path
import selectors
import sys
import time

root = Path('/sys/fs/cgroup')
limit = int((root / 'memory.max').read_text())
assert limit == 2_147_483_648, 'Pressure fixture requires its owned 2 GiB cgroup'
headroom = 96 * 1024 * 1024
blocks = []
allocated = 0
deadline = time.monotonic() + 60
selector = selectors.DefaultSelector()
selector.register(sys.stdin, selectors.EVENT_READ)


def observation(state):
    print(json.dumps({'state': state, 'allocated_bytes': allocated,
        'memory_current_bytes': int((root / 'memory.current').read_text()),
        'memory_events': (root / 'memory.events').read_text().strip()}), flush=True)


observation('ready')
while time.monotonic() < deadline:
    if not selector.select(max(0, deadline - time.monotonic())):
        break
    command = sys.stdin.readline().strip()
    if command != 'fill':
        break
    while time.monotonic() < deadline:
        current = int((root / 'memory.current').read_text())
        size = min(4 * 1024 * 1024, limit - headroom - current)
        if size < 64 * 1024:
            break
        block = bytearray(size)
        block[::4096] = b'x' * len(block[::4096])
        blocks.append(block)
        allocated += size
    observation('holding')
blocks.clear()
observation('released')
