//! Thin browser worker adapter. Preparation policy and wire validation live in core.
use openwebide_core::editor::{SYNTAX_BATCH_MS, SyntaxWorker};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};

const READY_MESSAGE: &str = "openwebide-editor-ready:6";

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
    let send = scope.clone();
    let callback =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(message) = event.data().as_string() else {
                return;
            };
            if let Some(reply) = service.borrow_mut().enqueue(&message) {
                let _ = send.post_message(&reply.into());
            }
            if !service.borrow().has_work() || running.replace(true) {
                return;
            }
            let service = service.clone();
            let running = running.clone();
            let send = send.clone();
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    let deadline = js_sys::Date::now() + f64::from(SYNTAX_BATCH_MS);
                    let reply = service
                        .borrow_mut()
                        .advance(|| true, || js_sys::Date::now() >= deadline);
                    if let Some(reply) = reply {
                        let _ = send.post_message(&reply.into());
                    }
                    if !service.borrow().has_work() {
                        running.set(false);
                        break;
                    }
                    crate::util::yield_task().await;
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
#[derive(serde::Deserialize)]
struct ReplyTicket {
    ticket: u32,
}
type Pending =
    Rc<RefCell<HashMap<u32, futures::channel::oneshot::Sender<Result<String, WorkerError>>>>>;

pub trait SyntaxTransport {
    fn request(
        &self,
        ticket: u32,
        message: String,
    ) -> futures::future::LocalBoxFuture<'_, Result<String, WorkerError>>;
    fn stop(&self);
}

/// Browser task adapter for the same Rust service used in a dedicated worker.
/// A command owns this request; dropping its future releases the service/job.
#[derive(Default)]
pub struct CooperativeClient {
    stopped: Cell<bool>,
}
impl CooperativeClient {
    pub async fn request_while(
        &self,
        message: String,
        mut should_continue: impl FnMut() -> bool,
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
            let deadline = js_sys::Date::now() + f64::from(SYNTAX_BATCH_MS);
            if let Some(reply) = service.advance(
                || !self.stopped.get() && should_continue(),
                || js_sys::Date::now() >= deadline,
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
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        let worker = web_sys::Worker::new_with_options("/editor-worker.js", &options)
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
                if text.len() > openwebide_core::editor::MAX_ANALYSIS_MESSAGE_BYTES {
                    return;
                }
                // Extract only the routing ticket; the shared facade validates the complete reply.
                let Ok(reply) = serde_json::from_str::<ReplyTicket>(&text) else {
                    return;
                };
                if let Some(sender) = messages.borrow_mut().remove(&reply.ticket) {
                    let _ = sender.send(Ok(text));
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
            for (_, sender) in failures.borrow_mut().drain() {
                let _ = sender.send(Err(WorkerError::Transport));
            }
        });
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        worker.set_onmessageerror(Some(error.as_ref().unchecked_ref()));
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
        for (_, sender) in self.pending.borrow_mut().drain() {
            let _ = sender.send(Err(WorkerError::Unavailable));
        }
    }

    pub async fn request(&self, ticket: u32, message: String) -> Result<String, WorkerError> {
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
        self.pending.borrow_mut().insert(ticket, sender);
        if self.worker.post_message(&message.into()).is_err() {
            self.pending.borrow_mut().remove(&ticket);
            return Err(WorkerError::Transport);
        }
        let received =
            futures::future::select(Box::pin(receiver), Box::pin(crate::util::sleep_ms(10_000)))
                .await;
        self.pending.borrow_mut().remove(&ticket);
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
    fn stop(&self) {
        WorkerClient::stop(self);
    }
}
