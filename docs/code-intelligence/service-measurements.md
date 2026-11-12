# Language-service reload and client handoff

`tools/measure-language-handoff-client.py` exercises the production WASM editor,
Spin backend, installed JavaScript/TypeScript plugin and real TypeScript language
server through the shared language facade and both authenticated transports.
It restores unsaved cross-file drafts after reload, then alternates two live page
clients whose library overlays return different types. Each query must reflect
its own client's overlay. Disk files must retain their original bytes.

Run after building the current release backend, frontend and bridge with compiled
bundled defaults, with the pinned TypeScript tools, Chrome and ChromeDriver on PATH:

```sh
python3 tools/measure-language-handoff-client.py --samples 8 --modules 128 --output /tmp/handoff-128.json
python3 tools/measure-language-handoff-client.py --samples 8 --modules 512 --output /tmp/handoff-512.json
```

The generated workspaces contain a dependency chain and exported object types.
Each mode has one first-load sample, seven subsequent reload samples and sixteen
alternating-client queries. Reloads must receive fresh logical service identities.
The probe records native root continuity, changed roots and overlapping-root
observations separately. Subsequent reloads can include process restarts; they
are not all classified as warm. After overlapping clients close, an explicit
settled check requires one idle native server for that project. Local browser folders use OPFS
and a controlled native proof correspondence; physical browser permissions are
covered by the [separate PWA checks](physical-folder-checks.json).

Current process observations follow the bridge's independent server owners.
They count each outermost Node server once, retain the watcher in the measured
child tree and include it in pressure-eviction checks. Native and container
samplers share this policy above their process-list primitives. The focused
contracts cover direct and watcher-owned servers, nested Node processes,
project isolation and matching adapter observations.

Ready time starts before navigation and ends after a hover confirms the restored
cross-file draft. Handoff time includes opening the UI command and waiting for
that client's exact result. Both include WebDriver polling and UI scheduling.
RSS samples observe the bridge and its descendants every 250 ms; short peaks can
be missed, and the sampler adds overhead. Local-stage aggregate child RSS includes
the still-idle Remote server. Post-GC CDP heap readings describe JavaScript heap,
not the complete frontend WASM allocation or browser footprint. RSS cannot establish
retained allocator heap or prove absence of a memory leak.

These observations are descriptive baselines. A single cold sample and small
repeated sample sets do not establish an improvement or a latency SLO. They do
not cover pressure eviction, another device/profile, persistent derived-cache
hits or other language servers. The complete constrained Linux app is measured separately below.

Recorded macOS run, 2026-11-09: Chrome 157, Node 26.13.0 and
TypeScript language server 6.0.3, installed plugin 0.2.0. All 32 reloads and
64 alternating-client queries preserved the expected drafts and original disk
bytes. Full samples and artifact hashes are in
[the measurement record](handoff-measurements.json).

| Dependency modules | Mode | First ready (s) | Warm reload median / p95 (s) | Alternating query median / p95 (s) | Peak bridge RSS (MiB) | Peak children RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 128 | Remote | 1.85 | 0.83 / 1.10 | 0.43 / 1.82 | 464 | 327 |
| 128 | Local | 1.41 | 0.84 / 1.10 | 0.44 / 0.66 | 457 | 575 |
| 512 | Remote | 2.10 | 0.89 / 0.91 | 0.35 / 1.54 | 453 | 378 |
| 512 | Local | 2.05 | 0.85 / 1.22 | 0.45 / 1.47 | 435 | 790 |

The 128-module workspaces contain 132 files / 163,637 source bytes; the
512-module workspaces contain 516 files / 670,133 source bytes. The original
native server processes survived every reload. Both modes returned each page's
own unsaved type during alternating queries. In the 128-module cases and the
512-module Remote case, changing logical server identities accompanied process
reuse. The 512-module Local case retained two distinct logical services and an
additional pair of Node processes throughout the alternating queries. This run
therefore demonstrates isolation with additional service residency, rather than
one process shared throughout that phase. This baseline motivated the idle-service consolidation measured below.

Post-GC JavaScript heap was approximately 4 MiB per reloaded page for 128 modules
and 7.6 MiB for 512 modules. This limited
sample does not establish stable complete retained memory. Pressure eviction,
other servers and complete retained-memory accounting remain open.

## Idle-service consolidation

A follow-up host change retains one compatible idle process after overlapping
requests finish, while retaining separate source inventories and request
watermarks. Active requests and command reviews remain pinned. Acquisition and
the periodic idle pass use the same policy. Cache leases remain protected until
the original retired process tree confirms cleanup. Memory observations retain only original-process
identity, so OS sampling does not itself prevent handoff or misapply a stale
reading to a replacement process.

The same workloads passed another 32 reloads and 64 alternating-client queries,
with original disk bytes intact. An additional final query and bounded wait check
that each project's overlapping clients settle to one native server root.
[Full follow-up samples](idle-reuse-measurements.json) include parent PIDs and
that settled observation. The baseline did not have the final settled check;
its comparison value below is the last alternating-query observation.

| Modules | Mode | Subsequent reload median / p95 (s) | Alternating query median / p95 (s) | Baseline final child RSS (MiB) | Current settled child RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: |
| 128 | Remote | 0.82 / 1.25 | 0.45 / 1.87 | 291 | 295 |
| 128 | Local | 0.82 / 0.83 | 0.35 / 0.67 | 481 | 587 |
| 512 | Remote | 0.89 / 0.90 | 0.36 / 0.68 | 343 | 345 |
| 512 | Local | 0.93 / 0.95 | 0.35 / 0.73 | 727 | 689 |

These samples establish source isolation and idle convergence for these runs.
Subsequent reloads are not all assumed warm: root continuities, replacements and
ambiguous overlapping-root transitions are recorded explicitly. Those observations
do not assign a response to one process while multiple roots overlap.
They do not establish a general memory reduction or a latency improvement. The
Local rows include idle Remote services, sampling can miss peaks, and active
clients may still require independent processes. Pressure eviction, complete
retained-memory accounting and broader server/device coverage remain open.

## Constrained whole-app Linux run

The same shared workflow also passed in the normal final app image on Linux
arm64, with Spin, the native bridge and language-server processes in one cgroup.
Docker inspection and cgroup readings verified 2 CPUs, 2 GiB memory and no swap.
The browser, ChromeDriver and Local OPFS correspondence client run outside that
server cgroup. Only disposable project and state directories are mounted.

```sh
python3 tools/measure-language-handoff-client.py --samples 8 --modules 128 --container-image IMAGE_ID --output /tmp/container-128.json
python3 tools/measure-language-handoff-client.py --samples 8 --modules 512 --container-image IMAGE_ID --output /tmp/container-512.json
```

The recorded image ID and runtime source digest, installed plugin, actual Linux
artifacts and Node/server versions are in the [full record](container-measurements.json).
The measurement harness is hashed separately: it was extended after the image's
runtime sources were copied. This image runs Node 24.22.2 and TypeScript language
server 6.0.3; the macOS run above uses Node 26.13.0. Differences between these
samples cannot be attributed solely to platform, quota or code changes.

| Modules | Mode | First ready (s) | Subsequent reload median / p95 (s) | Alternating query median / p95 (s) | Peak bridge RSS (MiB) | Peak children RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 128 | Remote | 2.46 | 1.20 / 1.97 | 0.45 / 0.77 | 362 | 316 |
| 128 | Local | 1.40 | 1.08 / 1.19 | 0.46 / 0.68 | 368 | 620 |
| 512 | Remote | 2.60 | 1.22 / 2.52 | 0.47 / 0.80 | 380 | 352 |
| 512 | Local | 1.71 | 1.06 / 1.16 | 0.45 / 0.67 | 392 | 711 |

Across both workloads, all 32 reloads and 64 alternating-client queries retained
the correct independent drafts and original disk bytes. Every measured reload
transition retained one native server root, and each project settled to one idle
server after client overlap. Local-stage child RSS includes the idle Remote server.
The container-wide cgroup peaks were 1,147,764,736 bytes (1.07 GiB) for 128 modules
and 1,282,772,992 bytes (1.19 GiB) for 512 modules. Both runs recorded zero memory
limit, OOM and OOM-kill events and removed their owned containers after shutdown.

Process RSS is sampled each second and at query boundaries; short peaks can be
missed. Cgroup peak covers startup and bounded control reads as well as the app;
Docker working-set samples start after installation and use different accounting.
These are finite successful workload observations, not pressure-eviction or
memory-leak proofs. Complete retained native/WASM memory, other language servers,
pressure eviction and other devices/profiles remain open. The editor's separate
paint/readiness targets are not evaluated by these UI hover timings.

## Idle recovery under real cgroup pressure

The same image and default host memory policy also passed the 128/512-module
workflows with an additional pressure/recovery phase in each mode. Both page
clients are frozen after their distinct overlays have been checked. A disposable
Python process outside the language-server tree allocates and touches pages inside
the same 2 GiB cgroup, keeping approximately 96 MiB below its limit. This enters
the host's 128 MiB reserve without changing its configured limits or killing a
server directly. If the older Remote project is reclaimed first during Local's
phase, the allocator fills again to exercise the current project's eviction too.

The check waits for the entire observed original server tree to disappear,
releases the allocator, preserves the real five-second retry backoff and resumes
both clients. Each client's next hover must return its own string/boolean library
overlay, with a fresh service identity and replacement process. The existing disk
preservation and single-idle-server convergence assertions still run afterward.
The allocator has a 60-second deadline; EOF or an explicit release also stops it.

```sh
python3 tools/measure-language-handoff-client.py --samples 8 --modules 128 --container-image IMAGE_ID --exercise-pressure --output /tmp/pressure-128.json
python3 tools/measure-language-handoff-client.py --samples 8 --modules 512 --container-image IMAGE_ID --exercise-pressure --output /tmp/pressure-512.json
```

| Modules | Mode | Observed eviction time (s) | Pressure fills | Peak fixture allocation (MiB) | Restored client types |
| --- | --- | ---: | ---: | ---: | --- |
| 128 | Remote | 2.26 | 1 | 1059 | string / boolean |
| 128 | Local | 9.41 | 2 | 1057 | string / boolean |
| 512 | Remote | 0.76 | 1 | 1036 | string / boolean |
| 512 | Local | 10.77 | 2 | 1032 | string / boolean |

[Full pressure samples](pressure-measurements.json) include all process trees,
resource observations and artifact/harness hashes. Both representative workloads
passed, after an additional 32-module smoke case. Each preserved original disk
bytes and both independent unsaved overlays, recorded no memory-limit/OOM events,
released its allocator and removed its owned container. Cgroup peaks near 1.91 GiB
in these runs include the artificial allocator; they are not application-footprint
measurements. The unpressured app peaks are reported in the preceding section.

These cases verify idle pressure recovery. They freeze UI requests during
allocation and do not establish behavior for in-flight analysis, pinned commands
or sustained pressure during editing. Complete retained native/WASM memory,
broader pressure behavior and other language servers/devices remain open.

The recorded runs above predate the independent server-owner integration. Their
pressure assertions checked disappearance from the bridge's descendant set;
they did not independently establish that a reparented process exited the
container. The corrected probe checks the original PIDs against the entire owned
container both at eviction and after each client's recovery. Its regression
keeps a reparented server alive after its watcher disappears and rejects early
cleanup. The fresh production runs below use that check; the historical numbers
above have not been relabelled as current measurements.

## Current process-owner measurements

The 2026-11-11 rerun uses the corrected shared process sampler and application
commit `9f16a87d3b8648cd3bea094703f177c0db7941ef`. Each outermost Node process is
identified beneath its independent watcher; nested Node children are counted
once, and watcher residency remains included. The
[complete record](owner-process-measurements.json) separates measurement source
from the previously verified native artifacts and immutable final Linux image.
Earlier records above remain historical observations of their original runtime.

All 64 reloads and 128 alternating-client queries passed across native macOS and
the complete Linux app, preserving both clients' unsaved overlays and original
disk bytes. Each project settled to one idle server after client overlap.
Every reload and alternating-query transition observed the same single root
for its project. These counts exclude the additional settled and recovery queries.

| Runtime | Modules | Mode | First ready (s) | Subsequent reload median / p95 (s) | Alternating query median / p95 (s) | Peak bridge RSS (MiB) | Peak children RSS (MiB) |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| macOS | 128 | Remote | 1.91 | 0.81 / 0.82 | 0.44 / 2.16 | 457 | 347 |
| macOS | 128 | Local | 1.97 | 0.93 / 0.93 | 0.45 / 0.77 | 379 | 625 |
| macOS | 512 | Remote | 2.15 | 1.15 / 2.60 | 0.67 / 0.89 | 463 | 383 |
| macOS | 512 | Local | 2.38 | 1.12 / 1.17 | 0.68 / 0.90 | 380 | 713 |
| Linux | 128 | Remote | 4.15 | 1.17 / 1.28 | 0.44 / 0.86 | 407 | 314 |
| Linux | 128 | Local | 1.38 | 0.95 / 0.97 | 0.44 / 0.65 | 416 | 608 |
| Linux | 512 | Remote | 2.70 | 1.26 / 2.60 | 0.45 / 0.88 | 354 | 366 |
| Linux | 512 | Local | 1.54 | 0.99 / 1.03 | 0.45 / 0.66 | 362 | 709 |

Both Linux workloads retained verified 2 CPU / 2 GiB / no-swap limits. In all
four mode/workload pressure phases, the original server and owning watcher PIDs
disappeared from the whole owned container's process inventory, including checks
after both clients recovered. The allocator exited successfully and released its
pages. Cgroup peaks, including that deliberate allocator, were 2,047,029,248 and
2,051,137,536 bytes for 128 and 512 modules; both recorded zero memory-limit, OOM
and OOM-kill events. Both owned containers were removed after shutdown.

Ready and handoff timings include any bounded retry of the specific
"Project files changed during language analysis" response. Retry events remain
in each report; this successful rerun recorded zero. An earlier attempt encountered
that startup response and stopped before collecting a successful first sample.
Unknown errors are not retried. The sampler's whole-container exit check also
has a regression where a retired server remains alive after reparenting.

These finite TypeScript observations do not establish complete retained native
or WASM memory stability, active-request or command-pressure behavior, derived
cache hit rates, editor paint targets or a prefetch performance benefit. Local
RSS still includes any idle Remote service. Sampling can miss short process peaks;
the cgroup peak uses different accounting and includes pressure allocation.

## Retained browser memory and natural residency

`tools/measure-language-memory.py` repeats the installed TypeScript reload and
independent-overlay handoff workflow in both modes. It records owned Spin,
bridge/watcher/server and Chrome/driver RSS every 250 ms, and records retained
V8 heap and live unshared WASM linear capacities in separate post-GC runs.

```sh
python3 tools/measure-language-memory.py --collection post-gc --samples 32 --modules 128 --idle-snapshots 16 --output /tmp/language-retained-128
python3 tools/measure-language-memory.py --collection post-gc --samples 16 --modules 512 --output /tmp/language-retained-512
python3 tools/measure-language-memory.py --collection natural --samples 16 --modules 512 --output /tmp/language-natural-512
```

The collector deduplicates V8 isolates and default execution realms. It enumerates
live `WebAssembly.Memory` objects and cross-checks each app page against its actual
Trunk-startup export. A disposable dedicated-worker calibration allocates and
releases a 4 MiB ArrayBuffer and an 8 MiB WASM memory. Chrome's backing-storage
counter omits that WASM allocation; the direct counter observes it and its release.
The application workload does not use a dedicated worker for these language requests.

[The full finite record](retained-memory-measurements.json) includes complete
query-phase time series, block medians, artifact/source fingerprints and natural
RSS peaks. Artifacts identify the actual measured bytes; the uncommitted source
manifest does not assign the preserved release bridge/backend to a newer producer.
Natural residency includes browser tooling and counts shared pages in each process;
it is neither unique physical memory nor live native allocation. Sampling begins
after authenticated browser preparation and can miss short peaks. Local-stage
native residency includes the still-idle Remote service.

| Modules | Mode | Alternating queries | Retained V8 first / last (MiB) | WASM capacity during handoff (MiB) |
| --- | --- | ---: | ---: | ---: |
| 128 | Remote | 64 | 8.23 / 8.41 | 37.56 |
| 128 | Local | 64 | 8.32 / 8.49 | 37.50 |
| 512 | Remote | 32 | 15.48 / 15.61 | 102.94 |
| 512 | Local | 32 | 15.54 / 15.66 | 103.25 |

The V8 values sum the owned browser isolates, including tooling realms. Two app
pages remain open during handoff. The 128-module run then closes the second page
and takes 16 idle snapshots per mode: Remote retained heap changes from 4,504,764
to 4,504,836 bytes and Local from 4,519,604 to 4,521,836 bytes. Those snapshots
record zero background language requests; they are a different one-page workload.

| Natural 512-module run | Peak Spin + bridge/server RSS (MiB) | Peak browser/driver RSS (MiB) | Peak owned-process union RSS (MiB) |
| --- | ---: | ---: | ---: |
| Remote | 907 | 1982 | 2814 |
| Local | 1205 | 2412 | 3612 |

Columns peak independently; summing their maxima does not give a simultaneous
process-union peak. The separate constrained Linux measurements above retain
their own cgroup accounting and runtime provenance.

Retaining action-menu controls once per mounted component fixes repeated
parent-owned subscriptions while preserving dialog callbacks and reactive options.
Controls detach on dismissal, restore focus on reopen and reject direct stale DOM
clicks after dismissal or unmount. Browser regressions cover both modes and verify
subscription disposal when the whole component unmounts. Isolated repeated menu and
language-result-dialog tests also retain a bounded number of WASM browser references.

The full DOM series warms up early and then levels off; a first/last comparison
alone would incorrectly describe that bounded change as continuing growth. Retained
JavaScript heap still shows a small increasing trend across the finite handoff
series. Idle observations issue no UI queries and report any background language
requests separately; they do not attribute all residual heap changes to the
collector or app. These observations do not establish complete retained native/WASM
allocator stability or explain every remaining heap increase. WASM
linear capacity is not live Rust allocation. Active analysis/command pressure,
other servers and devices, and a prefetch benefit remain unverified.
