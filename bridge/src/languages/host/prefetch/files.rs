//! Closed-file analysis owns its source bank and host slot through retirement.
use super::{super::*, Permit};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
};
use tokio::sync::OwnedMutexGuard;

const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub(in crate::languages::host) struct Documents {
    documents: Option<OwnedMutexGuard<documents::Documents>>,
    file: Option<File>,
}
struct File {
    service: ServiceUse,
    document: String,
    uri: String,
    _permit: Permit,
    documents: Option<OwnedMutexGuard<documents::Documents>>,
    retired: bool,
}
impl File {
    fn release_documents(&mut self, delivered: bool) {
        let entry = &self.service.0;
        if !delivered {
            entry.stop();
            if let Some(session) = entry.session.get() {
                session.client.close();
            }
        }
        // Snapshot readers take this mutex before trying the documents mutex.
        // Release both together, including if the cleanup task is aborted.
        let mut retained = entry.retained.lock().unwrap();
        if delivered
            && !*entry.stopped.borrow()
            && entry
                .session
                .get()
                .is_some_and(|session| !session.client.is_closed())
        {
            retained.take();
        }
        drop(self.documents.take());
        self.retired = true;
    }
}
impl Drop for File {
    fn drop(&mut self) {
        if !self.retired {
            // Runtime/task loss must stop the original service before either
            // its source mutex or speculative slot becomes available again.
            self.release_documents(false);
        }
    }
}
impl Documents {
    pub(in crate::languages::host) fn new(
        documents: OwnedMutexGuard<documents::Documents>,
        entry: Arc<Entry>,
        input: &Request,
        path: &Path,
        permit: Option<&Permit>,
    ) -> Result<Self, String> {
        let file = if input.query.closes_prefetch() {
            documents.require_closed_prefetch(input, path)?;
            let permit = permit
                .ok_or("Closed-file prefetch has no host admission")?
                .clone();
            let uri = url::Url::from_file_path(path)
                .map_err(|()| "Invalid closed language document URI")?
                .to_string();
            // A stopped service can be replaced while its source mutex is held.
            // Retain the original drafts, never the temporary disk overlay.
            *entry.retained.lock().unwrap() = Some(documents.snapshot());
            entry
                .active_uses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(File {
                service: ServiceUse(entry),
                document: input.document.clone(),
                uri,
                _permit: permit,
                documents: None,
                retired: false,
            })
        } else {
            None
        };
        Ok(Self {
            documents: Some(documents),
            file,
        })
    }

    pub(in crate::languages::host) async fn finish(&mut self) -> Result<(), String> {
        if let Some(completion) = self.retire() {
            completion.wait().await?;
        }
        Ok(())
    }

    fn prepare_retirement(&mut self) -> Option<(File, Result<Option<String>, String>)> {
        let mut file = self.file.take()?;
        file.documents = self.documents.take();
        let documents = file
            .documents
            .as_mut()
            .expect("closed source bank is owned");
        let entry = &file.service.0;
        let removed = documents.retire_prefetch(&file.document);
        if let Some(session) = entry.session.get() {
            session.diagnostics.lock().unwrap().remove(&file.uri);
        }
        *entry.retained.lock().unwrap() = Some(documents.snapshot());
        Some((file, removed))
    }

    fn retire(&mut self) -> Option<crate::exec::proc::Cleanup> {
        let (mut file, removed) = self.prepare_retirement()?;
        let entry = file.service.0.clone();
        let (done, completion) = crate::exec::proc::cleanup_channel();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Runtime loss cannot leave an eligible service with a disk overlay.
            entry.stop();
            if let Some(session) = entry.session.get() {
                session.client.close();
            }
            done.send_replace(Some(Err(
                "Closed-file prefetch cleanup lost its runtime".into()
            )));
            return Some(completion);
        };
        let worker = runtime.spawn(async move {
            let result = match (removed, entry.session.get()) {
                (Ok(Some(uri)), Some(session)) => tokio::time::timeout(
                    CLOSE_TIMEOUT,
                    session
                        .client
                        .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}})),
                )
                .await
                .map_err(|_| "Closed-file prefetch retirement timed out".to_string())
                .and_then(|result| result.map_err(|error| error.to_string())),
                (Ok(None), _) => Ok(()),
                (Err(error), _) => Err(error),
                (_, None) => {
                    Err("Original language service disappeared before source retirement".into())
                }
            };
            if let Err(error) = &result {
                // The RPC writer preserves FIFO after an acknowledged enqueue.
                // If enqueue failed, stop the original service before releasing
                // its documents; future analysis must not see an unclosed source.
                entry.failure.lock().unwrap().reason.clone_from(error);
            }
            file.release_documents(result.is_ok());
            if result.is_err() {
                let _ = tokio::time::timeout(super::TIMEOUT, entry.stop_and_wait()).await;
            }
            // Cancellation/handoff cannot steal this slot or service pin while
            // the owned retirement is pending, even if its observer was dropped.
            drop(file);
            done.send_replace(Some(result));
        });
        crate::exec::proc::track_cleanup(worker);
        Some(completion)
    }
}
impl Deref for Documents {
    type Target = documents::Documents;
    fn deref(&self) -> &Self::Target {
        self.documents
            .as_ref()
            .expect("source retirement follows analysis")
    }
}
impl DerefMut for Documents {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.documents
            .as_mut()
            .expect("source retirement follows analysis")
    }
}
impl Drop for Documents {
    fn drop(&mut self) {
        let _ = self.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::host::reuse::tests::{Fixture, input};

    #[tokio::test]
    async fn aborted_retirement_stops_the_original_service_before_releasing_its_source_bank() {
        let fixture = Fixture::new();
        let (_, first) = fixture.query(&input("A", "a.notes", "A unsaved", 1)).await;
        fixture.query(&input("A", "c.notes", "C unsaved", 2)).await;
        let entry = fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        let original = entry.process_identity().unwrap();
        let permit = fixture.host.inner.prefetch.acquire().unwrap();
        let session = entry.session.get().unwrap();
        let mut closed = input("A", "b.notes", "temporary closed-file source", 3);
        closed.query = Query::Prefetch {
            kind: protocol::PrefetchKind::Symbols,
            close: true,
        };
        let path = fixture.root.path().canonicalize().unwrap().join("b.notes");
        let mut documents = Documents::new(
            session.documents.clone().lock_owned().await,
            entry.clone(),
            &closed,
            &path,
            Some(&permit),
        )
        .unwrap();
        let normal = Request {
            query: Query::Hover {
                position: protocol::Position::default(),
            },
            ..closed
        };
        let (response, _) = documents
            .query(session, &normal, &path, "notes")
            .await
            .unwrap();
        let sources: Value =
            serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap();
        assert_eq!(sources["open"]["b.notes"], "temporary closed-file source");
        let (file, removed) = documents.prepare_retirement().unwrap();
        assert!(removed.unwrap().is_some());
        drop(permit);
        let (started, observed) = tokio::sync::oneshot::channel();
        let cleanup = tokio::spawn(async move {
            started.send(()).unwrap();
            std::future::pending::<()>().await;
            drop(file);
        });
        observed.await.unwrap();
        assert!(entry.busy());
        assert!(session.documents.try_lock().is_err());
        assert!(fixture.host.inner.prefetch.acquire().is_err());
        // Foreground work in another client can proceed on an independent
        // service while the original cleanup still owns its bank and slot.
        let (_, independent) = fixture
            .query(&input("B", "b.notes", "B independent", 1))
            .await;
        assert_eq!(independent["open"], json!({"b.notes":"B independent"}));
        assert_ne!(independent["pid"], first["pid"]);
        assert!(entry.busy());
        assert!(fixture.host.inner.prefetch.acquire().is_err());
        cleanup.abort();
        assert!(cleanup.await.unwrap_err().is_cancelled());
        assert!(*entry.stopped.borrow());
        assert!(session.client.is_closed());
        assert!(session.documents.try_lock().is_ok());
        assert!(fixture.host.inner.prefetch.acquire().is_ok());
        let (_, recovered) = fixture
            .query(&input("A", "a.notes", "A recovered", 4))
            .await;
        assert_eq!(
            recovered["open"],
            json!({"a.notes":"A recovered","c.notes":"C unsaved"})
        );
        assert_eq!(
            recovered["pid"], independent["pid"],
            "Only the interrupted original process must stop"
        );
        tokio::time::timeout(Duration::from_secs(5), original.completion.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fixture.starts(), 2);
    }
}
