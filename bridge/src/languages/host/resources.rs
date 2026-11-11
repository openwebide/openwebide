//! Process-tree readings and one memory policy for both authenticated transports.
use super::*;
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Copy, Debug)]
pub struct MemoryLimits {
    pub service_bytes: u64,
    pub total_bytes: u64,
    pub reserve_bytes: u64,
}
impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            // Includes compiler and proc-macro children during ordinary project
            // startup; the aggregate host budget still bounds concurrent work.
            service_bytes: 1024 * 1024 * 1024,
            total_bytes: 1536 * 1024 * 1024,
            reserve_bytes: 128 * 1024 * 1024,
        }
    }
}
impl MemoryLimits {
    /// Execution-host policy, separate from database-backed user preferences.
    pub fn from_environment() -> Result<Self, String> {
        let defaults = Self::default();
        let read = |name: &str, default| match std::env::var(name) {
            Ok(value) => value
                .parse::<u64>()
                .ok()
                .and_then(|mib| mib.checked_mul(1024 * 1024))
                .ok_or_else(|| format!("{name} must be an integer MiB budget")),
            Err(std::env::VarError::NotPresent) => Ok(default),
            Err(error) => Err(format!("{name}: {error}")),
        };
        Self {
            service_bytes: read(
                "OPENWEBIDE_BRIDGE_LANGUAGE_SERVICE_MEMORY_MIB",
                defaults.service_bytes,
            )?,
            total_bytes: read(
                "OPENWEBIDE_BRIDGE_LANGUAGE_TOTAL_MEMORY_MIB",
                defaults.total_bytes,
            )?,
            reserve_bytes: read(
                "OPENWEBIDE_BRIDGE_LANGUAGE_MEMORY_RESERVE_MIB",
                defaults.reserve_bytes,
            )?,
        }
        .validate()
    }
    fn validate(self) -> Result<Self, String> {
        if self.service_bytes < 1024 * 1024 || self.total_bytes < self.service_bytes {
            return Err("Language memory limits require at least 1 MiB per service and a larger total budget".into());
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ResourceUsage {
    pub resident_bytes: u64,
    pub peak_resident_bytes: u64,
    pub processes: usize,
    pub available_bytes: u64,
    pub evictions: u64,
    pub cache_bytes: u64,
    pub peak_cache_bytes: u64,
    pub cache_evictions: u64,
    pub cache_sample_errors: u64,
    pub cache_sample_complete: bool,
}
#[derive(Default)]
struct Tree {
    resident_bytes: u64,
    processes: usize,
}
struct Reading {
    trees: BTreeMap<u32, Tree>,
    available_bytes: u64,
}

/// The OS adapter supplies parent relationships and resident bytes. Do not count
/// Linux threads as separate address spaces or read process commands/environment.
fn read(roots: &[u32]) -> Reading {
    let mut system = System::new();
    system.refresh_memory();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().without_tasks(),
    );
    let processes = system.processes();
    let roots = roots.iter().copied().map(Pid::from_u32).collect::<Vec<_>>();
    let mut trees = BTreeMap::<u32, Tree>::new();
    for (pid, process) in processes {
        if let Some(root) = owning_root(*pid, processes, &roots) {
            let tree = trees.entry(root.as_u32()).or_default();
            tree.resident_bytes = tree.resident_bytes.saturating_add(process.memory());
            tree.processes += 1;
        }
    }
    Reading {
        trees,
        available_bytes: system.cgroup_limits().map_or_else(
            || system.available_memory(),
            |group| group.free_memory.min(system.available_memory()),
        ),
    }
}

pub(super) fn owning_root(
    pid: Pid,
    processes: &std::collections::HashMap<Pid, sysinfo::Process>,
    roots: &[Pid],
) -> Option<Pid> {
    let mut parent = Some(pid);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(pid) = parent {
        if !seen.insert(pid) {
            return None;
        }
        if roots.contains(&pid) {
            return Some(pid);
        }
        parent = processes.get(&pid).and_then(sysinfo::Process::parent);
    }
    None
}

struct Candidate<'a> {
    entry: &'a Entry,
    bytes: u64,
    busy: bool,
    used: Instant,
}
impl Host {
    pub(crate) fn set_memory_limits(&mut self, limits: MemoryLimits) -> Result<(), String> {
        let limits = limits.validate()?;
        Arc::get_mut(&mut self.inner)
            .ok_or("Configure language memory limits before using the host")?
            .limits = limits;
        Ok(())
    }
    pub fn with_memory_limits(limits: MemoryLimits) -> Result<Self, String> {
        Self::with_runtime(limits, crate::folder_coordination::Coordinator::default())
    }
    pub(crate) fn with_runtime(
        limits: MemoryLimits,
        coordinator: crate::folder_coordination::Coordinator,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: Arc::new(Inner {
                limits: limits.validate()?,
                coordinator,
                ..Inner::default()
            }),
        })
    }
    /// Last completed OS sample; counters include pressure and per-service
    /// evictions. A zero sample before startup is not a measured cold baseline.
    pub fn resource_usage(&self) -> ResourceUsage {
        self.inner.resources.lock().unwrap().clone()
    }
}

pub(super) fn snapshot(inner: &Inner) -> Vec<ProcessIdentity> {
    inner
        .entries
        .lock()
        .unwrap()
        .values()
        .filter(|entry| !*entry.stopped.borrow())
        .filter_map(|entry| entry.process_identity())
        .collect()
}

pub(super) async fn enforce(inner: &Arc<Inner>) {
    let entries = snapshot(inner);
    if entries.is_empty() {
        let mut usage = inner.resources.lock().unwrap();
        usage.resident_bytes = 0;
        usage.processes = 0;
        return;
    }
    let roots = entries.iter().map(|entry| entry.pid).collect::<Vec<_>>();
    let Ok(reading) = tokio::task::spawn_blocking(move || read(&roots)).await else {
        return;
    };
    apply(inner, entries, reading);
}

fn apply(inner: &Inner, observations: Vec<ProcessIdentity>, reading: Reading) {
    // Handoff may have changed the editing client during the OS scan. Resolve
    // the original process's current owner under the acquisition lock, using
    // its cleanup channel as well as PID. Retired/replaced processes do not
    // authorize decisions against another service that reuses a key or PID.
    let entries = inner.entries.lock().unwrap();
    let mut candidates = Vec::new();
    let mut resident = 0_u64;
    let mut processes = 0_usize;
    for observation in observations {
        let Some(tree) = reading.trees.get(&observation.pid) else {
            continue;
        };
        let Some(entry) = entries.values().find(|entry| {
            !*entry.stopped.borrow()
                && entry
                    .process_identity()
                    .is_some_and(|current| current.matches(&observation))
        }) else {
            continue;
        };
        resident = resident.saturating_add(tree.resident_bytes);
        processes += tree.processes;
        let busy = entry.busy();
        let used = *entry.used.lock().unwrap();
        candidates.push(Candidate {
            entry,
            bytes: tree.resident_bytes,
            busy,
            used,
        });
    }
    {
        let mut usage = inner.resources.lock().unwrap();
        usage.resident_bytes = resident;
        usage.peak_resident_bytes = usage.peak_resident_bytes.max(resident);
        usage.processes = processes;
        usage.available_bytes = reading.available_bytes;
    }
    // Reclaim idle services first. An individual service exceeding its budget
    // is stopped even while a read is active; the request reports that failure.
    candidates.sort_by_key(|candidate| {
        (
            candidate.bytes <= inner.limits.service_bytes,
            candidate.busy,
            candidate.used,
        )
    });
    let target = inner.limits.total_bytes.min(
        resident.saturating_sub(
            inner
                .limits
                .reserve_bytes
                .saturating_sub(reading.available_bytes),
        ),
    );
    let mut reclaim = resident.saturating_sub(target);
    for candidate in candidates {
        if candidate.bytes <= inner.limits.service_bytes && reclaim == 0 {
            continue;
        }
        if *candidate.entry.stopped.borrow() {
            continue;
        }
        let reason = if candidate.bytes > inner.limits.service_bytes {
            format!(
                "Language service exceeded its process-tree memory budget ({} resident bytes)",
                candidate.bytes
            )
        } else {
            "Language service was evicted under execution-host memory pressure".into()
        };
        {
            let mut failure = candidate.entry.failure.lock().unwrap();
            failure.until = Some(Instant::now() + Duration::from_secs(5));
            failure.reason = reason;
        }
        candidate.entry.stop();
        reclaim = reclaim.saturating_sub(candidate.bytes);
        let mut usage = inner.resources.lock().unwrap();
        usage.evictions = usage.evictions.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn resident_readings_include_child_processes_and_exclude_unrelated_processes() {
        let root = tempfile::tempdir().unwrap();
        let server = serde_json::from_value(json!({"command":"python3","args":["-c",
            "import subprocess,time; subprocess.Popen(['python3','-c','import time; data=bytearray(48*1024*1024); time.sleep(60)']); time.sleep(60)"]})).unwrap();
        let (mut process, _) = Process::spawn(&server, root.path()).unwrap();
        let pid = process.id().unwrap();
        let mut measured = false;
        for _ in 0..50 {
            let reading = tokio::task::spawn_blocking(move || read(&[pid]))
                .await
                .unwrap();
            if reading
                .trees
                .get(&pid)
                .is_some_and(|tree| tree.processes >= 2 && tree.resident_bytes >= 48 * 1024 * 1024)
            {
                measured = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        process.shutdown().await;
        assert!(
            measured,
            "The child allocation must be charged to the language service"
        );
        assert!(read(&[]).trees.is_empty());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use openwebide_core::plugins::languages::source_digest;

    #[tokio::test]
    async fn memory_eviction_reports_pressure_backs_off_and_resynchronizes_unsaved_source() {
        for pressure in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("main.notes"), "disk source").unwrap();
            let script = root.path().join("server.py");
            std::fs::write(&script, r#"import json,sys
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value}).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
source=''
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        key,value=line.decode().split(':',1);headers[key.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'hoverProvider':True}}
    elif method=='textDocument/didOpen':source=params['textDocument']['text']
    elif method=='textDocument/hover':result={'contents':source}
    if 'id' in message:send({'id':message['id'],'result':result})
"#).unwrap();
            let host = Host::with_memory_limits(MemoryLimits {
                service_bytes: if pressure {
                    1024 * 1024 * 1024
                } else {
                    1024 * 1024
                },
                total_bytes: 1024 * 1024 * 1024,
                // Force an OS-independent pressure case without allocating host RAM.
                reserve_bytes: if pressure { u64::MAX } else { 0 },
            })
            .unwrap();
            let scope = Scope {
                owner: "account".into(),
                project: 1,
                root: root.path().into(),
                provider: "community/notes".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({"command":"python3","args":[script]}))
                    .unwrap(),
            };
            let mut input = Request {
                version: protocol::VERSION,
                client: "client".into(),
                document: "document".into(),
                request: 1,
                revision: 1,
                language: "notes".into(),
                path: "main.notes".into(),
                source: "unsaved 文😀".into(),
                source_digest: source_digest("unsaved 文😀"),
                query: Query::Hover {
                    position: protocol::Position::default(),
                },
            };
            let initial = host.query(scope.clone(), &input).await.unwrap();
            assert_eq!(initial.result["contents"], "unsaved 文😀");
            enforce(&host.inner).await;
            let usage = host.resource_usage();
            assert!(usage.resident_bytes > 1024 * 1024);
            assert_eq!(usage.evictions, 1);
            assert!(usage.processes >= 1);
            input.request += 1;
            let error = host.query(scope.clone(), &input).await.unwrap_err();
            assert!(error.contains("waiting to retry"), "{error}");
            assert!(
                error.contains(if pressure {
                    "memory pressure"
                } else {
                    "process-tree memory budget"
                }),
                "{error}"
            );
            // Advance only the retry deadline; the real stopped native session is
            // replaced and must learn the unsaved source through didOpen again.
            for entry in host.inner.entries.lock().unwrap().values() {
                entry.failure.lock().unwrap().until = Some(Instant::now() - Duration::from_secs(1));
            }
            input.request += 1;
            let recovered = host.query(scope, &input).await.unwrap();
            assert_eq!(recovered.result["contents"], "unsaved 文😀");
            assert_ne!(recovered.server, initial.server);
            assert_eq!(
                std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
                "disk source"
            );
            host.shutdown();
        }
    }
}

#[cfg(test)]
mod sampling_tests {
    use super::*;
    use crate::languages::host::reuse::tests::{Fixture, input};

    fn over_budget(pid: u32) -> Reading {
        Reading {
            trees: BTreeMap::from([(
                pid,
                Tree {
                    resident_bytes: 2 * 1024 * 1024 * 1024,
                    processes: 1,
                },
            )]),
            available_bytes: u64::MAX,
        }
    }

    fn fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture
            .host
            .inner
            .reaping
            .store(true, std::sync::atomic::Ordering::Relaxed);
        fixture
    }

    #[tokio::test]
    async fn memory_pressure_follows_the_original_process_after_client_handoff() {
        let fixture = fixture();
        let (_, first) = fixture.query(&input("A", "a.notes", "A", 1)).await;
        fixture.query(&input("A", "b.notes", "hidden A", 2)).await;
        let observed = snapshot(&fixture.host.inner);
        let (_, second) = fixture.query(&input("B", "a.notes", "B", 1)).await;
        assert_eq!(first["pid"], second["pid"]);
        let pid = observed[0].pid;
        apply(&fixture.host.inner, observed, over_budget(pid));
        assert_eq!(fixture.host.resource_usage().evictions, 1);
        {
            let entries = fixture.host.inner.entries.lock().unwrap();
            let current = entries.iter().find(|(key, _)| key.client == "B").unwrap().1;
            assert!(*current.stopped.borrow());
            let parked = entries.iter().find(|(key, _)| key.client == "A").unwrap().1;
            assert!(!*parked.stopped.borrow());
        }
        let error = fixture
            .host
            .query(fixture.scope.clone(), &input("B", "a.notes", "B", 2))
            .await
            .unwrap_err();
        assert!(error.contains("waiting to retry"), "{error}");
        let (_, restored) = fixture.query(&input("A", "a.notes", "A after", 3)).await;
        assert_ne!(restored["pid"], first["pid"]);
        assert_eq!(
            restored["open"],
            json!({"a.notes":"A after","b.notes":"hidden A"})
        );
        assert_eq!(fixture.starts(), 2);
    }

    #[tokio::test]
    async fn retired_process_readings_do_not_evict_a_replacement_with_the_same_numeric_pid() {
        let fixture = fixture();
        fixture.query(&input("A", "a.notes", "A", 1)).await;
        let mut observed = snapshot(&fixture.host.inner);
        fixture
            .host
            .revoke(&fixture.scope.owner, Some(fixture.scope.project), None);
        let (_, replacement) = fixture.query(&input("B", "a.notes", "B", 1)).await;
        let current = snapshot(&fixture.host.inner);
        assert!(!observed[0].completion.same_process(&current[0].completion));
        // Model kernel PID reuse without forcing or depending on an OS PID wrap.
        observed[0].pid = current[0].pid;
        apply(&fixture.host.inner, observed, over_budget(current[0].pid));
        assert_eq!(fixture.host.resource_usage().evictions, 0);
        let (_, continued) = fixture.query(&input("B", "a.notes", "B after", 2)).await;
        assert_eq!(continued["pid"], replacement["pid"]);
        assert_eq!(fixture.starts(), 2);
    }

    #[tokio::test]
    async fn startup_samples_own_the_original_process_cleanup_without_retaining_the_service() {
        let fixture = fixture();
        std::fs::write(fixture.root.path().join("hold-initialize"), "").unwrap();
        let host = fixture.host.clone();
        let scope = fixture.scope.clone();
        let pending =
            tokio::spawn(async move { host.query(scope, &input("A", "a.notes", "A", 1)).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.root.path().join("initialize-started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let observed = snapshot(&fixture.host.inner);
        assert_eq!(observed.len(), 1);
        let completion = observed[0].completion.clone();
        let pid = observed[0].pid;
        apply(&fixture.host.inner, observed, over_budget(pid));
        assert_eq!(fixture.host.resource_usage().evictions, 1);
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("process-tree memory budget"), "{error}");
        tokio::time::timeout(Duration::from_secs(5), completion.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .host
                .inner
                .entries
                .lock()
                .unwrap()
                .values()
                .all(|entry| entry.starting.lock().unwrap().is_none())
        );
    }
}
