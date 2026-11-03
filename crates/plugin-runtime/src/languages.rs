//! Bounded, owner-isolated component sessions for the public language service.
//! Syntax is pure computation: imports have no host grants or filesystem access.
use super::{NoServices, Plugin, Runtime, State};
use anyhow::{Result, bail};
use openwebide_core::plugins::{PreparedPlugin, languages::*};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use wasmtime::{Store, UpdateDeadline, component::Component};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SESSIONS: usize = 8;
const MAX_OWNER_SESSIONS: usize = 4;
const MAX_COMPONENTS: usize = 4;
const MAX_COMPONENT_BYTES: usize = 64 * 1024 * 1024;
const MEMORY_BYTES: usize = 256 * 1024 * 1024;
// Includes decoding an admitted 2 MiB source with worst-case JSON escapes.
// Epoch interruption bounds wall time and makes CPU loops cancellable.
const FUEL: u64 = 2_000_000_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SessionKey {
    owner: String,
    namespace: String,
    digest: String,
    document: String,
}
struct Session {
    store: Store<State<NoServices>>,
    plugin: Plugin,
    document: Option<LanguageDocument>,
    used: Instant,
}
struct Compiled {
    component: Component,
    bytes: usize,
    used: Instant,
}

pub struct LanguageRuntime {
    runtime: Runtime,
    components: BTreeMap<String, Compiled>,
    sessions: BTreeMap<SessionKey, Session>,
}
impl std::fmt::Debug for LanguageRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LanguageRuntime")
            .field("sessions", &self.sessions.len())
            .field("components", &self.components.len())
            .finish_non_exhaustive()
    }
}
impl LanguageRuntime {
    pub fn new() -> Result<Self> {
        Ok(Self {
            runtime: Runtime::new()?,
            components: BTreeMap::new(),
            sessions: BTreeMap::new(),
        })
    }

    /// Called by the host's timer, so idle components are reclaimed even without
    /// another request. Cache loss uses the protocol's NeedsSource response.
    pub fn expire_idle(&mut self) {
        self.sessions
            .retain(|_, session| session.used.elapsed() < IDLE_TIMEOUT);
        self.components
            .retain(|_, component| component.used.elapsed() < IDLE_TIMEOUT);
    }

    /// Reuse a pinned owned component without rereading/hashing its immutable
    /// artifact for every edit. Cold documents use normal artifact loading.
    pub fn execute_retained(
        &mut self,
        owner: &str,
        prepared: &PreparedPlugin,
        input: &LanguageInput,
        cancelled: Arc<AtomicBool>,
    ) -> Option<Result<LanguageOutput>> {
        if let Err(error) = validate_input(input) {
            return Some(Err(anyhow::Error::msg(error)));
        }
        self.expire_idle();
        if let LanguageInput::Analyze(request) = input {
            let key = SessionKey {
                owner: owner.into(),
                namespace: prepared.storage_namespace(),
                digest: prepared.digest.clone(),
                document: request.document.clone(),
            };
            self.sessions.get_mut(&key)?.used = Instant::now();
        }
        Some(self.execute(owner, prepared, &[], input, cancelled))
    }

    pub fn execute(
        &mut self,
        owner: &str,
        prepared: &PreparedPlugin,
        bytes: &[u8],
        input: &LanguageInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<LanguageOutput> {
        validate_input(input).map_err(anyhow::Error::msg)?;
        if cancelled.load(Ordering::Relaxed) {
            bail!("Language invocation was cancelled");
        }
        prepared.manifest.validate().map_err(anyhow::Error::msg)?;
        let document = match input {
            LanguageInput::Analyze(request) => {
                let declared = prepared
                    .manifest
                    .contributions
                    .languages
                    .iter()
                    .any(|definition| {
                        definition.id == request.language
                            && (definition.features.syntax
                                || definition.features.folding
                                || definition.features.structure)
                    });
                if !declared {
                    bail!("Language analysis is not declared by this plugin");
                }
                &request.document
            }
            LanguageInput::Close { document, .. } => document,
        };
        let key = SessionKey {
            owner: owner.into(),
            namespace: prepared.storage_namespace(),
            digest: prepared.digest.clone(),
            document: document.clone(),
        };
        self.expire_idle();
        if let LanguageInput::Close { version, document } = input {
            // Dropping the entire owned store also drops a provider's cached
            // trees/source, even when it fails to implement its close callback.
            self.sessions.remove(&key);
            return Ok(LanguageOutput::Closed {
                version: *version,
                document: document.clone(),
            });
        }
        let LanguageInput::Analyze(request) = input else {
            unreachable!()
        };
        let previous = self
            .sessions
            .get(&key)
            .and_then(|session| session.document.as_ref());
        let Some(document) =
            LanguageDocument::apply(previous, request).map_err(anyhow::Error::msg)?
        else {
            return Ok(analysis_response(request, AnalysisStatus::NeedsSource));
        };
        let mut session = if let Some(session) = self.sessions.remove(&key) {
            session
        } else {
            self.admit(&key);
            let component = self.compile(bytes)?;
            let (store, plugin) =
                self.runtime
                    .instantiate_component(&component, NoServices, &[], MEMORY_BYTES)?;
            Session {
                store,
                plugin,
                document: None,
                used: Instant::now(),
            }
        };
        // Drop failed/cancelled/trapped sessions. Never reuse a partially mutated
        // provider or publish its document as the base of a later delta.
        let output = self.call(&mut session, input, cancelled)?;
        validate_output(input, &output, Some(&document.source)).map_err(anyhow::Error::msg)?;
        if let LanguageOutput::Analyzed(response) = &output {
            session.document =
                matches!(response.status, AnalysisStatus::Ready { .. }).then_some(document);
        }
        session.used = Instant::now();
        self.sessions.insert(key, session);
        Ok(output)
    }

    fn admit(&mut self, key: &SessionKey) {
        // A document cannot keep stale versions alive when its provider changes.
        self.sessions.retain(|old, _| {
            !(old.owner == key.owner
                && old.document == key.document
                && old.namespace == key.namespace
                && old.digest != key.digest)
        });
        if self
            .sessions
            .keys()
            .filter(|old| old.owner == key.owner)
            .count()
            >= MAX_OWNER_SESSIONS
        {
            let oldest = self
                .sessions
                .iter()
                .filter(|(old, _)| old.owner == key.owner)
                .min_by_key(|(_, session)| session.used)
                .map(|(old, _)| old.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }
        if self.sessions.len() >= MAX_SESSIONS {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.used)
                .map(|(old, _)| old.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }
    }

    fn compile(&mut self, bytes: &[u8]) -> Result<Component> {
        if bytes.len() > 32 * 1024 * 1024 {
            bail!("Language component exceeds its byte limit");
        }
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if let Some(entry) = self.components.get_mut(&digest) {
            entry.used = Instant::now();
            return Ok(entry.component.clone());
        }
        while self.components.len() >= MAX_COMPONENTS
            || self
                .components
                .values()
                .map(|entry| entry.bytes)
                .sum::<usize>()
                + bytes.len()
                > MAX_COMPONENT_BYTES
        {
            let oldest = self
                .components
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.components.remove(&oldest);
            } else {
                break;
            }
        }
        let component = Component::new(&self.runtime.engine, bytes)?;
        self.components.insert(
            digest,
            Compiled {
                component: component.clone(),
                bytes: bytes.len(),
                used: Instant::now(),
            },
        );
        Ok(component)
    }

    fn call(
        &self,
        session: &mut Session,
        input: &LanguageInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<LanguageOutput> {
        let arguments = serde_json::to_string(input)?;
        if arguments.len() > MAX_REQUEST_BYTES {
            bail!("Language input exceeds its byte limit");
        }
        let deadline = Instant::now() + CALL_TIMEOUT;
        let interrupted = cancelled.clone();
        session.store.set_fuel(FUEL)?;
        session.store.set_epoch_deadline(1);
        session.store.epoch_deadline_callback(move |_| {
            if interrupted.load(Ordering::Relaxed) || Instant::now() >= deadline {
                bail!("Language invocation was cancelled or timed out");
            }
            Ok(UpdateDeadline::Continue(1))
        });
        let _ticker = EpochTicker::new(self.runtime.engine.clone());
        let json = session
            .plugin
            .call_execute(&mut session.store, SERVICE_HANDLER, &arguments)?
            .map_err(anyhow::Error::msg)?;
        if cancelled.load(Ordering::Relaxed) {
            bail!("Language invocation was cancelled");
        }
        if json.len() > MAX_ENVELOPE_BYTES {
            bail!("Language outcome exceeds its byte limit");
        }
        let outcome: super::sdk::Outcome = serde_json::from_str(&json)?;
        if !outcome.ok {
            bail!("Language provider failed: {}", outcome.summary);
        }
        if outcome.content.len() > MAX_MESSAGE_BYTES {
            bail!("Language analysis exceeds its byte limit");
        }
        Ok(serde_json::from_str(&outcome.content)?)
    }
}

struct EpochTicker {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl EpochTicker {
    fn new(engine: wasmtime::Engine) -> Self {
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(Duration::from_millis(10)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                engine.increment_epoch();
            }
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}
impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepared() -> PreparedPlugin {
        serde_json::from_value(json!({
            "source":{"repository":"https://example.test/plugins.git","commit":"a".repeat(40),"path":"plugins/language"},
            "manifest":{
                "schemaVersion":1,"publisher":"example","name":"language","version":"1.0.0",
                "displayName":"Language fixture","description":"Real component language contract","license":"MIT",
                "compatibility":{"pluginApi":5},
                "executable":{"manifest":"Cargo.toml","library":"plugin_fixture","sdkVersion":"0.1.0","capabilities":[]},
                "contributions":{"skills":[],"languages":[{"id":"fixture-language","name":"Fixture language","extensions":["fixture"],"features":{"syntax":true}}]}
            }, "digest":"b".repeat(64),"host_id":"host"
        })).unwrap()
    }
    fn input(text: &str, request: u64, revision: u64) -> LanguageInput {
        LanguageInput::Analyze(AnalysisRequest {
            version: VERSION,
            document: "client/project/document".into(),
            request,
            revision,
            language: "fixture-language".into(),
            path: "test.fixture".into(),
            source_digest: source_digest(text),
            source: SourceChange::Full { text: text.into() },
            tab_width: 4,
        })
    }
    fn delta(text: &str, base: u64, request: u64, revision: u64) -> LanguageInput {
        let LanguageInput::Analyze(mut input) = input(text, request, revision) else {
            unreachable!()
        };
        input.source = SourceChange::Replace {
            base_revision: base,
            start: 0,
            end: 0,
            text: "文".into(),
        };
        LanguageInput::Analyze(input)
    }
    fn ready(output: &LanguageOutput) {
        assert!(
            matches!(output, LanguageOutput::Analyzed(response) if matches!(response.status, AnalysisStatus::Ready { .. }))
        );
    }
    fn missing(output: &LanguageOutput) {
        assert!(
            matches!(output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource)
        );
    }
    #[test]
    fn real_components_retain_owned_sources_and_drop_closed_updated_or_evicted_sessions() {
        let bytes = crate::tests::fixture();
        let mut runtime = LanguageRuntime::new().unwrap();
        let mut plugin = prepared();
        let cancel = Arc::new(AtomicBool::new(false));
        let source = "task 😀\r\n";
        let first = input(source, 1, 1);
        ready(
            &runtime
                .execute("owner", &plugin, &bytes, &first, cancel.clone())
                .unwrap(),
        );
        let edit = delta(&format!("文{source}"), 1, 2, 2);
        missing(
            &runtime
                .execute("other-owner", &plugin, &bytes, &edit, cancel.clone())
                .unwrap(),
        );
        ready(
            &runtime
                .execute("owner", &plugin, &bytes, &edit, cancel.clone())
                .unwrap(),
        );
        assert!(
            runtime
                .execute("owner", &plugin, &bytes, &first, cancel.clone())
                .is_err()
        );
        plugin.digest = "c".repeat(64);
        missing(
            &runtime
                .execute(
                    "owner",
                    &plugin,
                    &bytes,
                    &delta("文文", 2, 3, 3),
                    cancel.clone(),
                )
                .unwrap(),
        );
        ready(
            &runtime
                .execute(
                    "owner",
                    &plugin,
                    &bytes,
                    &input(source, 3, 3),
                    cancel.clone(),
                )
                .unwrap(),
        );
        assert_eq!(runtime.sessions.len(), 1);
        let close = LanguageInput::Close {
            version: VERSION,
            document: "client/project/document".into(),
        };
        assert_eq!(
            runtime
                .execute("other-owner", &plugin, &bytes, &close, cancel.clone())
                .unwrap(),
            LanguageOutput::Closed {
                version: VERSION,
                document: "client/project/document".into()
            }
        );
        assert_eq!(runtime.sessions.len(), 1);
        runtime
            .execute("owner", &plugin, &bytes, &close, cancel.clone())
            .unwrap();
        assert!(runtime.sessions.is_empty());
        for index in 0..MAX_OWNER_SESSIONS + 1 {
            let LanguageInput::Analyze(mut request) = input(source, 1, 1) else {
                unreachable!()
            };
            request.document = format!("document-{index}");
            ready(
                &runtime
                    .execute(
                        "owner",
                        &plugin,
                        &bytes,
                        &LanguageInput::Analyze(request),
                        cancel.clone(),
                    )
                    .unwrap(),
            );
        }
        assert_eq!(runtime.sessions.len(), MAX_OWNER_SESSIONS);
        assert!(
            !runtime
                .sessions
                .keys()
                .any(|key| key.document == "document-0")
        );
        for session in runtime.sessions.values_mut() {
            session.used = Instant::now() - IDLE_TIMEOUT;
        }
        for component in runtime.components.values_mut() {
            component.used = Instant::now() - IDLE_TIMEOUT;
        }
        runtime.expire_idle();
        assert!(runtime.sessions.is_empty());
        assert!(runtime.components.is_empty());
    }
    #[test]
    fn invalid_and_trapped_results_are_discarded_and_cpu_work_is_cancellable() {
        let bytes = crate::tests::fixture();
        let mut runtime = LanguageRuntime::new().unwrap();
        let plugin = prepared();
        for (path, expected) in [
            ("invalid.fixture", "another document request"),
            ("trap.fixture", "unreachable"),
        ] {
            let LanguageInput::Analyze(mut request) = input("task", 1, 1) else {
                unreachable!()
            };
            request.path = path.into();
            let error = runtime
                .execute(
                    "owner",
                    &plugin,
                    &bytes,
                    &LanguageInput::Analyze(request),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert!(runtime.sessions.is_empty());
        }
        // Compile/instantiate before timing cancellation so a cancelled compile
        // cannot masquerade as an interrupted provider loop.
        ready(
            &runtime
                .execute(
                    "owner",
                    &plugin,
                    &bytes,
                    &input("task", 1, 1),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap(),
        );
        let LanguageInput::Analyze(mut request) = input("task", 2, 2) else {
            unreachable!()
        };
        request.path = "loop.fixture".into();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let cancellation = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let error = runtime
            .execute(
                "owner",
                &plugin,
                &bytes,
                &LanguageInput::Analyze(request),
                cancelled.clone(),
            )
            .unwrap_err();
        cancellation.join().unwrap();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(
            format!("{error:#}").contains("cancelled or timed out"),
            "{error:#}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(runtime.sessions.is_empty());
        ready(
            &runtime
                .execute(
                    "owner",
                    &plugin,
                    &bytes,
                    &input("recovered", 1, 1),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap(),
        );
    }
    #[test]
    fn full_source_limit_survives_worst_case_json_escaping() {
        let bytes = crate::tests::fixture();
        let mut runtime = LanguageRuntime::new().unwrap();
        let source = "\0".repeat(MAX_SOURCE_BYTES);
        ready(
            &runtime
                .execute(
                    "owner",
                    &prepared(),
                    &bytes,
                    &input(&source, 1, 1),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap(),
        );
    }

    #[test]
    fn retained_documents_need_no_artifact_reload_and_never_cross_an_owner_or_version() {
        let bytes = crate::tests::fixture();
        let mut runtime = LanguageRuntime::new().unwrap();
        let mut plugin = prepared();
        let cancelled = Arc::new(AtomicBool::new(false));
        let first = input("task", 1, 1);
        assert!(
            runtime
                .execute_retained("owner", &plugin, &first, cancelled.clone())
                .is_none()
        );
        ready(
            &runtime
                .execute("owner", &plugin, &bytes, &first, cancelled.clone())
                .unwrap(),
        );
        let second = delta("文task", 1, 2, 2);
        assert!(
            runtime
                .execute_retained("other", &plugin, &second, cancelled.clone())
                .is_none()
        );
        ready(
            &runtime
                .execute_retained("owner", &plugin, &second, cancelled.clone())
                .unwrap()
                .unwrap(),
        );
        plugin.digest = "c".repeat(64);
        assert!(
            runtime
                .execute_retained("owner", &plugin, &second, cancelled.clone())
                .is_none()
        );
        plugin.digest = "b".repeat(64);
        let close = LanguageInput::Close {
            version: VERSION,
            document: "client/project/document".into(),
        };
        runtime
            .execute_retained("owner", &plugin, &close, cancelled.clone())
            .unwrap()
            .unwrap();
        assert!(
            runtime
                .execute_retained("owner", &plugin, &second, cancelled)
                .is_none()
        );
    }
}
