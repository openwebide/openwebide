//! Complete-only document loading above the shared workspace adapters.
use super::{EditorActions, EditorText};
use leptos::prelude::*;
use openwebide_core::editor::{DocumentPreparation, EditorAdmission};

impl EditorActions {
    /// Prepare an incoming file while retaining the current buffer. The caller's
    /// guard covers its workspace root/transport; this facade also protects editor
    /// ownership and drafts across every browser yield and final publication.
    pub(crate) async fn publish_read(
        self,
        content: String,
        baseline: &EditorText,
        current: impl Fn() -> bool,
    ) -> bool {
        let scope = self.presentation_scope();
        let revision = self.workspace.editor_source_revision.get_untracked();
        let owned = || {
            current()
                && !self.workspace.content.is_disposed()
                && untrack(|| self.presentation_scope()) == scope
                && self.workspace.editor_source_revision.get_untracked() == revision
                && !self.workspace.dirty.get_untracked()
                && self
                    .workspace
                    .content
                    .with_untracked(|text| text == baseline)
        };
        if scope.is_none() || !owned() {
            return false;
        }
        let text = EditorText::from(content);
        let source = text.shared();
        let document = match prepare_document(&source, &owned).await {
            Ok(Some(document)) => Some(document),
            Ok(None) => return false,
            Err(admission) => {
                text.record_admission(admission);
                None
            }
        };
        if !owned() {
            return false;
        }
        let scope = scope.unwrap();
        batch(|| {
            self.workspace.editor_documents.update(|documents| {
                let key = (scope.project, scope.path);
                if let Some(mut document) = document {
                    if documents
                        .get(&key)
                        .is_none_or(|existing| !existing.matches_text(&source))
                    {
                        document.enforce_editor_limits();
                        documents.insert(key, document);
                    }
                } else {
                    // Over-limit sources use the existing bounded read-only view.
                    documents.remove(&key);
                }
            });
            if self
                .workspace
                .content
                .with_untracked(|current| current.as_str() != source.as_str())
            {
                self.workspace.content.set(text);
            }
        });
        true
    }
}

/// Shared preparation policy for file reads and recovery; adapters only provide
/// incoming text and runtime scheduling. Cancellation never exposes partial rows.
async fn prepare_document<'a>(
    source: &'a std::sync::Arc<String>,
    current: &impl Fn() -> bool,
) -> Result<
    Option<openwebide_core::editor::Document>,
    openwebide_core::editor::EditorAdmissionResult<'a>,
> {
    if !current() {
        return Ok(None);
    }
    let mut admission = EditorAdmission::new(source);
    while !admission.advance(8) {
        crate::util::yield_task().await;
        if !current() {
            return Ok(None);
        }
    }
    let admission = admission.finish_with_source().unwrap();
    if admission.limit().is_some() {
        return Err(admission);
    }
    let mut preparation = DocumentPreparation::new(source);
    while !preparation.advance(64) {
        crate::util::yield_task().await;
        if !current() {
            return Ok(None);
        }
    }
    if !current() {
        return Ok(None);
    }
    let mut document = preparation.finish().unwrap();
    document.enforce_editor_limits();
    Ok(Some(document))
}

impl EditorActions {
    pub(crate) async fn restore_recovery(
        workspace: crate::state::workspace::WorkspaceState,
        project: &openwebide_core::Project,
        guard: &crate::state::workspace::EditorRecoveryGuard,
        recovery: &openwebide_core::editor::EditorRecovery,
        active_read_only: bool,
        current: impl Fn() -> bool,
    ) -> Result<Option<crate::state::workspace::EditorRecoveryHydration>, String> {
        if !current()
            || !workspace.can_restore_editor_recovery(project, guard, recovery, active_read_only)?
        {
            return Ok(None);
        }
        let source_revision = workspace.editor_source_revision.get_untracked();
        let owned = || {
            current()
                && workspace.editor_source_revision.try_get_untracked() == Some(source_revision)
                && workspace.pending_epoch.try_get_untracked() == Some(guard.epoch)
                && workspace.editor_read_revision.try_get_untracked() == Some(guard.read_revision)
                && workspace
                    .editor_composition
                    .try_with_untracked(|composition| {
                        composition
                            .as_ref()
                            .is_none_or(|composition| composition.key.0 != project.id)
                    })
                    == Some(true)
        };
        let mut documents = std::collections::HashMap::new();
        for file in &recovery.files {
            if let Some(saved) = &file.document {
                let Some(document) = Self::prepare_recovery(saved, owned).await? else {
                    return Ok(None);
                };
                documents.insert((project.id, file.path.clone()), document);
            }
        }
        if !owned()
            || !workspace.can_restore_editor_recovery(project, guard, recovery, active_read_only)?
        {
            return Ok(None);
        }
        let prepared = crate::state::workspace::PreparedEditorRecovery::from_documents(
            project.id, recovery, documents,
        );
        Ok(Some(
            workspace.install_editor_recovery(project, recovery, prepared),
        ))
    }

    pub(crate) async fn prepare_source(
        source: &std::sync::Arc<String>,
        current: impl Fn() -> bool,
    ) -> Result<Option<openwebide_core::editor::Document>, openwebide_core::editor::EditorLimit>
    {
        prepare_document(source, &current)
            .await
            .map_err(|admission| admission.limit().unwrap())
    }

    pub(crate) async fn prepare_recovery(
        recovery: &openwebide_core::editor::DocumentRecovery,
        current: impl Fn() -> bool,
    ) -> Result<Option<openwebide_core::editor::Document>, String> {
        if !current() {
            return Ok(None);
        }
        recovery.validate()?;
        let saved_source = recovery.saved.clone();
        let Some(saved) = prepare_document(&saved_source, &current)
            .await
            .map_err(|admission| {
                openwebide_core::editor::EditError::Capacity(admission.limit().unwrap()).to_string()
            })?
        else {
            return Ok(None);
        };
        let draft = if recovery.text == recovery.saved {
            saved.clone()
        } else {
            let source = recovery.text.clone();
            let Some(draft) = prepare_document(&source, &current)
                .await
                .map_err(|admission| {
                    openwebide_core::editor::EditError::Capacity(admission.limit().unwrap())
                        .to_string()
                })?
            else {
                return Ok(None);
            };
            draft
        };
        if !current() {
            return Ok(None);
        }
        recovery.restore_prepared(saved, draft).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        future::Future,
        task::{Context, Poll, Waker},
    };

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn recovery_hydration_yields_and_publishes_complete_documents_only() {
        use openwebide_core::{
            Project, WorkspaceMode,
            editor::{
                DocumentRecovery, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot,
                RecoveryScroll, Selection,
            },
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for phase in ["saved", "draft"] {
                for stale in [
                    "complete", "draft", "read", "epoch", "root", "account", "dispose",
                ] {
                    let owner = Owner::new();
                    let project = Project {
                        id: 1,
                        user_id: None,
                        created_at: 1,
                        name: "recovery".into(),
                        path: Some("project".into()),
                        mode,
                    };
                    let (workspace, auth, guard) = owner.with(|| {
                        let auth = crate::state::auth::AuthState::new();
                        let workspace = crate::state::workspace::WorkspaceState::new();
                        workspace.active_project.set(Some(1));
                        workspace.open_file.set(Some("old.txt".into()));
                        workspace.content.set("keep".into());
                        let guard = workspace.editor_recovery_guard(&project, false).unwrap();
                        (workspace, auth, guard)
                    });
                    let incoming = "文😀\r\n".repeat(2000);
                    let saved = if phase == "saved" {
                        incoming.clone()
                    } else {
                        "base".into()
                    };
                    let recovery = EditorRecovery {
                        format: 1,
                        root: Some(EditorRecoveryRoot::for_project(&project)),
                        selected: Some("incoming.txt".into()),
                        files: vec![EditorRecoveryFile {
                            path: "incoming.txt".into(),
                            document: Some(DocumentRecovery {
                                text: format!("{incoming}tail").into(),
                                saved: saved.clone().into(),
                                selections: vec![Selection::caret(0)],
                                collapsed: vec![],
                            }),
                            scroll: RecoveryScroll::default(),
                            read_only: false,
                        }],
                    };
                    let root = Cell::new(true);
                    let account = auth.generation.get_untracked();
                    let mut pending = Box::pin(EditorActions::restore_recovery(
                        workspace,
                        &project,
                        &guard,
                        &recovery,
                        false,
                        || root.get() && auth.generation.try_get_untracked() == Some(account),
                    ));
                    assert!(matches!(
                        pending
                            .as_mut()
                            .poll(&mut Context::from_waker(Waker::noop())),
                        Poll::Pending
                    ));
                    assert_eq!(workspace.content.get_untracked(), "keep");
                    assert!(
                        workspace
                            .editor_documents
                            .with_untracked(std::collections::HashMap::is_empty)
                    );
                    match stale {
                        "draft" => {
                            workspace.content.set("new draft".into());
                            workspace.dirty.set(true);
                        }
                        "read" => {
                            workspace.begin_editor_read();
                        }
                        "epoch" => workspace.pending_epoch.update(|epoch| *epoch += 1),
                        "root" => root.set(false),
                        "account" => auth.generation.update(|account| *account += 1),
                        "dispose" => owner.cleanup(),
                        _ => {}
                    }
                    assert_eq!(pending.await.unwrap().is_some(), stale == "complete");
                    if stale == "complete" {
                        assert_eq!(
                            workspace.open_file.get_untracked().as_deref(),
                            Some("incoming.txt")
                        );
                        workspace.editor_documents.update(|documents| {
                            let document = documents.get_mut(&(1, "incoming.txt".into())).unwrap();
                            assert_eq!(document.text(), format!("{incoming}tail"));
                            assert!(std::sync::Arc::ptr_eq(
                                &document.shared_text(),
                                &workspace.content.get_untracked().shared()
                            ));
                            let captured = document.recovery();
                            let payload = recovery.files[0].document.as_ref().unwrap();
                            assert!(std::sync::Arc::ptr_eq(&captured.text, &payload.text));
                            assert!(std::sync::Arc::ptr_eq(&captured.saved, &payload.saved));
                            assert!(document.is_dirty());
                            assert!(document.undo());
                            assert_eq!(document.text(), saved);
                            assert!(!document.is_dirty());
                            assert!(document.redo());
                            assert_eq!(document.text(), format!("{incoming}tail"));
                        });
                    } else if stale != "dispose" {
                        assert_eq!(
                            workspace.content.get_untracked().as_str(),
                            if stale == "draft" {
                                "new draft"
                            } else {
                                "keep"
                            }
                        );
                        assert!(
                            workspace
                                .editor_documents
                                .with_untracked(std::collections::HashMap::is_empty)
                        );
                    }
                    owner.cleanup();
                }
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn incoming_documents_yield_and_preserve_drafts_and_ownership() {
        for phase in ["admission", "index"] {
            let incoming = if phase == "admission" {
                "文😀\r\n".repeat(12_000)
            } else {
                "文😀\r\n".repeat(2000)
            };
            for stale in [
                "complete", "draft", "read", "project", "account", "epoch", "root", "dispose",
            ] {
                let owner = Owner::new();
                let (workspace, auth, actions) = owner.with(|| {
                    let auth = crate::state::auth::AuthState::new();
                    provide_context(auth);
                    let workspace = crate::state::workspace::WorkspaceState::new();
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("incoming.rs".into()));
                    workspace.content.set("baseline".into());
                    (workspace, auth, EditorActions::new(workspace))
                });
                let baseline = workspace.content.get_untracked();
                let root_current = Cell::new(true);
                let mut pending =
                    Box::pin(
                        actions.publish_read(incoming.clone(), &baseline, || root_current.get()),
                    );
                assert!(matches!(
                    pending
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                assert_eq!(workspace.content.get_untracked(), baseline);
                assert!(
                    workspace
                        .editor_documents
                        .with_untracked(std::collections::HashMap::is_empty)
                );
                match stale {
                    "draft" => {
                        workspace.content.set("my draft".into());
                        workspace.dirty.set(true);
                    }
                    "read" => {
                        workspace.begin_editor_read();
                    }
                    "project" => workspace.active_project.set(Some(2)),
                    "account" => auth.generation.update(|generation| *generation += 1),
                    "epoch" => workspace.pending_epoch.update(|epoch| *epoch += 1),
                    "root" => root_current.set(false),
                    "dispose" => owner.cleanup(),
                    _ => {}
                }
                assert_eq!(pending.await, stale == "complete");
                if stale == "complete" {
                    workspace.editor_documents.with_untracked(|docs| {
                        let document = &docs[&(1, "incoming.rs".into())];
                        assert_eq!(document.text(), incoming);
                        assert!(!document.is_dirty());
                        assert!(std::sync::Arc::ptr_eq(
                            &document.shared_text(),
                            &workspace.content.get_untracked().shared()
                        ));
                        assert_eq!(
                            document.byte_to_textarea(incoming.len()).unwrap(),
                            incoming.encode_utf16().count()
                                - incoming.bytes().filter(|byte| *byte == b'\r').count()
                        );
                    });
                } else if stale != "dispose" {
                    assert_eq!(
                        workspace.content.get_untracked().as_str(),
                        if stale == "draft" {
                            "my draft"
                        } else {
                            "baseline"
                        }
                    );
                    assert!(
                        workspace
                            .editor_documents
                            .with_untracked(std::collections::HashMap::is_empty)
                    );
                }
                owner.cleanup();
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn unchanged_reads_retain_selection_and_undo_history() {
        use openwebide_core::editor::{Document, Edit, Selection};
        let owner = Owner::new();
        let (workspace, actions) = owner.with(|| {
            let workspace = crate::state::workspace::WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("same.txt".into()));
            workspace.content.set("old!".into());
            let mut document = Document::for_editor("old").unwrap();
            document
                .apply(
                    vec![Edit::replace(3..3, "!")],
                    vec![Selection::caret(2)],
                    None,
                )
                .unwrap();
            document.mark_saved();
            workspace.editor_documents.update(|docs| {
                docs.insert((1, "same.txt".into()), document);
            });
            (workspace, EditorActions::new(workspace))
        });
        let baseline = workspace.content.get_untracked();
        assert!(
            actions
                .publish_read("old!".into(), &baseline, || true)
                .await
        );
        workspace.editor_documents.update(|docs| {
            let document = docs.get_mut(&(1, "same.txt".into())).unwrap();
            assert_eq!(document.selections(), &[Selection::caret(2)]);
            assert!(!document.is_dirty());
            assert!(document.undo());
            assert_eq!(document.text(), "old");
        });
        owner.cleanup();
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn oversized_reads_keep_the_read_only_source_without_document_indexes() {
        let owner = Owner::new();
        let (workspace, actions) = owner.with(|| {
            let workspace = crate::state::workspace::WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("large.txt".into()));
            (workspace, EditorActions::new(workspace))
        });
        let incoming = "x".repeat(openwebide_core::editor::MAX_EDITOR_LINE_BYTES + 1);
        let source = std::sync::Arc::new(incoming.clone());
        assert_eq!(
            EditorActions::prepare_source(&source, || true).await,
            Err(openwebide_core::editor::EditorLimit::LineBytes)
        );
        assert!(
            actions
                .publish_read(incoming.clone(), &EditorText::default(), || true)
                .await
        );
        assert_eq!(workspace.content.get_untracked().as_str(), incoming);
        assert!(
            workspace
                .editor_documents
                .with_untracked(std::collections::HashMap::is_empty)
        );
        assert_eq!(
            actions.limit(),
            Some(openwebide_core::editor::EditorLimit::LineBytes)
        );
        owner.cleanup();
    }
}
