//! IndexedDB helpers for persisting File System Access API directory handles
//! so local-mode projects survive a page reload.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::{LocalBoxFuture, Shared};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemHandle, FileSystemHandlePermissionDescriptor,
    FileSystemPermissionMode, IdbDatabase, IdbObjectStore, IdbRequest, IdbTransaction,
    IdbTransactionMode,
};

use crate::local_fs::js_error;

const DB_NAME: &str = "openwebide";
const STORE: &str = "directories";

/// A future that resolves when an `IdbRequest` fires its success or error
/// event. It owns the event-handler closures (so they stay alive until the
/// request settles) and the request itself.
struct IdbRequestFuture {
    request: IdbRequest,
    rx: Option<oneshot::Receiver<Result<JsValue, String>>>,
    _closures: Vec<Closure<dyn FnMut()>>,
}

impl Drop for IdbRequestFuture {
    fn drop(&mut self) {
        self.request.set_onsuccess(None);
        self.request.set_onerror(None);
        if let Some(open) = self.request.dyn_ref::<web_sys::IdbOpenDbRequest>() {
            open.set_onupgradeneeded(None);
        }
    }
}

impl Future for IdbRequestFuture {
    type Output = Result<JsValue, String>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut() {
            Some(rx) => rx
                .poll_unpin(cx)
                .map(|r| r.map_err(|_| "IndexedDB request failed".to_string())?),
            None => Poll::Ready(Err("IndexedDB request already settled".to_string())),
        }
    }
}

/// Attach success/error handlers to `request`, resolving `tx` when it settles.
/// The closures are pushed onto `closures` so they outlive the request.
fn attach(
    request: &IdbRequest,
    closures: &mut Vec<Closure<dyn FnMut()>>,
    tx: oneshot::Sender<Result<JsValue, String>>,
    event: &str,
) {
    let event = event.to_string();
    // Only one of the success/error handlers fires, but both must be able to
    // send, so share the (non-cloneable) sender and let the first to fire take it.
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let success = Closure::<dyn FnMut()>::new({
        let req = request.clone();
        let tx = std::rc::Rc::clone(&tx);
        move || {
            let result = req.result().unwrap_or(JsValue::NULL);
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Ok(result));
            }
        }
    });
    let error = Closure::<dyn FnMut()>::new({
        let tx = std::rc::Rc::clone(&tx);
        move || {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Err(format!("IndexedDB {event} failed")));
            }
        }
    });
    request.set_onsuccess(Some(success.as_ref().unchecked_ref()));
    request.set_onerror(Some(error.as_ref().unchecked_ref()));
    closures.push(success);
    closures.push(error);
}

type PendingOpen = Shared<LocalBoxFuture<'static, Result<IdbDatabase, String>>>;

#[derive(Default)]
struct DatabaseCache {
    connection: Option<Connection>,
    pending: Option<PendingOpen>,
}

struct Connection {
    db: IdbDatabase,
    _handlers: Vec<Closure<dyn FnMut()>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.db.set_onversionchange(None);
        self.db.set_onclose(None);
        self.db.set_onerror(None);
        self.db.close();
    }
}

thread_local! {
    static CACHE: RefCell<DatabaseCache> = RefCell::default();
}

fn connection(db: IdbDatabase) -> Connection {
    let mut handlers = Vec::new();
    for event in ["versionchange", "close", "error"] {
        let database = db.clone();
        let handler = Closure::<dyn FnMut()>::new(move || {
            database.close();
            let database = database.clone();
            // Detach handlers after this callback returns, not while it is executing.
            wasm_bindgen_futures::spawn_local(async move {
                CACHE.with_borrow_mut(|cache| {
                    if cache.connection.as_ref().is_some_and(|c| c.db == database) {
                        cache.connection = None;
                    }
                });
            });
        });
        let callback = Some(handler.as_ref().unchecked_ref());
        match event {
            "versionchange" => db.set_onversionchange(callback),
            "close" => db.set_onclose(callback),
            _ => db.set_onerror(callback),
        }
        handlers.push(handler);
    }
    Connection {
        db,
        _handlers: handlers,
    }
}

/// Reuse the live connection and share concurrent opens in browser memory.
pub async fn open_db() -> Result<IdbDatabase, String> {
    if let Some(db) = CACHE.with_borrow(|cache| {
        cache
            .connection
            .as_ref()
            .map(|connection| connection.db.clone())
    }) {
        return Ok(db);
    }
    let pending = CACHE.with_borrow_mut(|cache| {
        if let Some(pending) = &cache.pending {
            return pending.clone();
        }
        let pending = async {
            let result = open_uncached().await;
            CACHE.with_borrow_mut(|cache| {
                cache.pending = None;
                if let Ok(db) = &result {
                    cache.connection = Some(connection(db.clone()));
                }
            });
            result
        }
        .boxed_local()
        .shared();
        cache.pending = Some(pending.clone());
        // An abandoned caller must not leave an unpolled open or event handlers behind.
        let driver = pending.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = driver.await;
        });
        pending
    });
    pending.await
}

#[cfg(feature = "test-support")]
pub fn reset_connection() {
    CACHE.with_borrow_mut(|cache| cache.connection = None);
}

/// Open the database, creating the `directories` store on first use.
async fn open_uncached() -> Result<IdbDatabase, String> {
    let idb = web_sys::window()
        .and_then(|w| w.indexed_db().ok().flatten())
        .ok_or_else(|| "IndexedDB is not available".to_string())?;
    let request = idb
        .open_with_u32(DB_NAME, 1)
        .map_err(|e| e.as_string().unwrap_or_else(|| "open failed".to_string()))?;
    let (tx, rx) = oneshot::channel();
    let mut closures = Vec::new();
    // The onupgradeneeded handler reads the freshly opened DB off the request,
    // which requires the base `IdbRequest` type.
    let upgrade_request: IdbRequest = request.clone().into();
    let onupgradeneeded = Closure::<dyn FnMut()>::new(move || {
        if let Ok(value) = upgrade_request.result()
            && let Some(db) = value.dyn_ref::<IdbDatabase>()
        {
            let _ = db.create_object_store(STORE);
        }
    });
    request.set_onupgradeneeded(Some(onupgradeneeded.as_ref().unchecked_ref()));
    closures.push(onupgradeneeded);
    // Convert to the base `IdbRequest` for the success/error handlers and storage.
    let request: IdbRequest = request.into();
    attach(&request, &mut closures, tx, "open");
    let result = IdbRequestFuture {
        request,
        rx: Some(rx),
        _closures: closures,
    }
    .await?;
    result
        .dyn_into::<IdbDatabase>()
        .map_err(|_| "IndexedDB open did not return a database".to_string())
}

struct TransactionFuture {
    transaction: IdbTransaction,
    rx: oneshot::Receiver<Result<(), String>>,
    _handlers: Vec<Closure<dyn FnMut()>>,
}

impl TransactionFuture {
    fn new(transaction: IdbTransaction) -> Self {
        let (tx, rx) = oneshot::channel();
        let tx = Rc::new(RefCell::new(Some(tx)));
        let mut handlers = Vec::new();
        for event in ["complete", "abort", "error"] {
            let tx = Rc::clone(&tx);
            let handler = Closure::<dyn FnMut()>::new(move || {
                if let Some(tx) = tx.borrow_mut().take() {
                    let result = if event == "complete" {
                        Ok(())
                    } else {
                        Err(format!("IndexedDB transaction {event}"))
                    };
                    let _ = tx.send(result);
                }
            });
            let callback = Some(handler.as_ref().unchecked_ref());
            match event {
                "complete" => transaction.set_oncomplete(callback),
                "abort" => transaction.set_onabort(callback),
                _ => transaction.set_onerror(callback),
            }
            handlers.push(handler);
        }
        Self {
            transaction,
            rx,
            _handlers: handlers,
        }
    }
}

impl Future for TransactionFuture {
    type Output = Result<(), String>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_unpin(cx).map(|result| {
            result.unwrap_or_else(|_| Err("IndexedDB transaction failed".to_string()))
        })
    }
}

impl Drop for TransactionFuture {
    fn drop(&mut self) {
        self.transaction.set_oncomplete(None);
        self.transaction.set_onabort(None);
        self.transaction.set_onerror(None);
    }
}

async fn read(request: IdbRequest) -> Result<JsValue, String> {
    let (tx, rx) = oneshot::channel();
    let mut closures = Vec::new();
    attach(&request, &mut closures, tx, "request");
    IdbRequestFuture {
        request,
        rx: Some(rx),
        _closures: closures,
    }
    .await
}

fn get_store(db: &IdbDatabase, mode: IdbTransactionMode) -> Result<IdbObjectStore, String> {
    let tx = db.transaction_with_str_and_mode(STORE, mode).map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| "transaction failed".to_string())
    })?;
    tx.object_store(STORE).map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| "object store failed".to_string())
    })
}

/// Build the IndexedDB key for a project. IndexedDB keys must be a number,
/// string, Date, or buffer — `JsValue::from(i64)` yields a BigInt, which
/// IndexedDB rejects with a DataError. Project ids are small, so an f64 number
/// is exact.
fn key(project_id: i64) -> JsValue {
    JsValue::from_f64(project_id as f64)
}

/// Persist a directory handle keyed by project id.
pub async fn save_handle(
    project_id: i64,
    user_id: Option<openwebide_core::UserId>,
    handle: &FileSystemDirectoryHandle,
) -> Result<(), String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readwrite)?;
    let completion = TransactionFuture::new(store.transaction());
    let record = js_sys::Object::new();
    js_sys::Reflect::set(&record, &JsValue::from_str("projectId"), &key(project_id))
        .map_err(|e| js_error(&e))?;
    js_sys::Reflect::set(&record, &JsValue::from_str("handle"), handle)
        .map_err(|e| js_error(&e))?;
    if let Some(user_id) = user_id {
        js_sys::Reflect::set(&record, &JsValue::from_str("userId"), &key(user_id.get()))
            .map_err(|e| js_error(&e))?;
    }
    let revision = format!(
        "{}-{}-{}",
        js_sys::Date::now(),
        js_sys::Math::random(),
        js_sys::Math::random()
    );
    js_sys::Reflect::set(
        &record,
        &JsValue::from_str("revision"),
        &JsValue::from_str(&revision),
    )
    .map_err(|e| js_error(&e))?;
    let request = store
        .put_with_key(&record, &key(project_id))
        .map_err(|e| js_error(&e))?;
    let _ = read(request).await?;
    completion.await
}

/// Load a previously persisted directory handle for a project.
pub async fn load_handle(project_id: i64) -> Result<Option<FileSystemDirectoryHandle>, String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readonly)?;
    let request = store.get(&key(project_id)).map_err(|e| js_error(&e))?;
    let result = read(request).await?;
    if result.is_null() || result.is_undefined() {
        return Ok(None);
    }
    let record = result.unchecked_into::<js_sys::Object>();
    let handle =
        js_sys::Reflect::get(&record, &JsValue::from_str("handle")).map_err(|e| js_error(&e))?;
    if handle.is_null() || handle.is_undefined() {
        return Ok(None);
    }
    handle
        .dyn_into::<FileSystemDirectoryHandle>()
        .map(Some)
        .map_err(|_| "stored handle is not a directory".to_string())
}

/// Re-request read/write permission for a handle (needed after a reload).
/// Returns true when permission is granted.
pub async fn request_permission(handle: &FileSystemDirectoryHandle) -> Result<bool, String> {
    let descriptor = FileSystemHandlePermissionDescriptor::new();
    descriptor.set_mode(FileSystemPermissionMode::Readwrite);
    let handle_ref: &FileSystemHandle = handle
        .dyn_ref::<FileSystemHandle>()
        .ok_or_else(|| "not a file system handle".to_string())?;
    let promise = handle_ref.request_permission_with_descriptor(&descriptor);
    let result = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| js_error(&e))?;
    Ok(result.as_string().as_deref() == Some("granted"))
}

pub async fn set_bridge_pairing_token(token: &str) -> Result<(), String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readwrite)?;
    let completion = TransactionFuture::new(store.transaction());
    let request = store
        .put_with_key(
            &JsValue::from_str(token),
            &JsValue::from_str("bridge_pairing_token"),
        )
        .map_err(|e| js_error(&e))?;
    let _ = read(request).await?;
    completion.await
}

pub async fn get_bridge_pairing_token() -> Result<Option<String>, String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readonly)?;
    let request = store
        .get(&JsValue::from_str("bridge_pairing_token"))
        .map_err(|e| js_error(&e))?;
    let result = read(request).await?;
    if result.is_null() || result.is_undefined() {
        return Ok(None);
    }
    Ok(result.as_string())
}

pub async fn delete_bridge_pairing_token() -> Result<(), String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readwrite)?;
    let completion = TransactionFuture::new(store.transaction());
    let request = store
        .delete(&JsValue::from_str("bridge_pairing_token"))
        .map_err(|e| js_error(&e))?;
    let _ = read(request).await?;
    completion.await
}

/// Remove only the numeric directory key, leaving the pairing token intact.
pub async fn delete_handle(project_id: i64) -> Result<(), String> {
    delete_key(&key(project_id)).await?;
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readonly)?;
    let keys = read(store.get_all_keys().map_err(|e| js_error(&e))?).await?;
    for stored in js_sys::Array::from(&keys).iter() {
        if stored.as_string().is_some_and(|key| {
            key.starts_with("transaction-root:")
                && key
                    .split(':')
                    .nth(2)
                    .and_then(|project| project.parse::<i64>().ok())
                    == Some(project_id)
        }) {
            delete_key(&stored).await?;
        }
    }
    Ok(())
}

async fn delete_key(key: &JsValue) -> Result<(), String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readwrite)?;
    let completion = TransactionFuture::new(store.transaction());
    let request = store.delete(key).map_err(|e| js_error(&e))?;
    read(request).await?;
    completion.await
}

/// Snapshot owned directory revisions before starting the account's project-list request.
/// Legacy records have unknown ownership and must survive another account's startup.
pub async fn orphan_candidates(
    user_id: openwebide_core::UserId,
) -> Result<Vec<(JsValue, JsValue)>, String> {
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readonly)?;
    let records = read(store.get_all().map_err(|e| js_error(&e))?).await?;
    let mut candidates = Vec::new();
    for record in js_sys::Array::from(&records).iter() {
        if !record.is_object() {
            continue;
        }
        let owner = js_sys::Reflect::get(&record, &JsValue::from_str("userId"))
            .map_err(|e| js_error(&e))?;
        let project = js_sys::Reflect::get(&record, &JsValue::from_str("projectId"))
            .map_err(|e| js_error(&e))?;
        let revision = js_sys::Reflect::get(&record, &JsValue::from_str("revision"))
            .map_err(|e| js_error(&e))?;
        if owner == key(user_id.get())
            && project.as_f64().is_some()
            && revision.as_string().is_some()
        {
            candidates.push((project, revision));
        }
    }
    Ok(candidates)
}

/// Delete only unchanged owned records saved before the project-list request.
pub async fn delete_orphan_handles(
    user_id: openwebide_core::UserId,
    candidates: &[(JsValue, JsValue)],
    project_ids: &[i64],
    is_current: impl Fn() -> bool,
) -> Result<(), String> {
    let db = open_db().await?;
    for (stored_key, revision) in candidates {
        if !is_current() {
            return Ok(());
        }
        if project_ids
            .iter()
            .any(|project_id| key(*project_id) == *stored_key)
        {
            continue;
        }
        // Read and conditional deletion share a transaction, excluding concurrent tab writes.
        let store = get_store(&db, IdbTransactionMode::Readwrite)?;
        let completion = TransactionFuture::new(store.transaction());
        let record = read(store.get(stored_key).map_err(|e| js_error(&e))?).await?;
        if !is_current() {
            return Ok(());
        }
        if !record.is_undefined()
            && !record.is_null()
            && js_sys::Reflect::get(&record, &JsValue::from_str("userId"))
                .map_err(|e| js_error(&e))?
                == key(user_id.get())
            && js_sys::Reflect::get(&record, &JsValue::from_str("revision"))
                .map_err(|e| js_error(&e))?
                == *revision
        {
            read(store.delete(stored_key).map_err(|e| js_error(&e))?).await?;
        }
        completion.await?;
    }
    Ok(())
}

/// Retain the origin-bound capability used by one reviewed transaction. The
/// journal itself stays in the database; IndexedDB stores only directory handles.
fn transaction_root_key(user: i64, project: i64, id: &str) -> Result<JsValue, String> {
    if user <= 0 || project <= 0 || !openwebide_core::workspace_transaction::valid_id(id) {
        return Err("Invalid workspace folder capability scope".into());
    }
    Ok(JsValue::from_str(&format!(
        "transaction-root:{user}:{project}:{id}"
    )))
}
pub async fn load_transaction_root(
    user: i64,
    project: i64,
    id: &str,
) -> Result<Option<FileSystemDirectoryHandle>, String> {
    let key = transaction_root_key(user, project, id)?;
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readonly)?;
    let value = read(store.get(&key).map_err(|e| js_error(&e))?).await?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    value
        .dyn_into()
        .map(Some)
        .map_err(|_| "The original workspace folder capability is invalid".into())
}
pub async fn capture_transaction_root(
    user: i64,
    project: i64,
    id: &str,
    handle: &FileSystemDirectoryHandle,
) -> Result<(), String> {
    // Callers hold the workspace Web Lock while checking and saving this binding.
    if let Some(original) = load_transaction_root(user, project, id).await? {
        let same = wasm_bindgen_futures::JsFuture::from(original.is_same_entry(handle))
            .await
            .map_err(|e| js_error(&e))?;
        if same.as_bool() != Some(true) {
            return Err("Reconnect the original project folder before recovery".into());
        }
        return Ok(());
    }
    let key = transaction_root_key(user, project, id)?;
    let db = open_db().await?;
    let store = get_store(&db, IdbTransactionMode::Readwrite)?;
    let completion = TransactionFuture::new(store.transaction());
    let _ = read(store.put_with_key(handle, &key).map_err(|e| js_error(&e))?).await?;
    completion.await
}
pub async fn delete_transaction_root(user: i64, project: i64, id: &str) -> Result<(), String> {
    delete_key(&transaction_root_key(user, project, id)?).await
}
