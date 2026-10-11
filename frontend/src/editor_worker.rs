//! Thin browser worker adapter. Preparation policy and wire validation live in core.
use openwebide_core::editor::{
    SYNTAX_BATCH_MS, SYNTAX_INPUT_BUDGET_MS, SyntaxFrameProgress, SyntaxReply,
    SyntaxReplyFrameDecoding, SyntaxReplyFrames, SyntaxWorker,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};

const READY_MESSAGE: &str = "openwebide-editor-ready:6:frames:1";

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function editor_worker_bundle_url() {
    for (const link of document.querySelectorAll('link[rel="modulepreload"][href]')) {
        const url = new URL(link.href, document.baseURI);
        if (/\/openwebide-frontend-[a-f0-9]+\.js$/.test(url.pathname)) return url.href;
    }
    return null;
}
export function editor_worker_entry_url(bundle) {
    const match = bundle && new URL(bundle).pathname.match(/\/openwebide-frontend-([a-f0-9]+)\.js$/);
    return match ? `/editor-worker-${match[1]}.js` : '/editor-worker.js';
}
export function send_editor_module(worker, module, bundle_url) {
    worker.postMessage({kind:'openwebide-editor-module:1', module, bundle_url});
}
"#)]
extern "C" {
    fn editor_worker_bundle_url() -> wasm_bindgen::JsValue;
    fn editor_worker_entry_url(bundle: Option<&str>) -> String;
    #[wasm_bindgen(catch)]
    fn send_editor_module(
        worker: &web_sys::Worker,
        module: &wasm_bindgen::JsValue,
        bundle: &str,
    ) -> Result<(), wasm_bindgen::JsValue>;
}

thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; }
pub fn enable() {
    ENABLED.set(true);
}
pub fn enabled() -> bool {
    ENABLED.get()
}

/// Called by the same WASM entry point in a dedicated module worker.
pub fn mount_worker() -> bool {
    let Ok(scope) = js_sys::global().dyn_into::<web_sys::DedicatedWorkerGlobalScope>() else {
        return false;
    };
    let service = Rc::new(RefCell::new(SyntaxWorker::default()));
    let running = Rc::new(Cell::new(false));
    let controls = Rc::new(RefCell::new(
        std::collections::VecDeque::<SyntaxReplyFrames>::new(),
    ));
    let send = scope.clone();
    let callback =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(message) = event.data().as_string() else {
                return;
            };
            if let Some(reply) = service.borrow_mut().enqueue(&message)
                && let Some(frames) = SyntaxReply::decode(&reply).and_then(SyntaxReplyFrames::new)
            {
                controls.borrow_mut().push_back(frames);
            }
            if (!service.borrow().has_work() && controls.borrow().is_empty())
                || running.replace(true)
            {
                return;
            }
            let controls = controls.clone();
            let service = service.clone();
            let running = running.clone();
            let send = send.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let mut deadline = js_sys::Date::now() + f64::from(SYNTAX_BATCH_MS);
                loop {
                    let progress = if let Some(frames) = controls.borrow_mut().front_mut() {
                        frames.advance()
                    } else {
                        service
                            .borrow_mut()
                            .advance_frame(|| true, || js_sys::Date::now() >= deadline)
                    };
                    match progress {
                        Some(SyntaxFrameProgress::Frame(frame)) => {
                            let _ = send.post_message(&frame.into());
                        }
                        Some(SyntaxFrameProgress::Complete) => {
                            controls.borrow_mut().pop_front();
                        }
                        Some(SyntaxFrameProgress::Pending) | None => {}
                    }
                    if !service.borrow().has_work() && controls.borrow().is_empty() {
                        running.set(false);
                        break;
                    }
                    if js_sys::Date::now() >= deadline {
                        crate::util::yield_task().await;
                        deadline = js_sys::Date::now() + f64::from(SYNTAX_BATCH_MS);
                    }
                }
            });
        });
    scope.set_onmessage(Some(callback.as_ref().unchecked_ref()));
    let _ = scope.post_message(&READY_MESSAGE.into());
    callback.forget(); // Owned for the worker's lifetime; termination releases its WASM instance.
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Unavailable,
    Transport,
    Timeout,
}
struct PendingReply {
    sender: futures::channel::oneshot::Sender<Result<SyntaxReply, WorkerError>>,
    decoding: SyntaxReplyFrameDecoding,
    owner: Rc<()>,
}
type Pending = Rc<RefCell<HashMap<u32, PendingReply>>>;

struct PendingRequest {
    pending: Pending,
    ticket: u32,
    owner: Rc<()>,
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        let mut pending = self.pending.borrow_mut();
        if pending
            .get(&self.ticket)
            .is_some_and(|reply| Rc::ptr_eq(&reply.owner, &self.owner))
        {
            pending.remove(&self.ticket);
        }
    }
}

pub trait SyntaxTransport {
    fn request(
        &self,
        ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<String, WorkerError>>;
    fn request_reply(
        &self,
        ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<SyntaxReply, WorkerError>>;
    fn stop(&self);
}

/// Browser task adapter for the same Rust service used in a dedicated worker.
/// A command owns this request; dropping its future releases the service/job.
#[derive(Default)]
pub struct CooperativeClient {
    stopped: Cell<bool>,
}
impl CooperativeClient {
    /// In-process commands retain their immutable source and analysis. The same
    /// core facade owns preparation and retention; this adapter only yields tasks.
    pub async fn analyze_shared_while(
        &self,
        language: openwebide_core::highlight::Language,
        source: std::sync::Arc<String>,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
    ) -> Result<
        (
            openwebide_core::editor::SyntaxStatus,
            Option<std::sync::Arc<openwebide_core::editor::SyntaxAnalysis>>,
        ),
        WorkerError,
    > {
        self.analyze_shared_while_with_clock(
            language,
            source,
            tab_width,
            should_continue,
            js_sys::Date::now,
        )
        .await
    }

    pub(crate) async fn analyze_shared_while_with_clock(
        &self,
        language: openwebide_core::highlight::Language,
        source: std::sync::Arc<String>,
        tab_width: usize,
        mut should_continue: impl FnMut() -> bool,
        mut clock: impl FnMut() -> f64,
    ) -> Result<
        (
            openwebide_core::editor::SyntaxStatus,
            Option<std::sync::Arc<openwebide_core::editor::SyntaxAnalysis>>,
        ),
        WorkerError,
    > {
        let mut service = openwebide_core::editor::SyntaxPreparations::default();
        loop {
            if self.stopped.get() {
                return Err(WorkerError::Unavailable);
            }
            let deadline = clock() + f64::from(SYNTAX_INPUT_BUDGET_MS);
            if let Some(result) = service.prepare_cooperative(
                (),
                language,
                source.clone(),
                tab_width,
                || !self.stopped.get() && should_continue(),
                || clock() >= deadline,
            ) {
                return if self.stopped.get() {
                    Err(WorkerError::Unavailable)
                } else {
                    Ok(result)
                };
            }
            crate::util::yield_task().await;
        }
    }

    async fn request_reply_while(
        &self,
        message: String,
        mut should_continue: impl FnMut() -> bool,
    ) -> Result<SyntaxReply, WorkerError> {
        let mut service = SyntaxWorker::default();
        if self.stopped.get() {
            return Err(WorkerError::Unavailable);
        }
        if let Some(reply) = service.enqueue(&message) {
            return SyntaxReply::decode(&reply).ok_or(WorkerError::Transport);
        }
        while service.has_work() {
            if self.stopped.get() {
                return Err(WorkerError::Unavailable);
            }
            let deadline = js_sys::Date::now() + f64::from(SYNTAX_INPUT_BUDGET_MS);
            if let Some(reply) = service.advance_reply(
                || !self.stopped.get() && should_continue(),
                || js_sys::Date::now() >= deadline,
            ) {
                return if self.stopped.get() {
                    Err(WorkerError::Unavailable)
                } else {
                    Ok(reply)
                };
            }
            crate::util::yield_task().await;
        }
        Err(WorkerError::Transport)
    }

    pub async fn request_while(
        &self,
        message: String,
        should_continue: impl FnMut() -> bool,
    ) -> Result<String, WorkerError> {
        self.request_while_with_clock(message, should_continue, js_sys::Date::now)
            .await
    }

    pub(crate) async fn request_while_with_clock(
        &self,
        message: String,
        mut should_continue: impl FnMut() -> bool,
        mut clock: impl FnMut() -> f64,
    ) -> Result<String, WorkerError> {
        let mut service = SyntaxWorker::default();
        if self.stopped.get() {
            return Err(WorkerError::Unavailable);
        }
        if let Some(reply) = service.enqueue(&message) {
            return Ok(reply);
        }
        while service.has_work() {
            if self.stopped.get() {
                return Err(WorkerError::Unavailable);
            }
            let deadline = clock() + f64::from(SYNTAX_INPUT_BUDGET_MS);
            if let Some(reply) = service.advance(
                || !self.stopped.get() && should_continue(),
                || clock() >= deadline,
            ) {
                if self.stopped.get() {
                    return Err(WorkerError::Unavailable);
                }
                return Ok(reply);
            }
            crate::util::yield_task().await;
        }
        Err(WorkerError::Transport)
    }
}
impl SyntaxTransport for CooperativeClient {
    fn request(
        &self,
        _ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<String, WorkerError>> {
        Box::pin(self.request_while(message, || true))
    }
    fn request_reply(
        &self,
        _ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<SyntaxReply, WorkerError>> {
        Box::pin(self.request_reply_while(message, || true))
    }
    fn stop(&self) {
        self.stopped.set(true);
    }
}

type ReadyWaiter = Rc<RefCell<Option<futures::channel::oneshot::Sender<Result<(), WorkerError>>>>>;

pub struct WorkerClient {
    worker: web_sys::Worker,
    pending: Pending,
    failed: Rc<Cell<bool>>,
    ready: Rc<Cell<bool>>,
    ready_waiter: ReadyWaiter,
    message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    error: Closure<dyn FnMut(web_sys::Event)>,
}
impl WorkerClient {
    pub fn new() -> Result<Self, WorkerError> {
        Self::new_for_bundle(editor_worker_bundle_url().as_string())
    }

    fn new_for_bundle(bundle: Option<String>) -> Result<Self, WorkerError> {
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        let worker = web_sys::Worker::new_with_options(
            &editor_worker_entry_url(bundle.as_deref()),
            &options,
        )
        .map_err(|_| WorkerError::Unavailable)?;
        let pending: Pending = Rc::new(RefCell::new(HashMap::new()));
        let failed = Rc::new(Cell::new(false));
        let ready = Rc::new(Cell::new(false));
        let ready_waiter = Rc::new(RefCell::new(
            None::<futures::channel::oneshot::Sender<Result<(), WorkerError>>>,
        ));
        let ready_message = ready.clone();
        let wake = ready_waiter.clone();
        let messages = pending.clone();
        let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                if text == READY_MESSAGE {
                    ready_message.set(true);
                    if let Some(sender) = wake.borrow_mut().take() {
                        let _ = sender.send(Ok(()));
                    }
                    return;
                }
                let Some(ticket) = SyntaxReply::routing_ticket(&text) else {
                    return;
                };
                let mut pending = messages.borrow_mut();
                let Some(reply) = pending.get_mut(&ticket) else {
                    return;
                };
                match reply.decoding.push(&text) {
                    Some(false) => {}
                    complete => {
                        let reply = pending.remove(&ticket).expect("routed pending reply");
                        let result = if complete == Some(true) {
                            reply.decoding.finish().ok_or(WorkerError::Transport)
                        } else {
                            Err(WorkerError::Transport)
                        };
                        let _ = reply.sender.send(result);
                    }
                }
            },
        );
        let failures = pending.clone();
        let closed = failed.clone();
        let wake = ready_waiter.clone();
        let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            closed.set(true);
            if let Some(sender) = wake.borrow_mut().take() {
                let _ = sender.send(Err(WorkerError::Transport));
            }
            for (_, reply) in failures.borrow_mut().drain() {
                let _ = reply.sender.send(Err(WorkerError::Transport));
            }
        });
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        worker.set_onmessageerror(Some(error.as_ref().unchecked_ref()));
        if let Some(bundle) = bundle {
            // Cloning is optional: the bootstrap uses its URL when this fails.
            let _ = send_editor_module(&worker, &wasm_bindgen::module(), &bundle);
        }
        Ok(Self {
            worker,
            pending,
            failed,
            ready,
            ready_waiter,
            message,
            error,
        })
    }

    pub fn stop(&self) {
        self.failed.set(true);
        if let Some(sender) = self.ready_waiter.borrow_mut().take() {
            let _ = sender.send(Err(WorkerError::Unavailable));
        }
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.set_onmessageerror(None);
        self.worker.terminate();
        for (_, reply) in self.pending.borrow_mut().drain() {
            let _ = reply.sender.send(Err(WorkerError::Unavailable));
        }
    }

    pub async fn request(&self, ticket: u32, message: String) -> Result<String, WorkerError> {
        let reply = self.request_reply(ticket, message).await?;
        serde_json::to_string(&reply).map_err(|_| WorkerError::Transport)
    }

    pub async fn request_reply(
        &self,
        ticket: u32,
        message: String,
    ) -> Result<SyntaxReply, WorkerError> {
        if self.failed.get() {
            return Err(WorkerError::Transport);
        }
        if !self.ready.get() {
            let (sender, receiver) = futures::channel::oneshot::channel();
            *self.ready_waiter.borrow_mut() = Some(sender);
            match futures::future::select(
                Box::pin(receiver),
                Box::pin(crate::util::sleep_ms(10_000)),
            )
            .await
            {
                futures::future::Either::Left((Ok(Ok(())), _)) => {}
                futures::future::Either::Left(_) => return Err(WorkerError::Transport),
                futures::future::Either::Right(_) => return Err(WorkerError::Timeout),
            }
        }
        let (sender, receiver) = futures::channel::oneshot::channel();
        let owner = Rc::new(());
        let _request = PendingRequest {
            pending: self.pending.clone(),
            ticket,
            owner: owner.clone(),
        };
        self.pending.borrow_mut().insert(
            ticket,
            PendingReply {
                sender,
                decoding: SyntaxReplyFrameDecoding::new(ticket),
                owner,
            },
        );
        if self.worker.post_message(&message.into()).is_err() {
            return Err(WorkerError::Transport);
        }
        let received =
            futures::future::select(Box::pin(receiver), Box::pin(crate::util::sleep_ms(10_000)))
                .await;
        match received {
            futures::future::Either::Left((Ok(result), _)) => result,
            futures::future::Either::Left((Err(_), _)) => Err(WorkerError::Transport),
            futures::future::Either::Right(_) => Err(WorkerError::Timeout),
        }
    }
}
impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.stop();
        // Keep callback captures alive through listener removal and termination.
        let _ = (&self.message, &self.error);
    }
}

impl SyntaxTransport for WorkerClient {
    fn request(
        &self,
        ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<String, WorkerError>> {
        Box::pin(WorkerClient::request(self, ticket, message))
    }
    fn request_reply(
        &self,
        ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<SyntaxReply, WorkerError>> {
        Box::pin(WorkerClient::request_reply(self, ticket, message))
    }
    fn stop(&self) {
        WorkerClient::stop(self);
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use openwebide_core::editor::{SyntaxRequest, SyntaxStatus};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
    export function installRoutingWorker(ready) {
        const original = globalThis.Worker;
        globalThis.Worker = class {
            constructor() {
                queueMicrotask(() => this.onmessage?.({data:ready}));
            }
            postMessage(message) {
                const request = JSON.parse(message);
                const begin = {version:request.version,framing:1,ticket:request.ticket,sequence:0,part:{Begin:{status:'Cancelled',analysis:null}}};
                const end = {version:request.version,framing:1,ticket:request.ticket,sequence:1,part:'End'};
                if (request.ticket === 7) begin.padding = 'x'.repeat(2 * 1024 * 1024);
                if (request.ticket === 9) end.sequence = 2;
                if (request.ticket === 10) end.part = {Source:'unexpected'};
                queueMicrotask(() => {
                    this.onmessage?.({data:JSON.stringify(begin)});
                    if (request.ticket !== 11) this.onmessage?.({data:JSON.stringify(end)});
                });
            }
            terminate() {}
        };
        return () => { globalThis.Worker = original; };
    }
    "#)]
    extern "C" {
        fn installRoutingWorker(ready: &str) -> js_sys::Function;
    }

    #[wasm_bindgen::prelude::wasm_bindgen(js_name = mountSyntaxWorkerContract)]
    pub fn mount_syntax_worker_contract() -> bool {
        mount_worker()
    }

    #[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
    export function installRealSyntaxWorker(template, rejectModule) {
        const original = globalThis.Worker;
        const module = performance.getEntriesByType('resource').map(entry => entry.name)
            .find(url => /\/(?:tests\.js|wasm-bindgen-test(?:\.js)?)$/.test(new URL(url).pathname));
        if (!module) throw new Error('Missing generated WASM test module');
        const wasm = new URL(module);
        wasm.pathname = wasm.pathname.replace(/(?:\.js)?$/, '_bg.wasm');
        wasm.search = ''; wasm.hash = '';
        const source = template.replaceAll('__EDITOR_MODULE__', module).replaceAll('__EDITOR_WASM__', wasm.href) +
            `\nconst {mountSyntaxWorkerContract} = await import(bundle.href);
            if (!mountSyntaxWorkerContract()) throw new Error('Not in a dedicated worker');`;
        const url = URL.createObjectURL(new Blob([source], {type:'text/javascript'}));
        globalThis.Worker = class extends original {
            constructor(_url, options) {
                super(url, options);
                this.addEventListener('error', event => console.error('Syntax fixture worker error:', event.message));
            }
            postMessage(value, ...args) {
                if (rejectModule && value?.kind === 'openwebide-editor-module:1') {
                    throw new DOMException('Fixture cannot clone a Module', 'DataCloneError');
                }
                super.postMessage(value, ...args);
            }
        };
        const restore = () => { globalThis.Worker = original; URL.revokeObjectURL(url); };
        restore.module_url = module;
        return restore;
    }
    "#)]
    extern "C" {
        fn installRealSyntaxWorker(template: &str, reject_module: bool) -> js_sys::Function;
    }

    #[wasm_bindgen_test]
    async fn dedicated_framed_worker_and_cooperative_adapter_preserve_language_contracts() {
        use openwebide_core::highlight::Language;
        use std::sync::Arc;
        let restore = installRealSyntaxWorker(include_str!("../pwa/editor-worker.js"), false);
        let bundle = js_sys::Reflect::get(&restore, &"module_url".into())
            .unwrap()
            .as_string()
            .unwrap();
        let stage = Cell::new("worker creation");
        let result = async {
            let mut contracts = Vec::new();
            for bundle in [None, Some(bundle)] {
            let worker = WorkerClient::new_for_bundle(bundle)?;
            let cooperative = CooperativeClient::default();
            for (language, path, body) in [
                (
                    Language::Rust,
                    "worker.rs",
                    "fn sample() { let word = \"文😀\"; }\n",
                ),
                (
                    Language::Html,
                    "worker.html",
                    "<script>const word = '文😀';</script><style>.word { color: red; }</style>\n",
                ),
                (
                    Language::Markdown,
                    "worker.md",
                    "# 文😀\n```rust\nfn sample() {}\n```\n",
                ),
            ] {
                let mut previous = None;
                for (ticket, source) in [
                    (1, body.repeat(100)),
                    (2, format!("{}\n", body.repeat(100))),
                ] {
                    let request = SyntaxRequest::new(
                        ticket,
                        path.into(),
                        language,
                        &source,
                        4,
                        previous.as_ref().map(
                            |(ticket, analysis): &(
                                u32,
                                Arc<openwebide_core::editor::SyntaxAnalysis>,
                            )| (*ticket, analysis.as_ref()),
                        ),
                    );
                    let message = serde_json::to_string(&request).unwrap();
                    stage.set("dedicated worker request");
                    let actual = worker.request_reply(ticket, message.clone()).await?;
                    stage.set("cooperative request");
                    let mut fallback = cooperative.request_reply(ticket, message).await?;
                    if fallback.status == SyntaxStatus::NeedsSource {
                        let full =
                            SyntaxRequest::new(ticket, path.into(), language, &source, 4, None);
                        fallback = cooperative
                            .request_reply(ticket, serde_json::to_string(&full).unwrap())
                            .await?;
                    }
                    let source = Arc::new(source);
                    stage.set("dedicated worker proof setup");
                    let mut proof = actual
                        .prepare_received_shared(
                            ticket,
                            source.clone(),
                            previous
                                .as_ref()
                                .map(|(ticket, analysis)| (*ticket, analysis.as_ref())),
                        )
                        .ok_or(WorkerError::Transport)?;
                    while !proof.advance() {
                        crate::util::yield_task().await;
                    }
                    stage.set("dedicated worker proof finish");
                    let actual = proof
                        .finish()
                        .ok_or(WorkerError::Transport)?
                        .1
                        .ok_or(WorkerError::Transport)?;
                    stage.set("cooperative proof setup");
                    let mut proof = fallback
                        .prepare_received_shared(ticket, source, None)
                        .ok_or(WorkerError::Transport)?;
                    while !proof.advance() {
                        crate::util::yield_task().await;
                    }
                    stage.set("cooperative proof finish");
                    let fallback = proof
                        .finish()
                        .ok_or(WorkerError::Transport)?
                        .1
                        .ok_or(WorkerError::Transport)?;
                    contracts.push(
                        serde_json::to_value(actual.transfer_data()).unwrap()
                            == serde_json::to_value(fallback.transfer_data()).unwrap(),
                    );
                    previous = Some((ticket, actual));
                }
            }
            worker.stop();
            cooperative.stop();
            }
            Ok::<_, WorkerError>(contracts)
        }
        .await;
        restore.call0(&wasm_bindgen::JsValue::NULL).unwrap();
        let contracts = result.unwrap_or_else(|error| panic!("{}: {error:?}", stage.get()));
        assert_eq!(contracts.len(), 12);
        assert!(contracts.into_iter().all(|same| same));
    }

    #[wasm_bindgen_test]
    async fn module_clone_failures_and_bundle_mismatches_use_url_initialization() {
        for reject_module in [false, true] {
            let restore =
                installRealSyntaxWorker(include_str!("../pwa/editor-worker.js"), reject_module);
            let mut bundle = js_sys::Reflect::get(&restore, &"module_url".into())
                .unwrap()
                .as_string()
                .unwrap();
            if !reject_module {
                bundle.push_str("?different-bundle");
            }
            let result = async {
                let worker = WorkerClient::new_for_bundle(Some(bundle))?;
                let source =
                    std::sync::Arc::new("fn sample() { let word = \"文😀\"; }\n".to_string());
                let request = SyntaxRequest::new(
                    1,
                    "fallback.rs".into(),
                    openwebide_core::highlight::Language::Rust,
                    &source,
                    4,
                    None,
                );
                let reply = worker
                    .request_reply(1, serde_json::to_string(&request).unwrap())
                    .await?;
                let mut proof = reply
                    .prepare_received_shared(1, source.clone(), None)
                    .ok_or(WorkerError::Transport)?;
                while !proof.advance() {
                    crate::util::yield_task().await;
                }
                let analysis = proof
                    .finish()
                    .ok_or(WorkerError::Transport)?
                    .1
                    .ok_or(WorkerError::Transport)?;
                worker.stop();
                assert_eq!(analysis.source(), source.as_str());
                Ok::<_, WorkerError>(())
            }
            .await;
            restore.call0(&wasm_bindgen::JsValue::NULL).unwrap();
            result.unwrap();
        }
    }

    #[wasm_bindgen_test]
    async fn dropped_framed_requests_release_partial_transactions() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let restore = installRoutingWorker(READY_MESSAGE);
        let result = async {
            let client = WorkerClient::new()?;
            client.ready.set(true);
            let request = SyntaxRequest::new(
                11,
                "owner.rs".into(),
                openwebide_core::highlight::Language::Rust,
                "fn main() {}",
                4,
                None,
            );
            let mut future =
                Box::pin(client.request_reply(11, serde_json::to_string(&request).unwrap()));
            let started = matches!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            );
            crate::util::yield_task().await;
            let retained = client.pending.borrow().len();
            drop(future);
            let released = client.pending.borrow().is_empty();
            client.stop();
            Ok::<_, WorkerError>((started, retained, released))
        }
        .await;
        restore.call0(&wasm_bindgen::JsValue::NULL).unwrap();
        let (started, retained, released) = result.unwrap();
        assert!(started);
        assert_eq!(retained, 1);
        assert!(released);
    }

    #[wasm_bindgen_test]
    async fn bounded_worker_routing_preserves_complete_reply_validation() {
        let restore = installRoutingWorker(READY_MESSAGE);
        let result = async {
            let client = WorkerClient::new()?;
            let mut replies = Vec::new();
            for ticket in [7, 8, 9, 10] {
                let request = SyntaxRequest::new(
                    ticket,
                    "worker.rs".into(),
                    openwebide_core::highlight::Language::Rust,
                    "let value = 1;",
                    4,
                    None,
                );
                replies.push(
                    client
                        .request_reply(ticket, serde_json::to_string(&request).unwrap())
                        .await,
                );
            }
            client.stop();
            Ok::<_, WorkerError>(replies)
        }
        .await;
        // Restore the global primitive before assertions, including transport
        // failures: WASM panics do not unwind test fixture cleanup.
        restore.call0(&wasm_bindgen::JsValue::NULL).unwrap();
        let replies = result.unwrap();
        assert!(matches!(replies[0], Err(WorkerError::Transport)));
        let reply = replies[1].as_ref().unwrap();
        assert_eq!(reply.status, SyntaxStatus::Cancelled);
        assert!(reply.analysis.is_none());
        assert!(matches!(replies[2], Err(WorkerError::Transport)));
        assert!(matches!(replies[3], Err(WorkerError::Transport)));
    }
}
