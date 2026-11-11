use super::*;
use crate::testing::fake_backend::FakeBackend;
use futures::{FutureExt, channel::oneshot};
use std::cell::Cell;

#[wasm_bindgen_test::wasm_bindgen_test]
async fn accepted_mutations_keep_the_lock_after_the_observer_is_cancelled() {
    let owner = Owner::new();
    let backend = Rc::new(FakeBackend::default());
    let api: Api = owner.with(|| StoredValue::new_local(backend.clone() as Rc<dyn Backend>));
    let scope = WorkspaceMutations::new(api, 1, Rc::new(|| true)).unwrap();
    let writes = Rc::new(Cell::new(0));
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let done = writes.clone();
    let mut first = Box::pin(scope.run(async move {
        entered.send(()).unwrap();
        released.await.unwrap();
        done.set(done.get() + 1);
        Ok(())
    }));
    assert!(first.as_mut().now_or_never().is_none());
    started.await.unwrap();
    drop(first);
    let done = writes.clone();
    let mut next = Box::pin(scope.run(async move {
        done.set(done.get() + 1);
        Ok(())
    }));
    assert!(next.as_mut().now_or_never().is_none());
    crate::util::sleep_ms(20).await;
    assert_eq!(writes.get(), 0);
    assert_eq!(backend.workspace_transactions.latest_reads.get(), 1);
    release.send(()).unwrap();
    next.await.unwrap();
    assert_eq!(writes.get(), 2);
    assert_eq!(backend.workspace_transactions.latest_reads.get(), 2);
    owner.cleanup();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn failed_and_changed_recovery_checks_never_poll_the_write() {
    let owner = Owner::new();
    let backend = Rc::new(FakeBackend::default());
    let api: Api = owner.with(|| StoredValue::new_local(backend.clone() as Rc<dyn Backend>));
    let current = Rc::new(Cell::new(true));
    let check = {
        let current = current.clone();
        Rc::new(move || current.get())
    };
    let scope = WorkspaceMutations::new(api, 1, check).unwrap();
    let writes = Rc::new(Cell::new(0));
    backend.workspace_transactions.latest_error.set(true);
    let done = writes.clone();
    assert!(
        scope
            .run(async move {
                done.set(1);
                Ok(())
            })
            .await
            .is_err()
    );
    backend.workspace_transactions.latest_error.set(false);
    backend
        .workspace_transactions
        .command_journal_error
        .set(true);
    let done = writes.clone();
    assert!(
        scope
            .run(async move {
                done.set(1);
                Ok(())
            })
            .await
            .is_err()
    );
    backend
        .workspace_transactions
        .command_journal_error
        .set(false);
    backend.workspace_transactions.latest_delay_ms.set(50);
    let done = writes.clone();
    let mut work = Box::pin(scope.run(async move {
        done.set(1);
        Ok(())
    }));
    assert!(work.as_mut().now_or_never().is_none());
    crate::util::sleep_ms(10).await;
    current.set(false);
    assert!(work.await.is_err());
    assert_eq!(writes.get(), 0);
    owner.cleanup();
}
