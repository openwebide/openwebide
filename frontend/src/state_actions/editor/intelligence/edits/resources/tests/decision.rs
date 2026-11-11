//! Editor publication and lost cleanup acknowledgements use the same UI path
//! for both adapters. Native object/permission proofs live in adapter contracts.
use super::*;
use crate::{
    state::workspace::WorkspaceRecoveryStatus, state_actions::editor_recovery::RecoveryPause,
};
use openwebide_core::workspace_transaction::{
    AdvanceWorkspaceTransaction, TransactionProgress, WorkspaceTransactionState,
};

struct Decision {
    owner: Owner,
    intelligence: EditorIntelligence,
    fake: Rc<FakeBackend>,
    folder: Option<wasm_bindgen::JsValue>,
    review: Arc<WorkspaceResourceReview>,
    after: EditorRecovery,
    pause: Option<RecoveryPause>,
    lock: Option<crate::state::workspace::WorkspaceEditGuard>,
    state: WorkspaceTransactionState,
}
impl Decision {
    async fn new(mode: WorkspaceMode) -> Self {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        drop(prepare_create_review(&owner, intelligence).await);
        intelligence.resources.update(|value| {
            Arc::get_mut(value.as_mut().unwrap())
                .unwrap()
                .draft
                .plan
                .version = 2;
        });
        let review = intelligence.resources.get_untracked().unwrap();
        let context = intelligence.resource_context.unwrap();
        let lock = intelligence
            .editor
            .workspace
            .begin_workspace_edit(1)
            .unwrap();
        let pause = context
            .recovery
            .pause_and_flush(1, Rc::new(|| true))
            .await
            .unwrap();
        let after = review.after_recovery().unwrap();
        let backend = context.api.with_value(Clone::clone);
        backend
            .begin_workspace_transaction(
                1,
                &BeginWorkspaceTransaction {
                    language_review: None,
                    id: review.id.clone(),
                    plan: review.draft.plan.clone(),
                    editor: review.patch(pause.record(), &after).unwrap(),
                },
            )
            .await
            .unwrap();
        for (revision, completed, active) in [(1, 0, false), (2, 0, true), (3, 1, false)] {
            backend
                .advance_workspace_transaction(
                    1,
                    &AdvanceWorkspaceTransaction {
                        id: review.id.clone(),
                        generation: backend
                            .workspace_transaction(1, Some(&review.id))
                            .await
                            .unwrap()
                            .unwrap()
                            .journal
                            .generation,
                        revision,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Applying,
                            completed,
                            active,
                            rollback_from: None,
                        },
                    },
                )
                .await
                .unwrap();
        }
        // The filesystem adapter has acknowledged the new empty file. This
        // fixture verifies publication policy, not native object preservation.
        Workspace::for_project(context.api, context.projects, 1)
            .unwrap()
            .write("interrupted.rs", "")
            .await
            .unwrap();
        backend
            .advance_workspace_transaction(
                1,
                &AdvanceWorkspaceTransaction {
                    id: review.id.clone(),
                    generation: backend
                        .workspace_transaction(1, Some(&review.id))
                        .await
                        .unwrap()
                        .unwrap()
                        .journal
                        .generation,
                    revision: 4,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Finalizing,
                        completed: 1,
                        active: false,
                        rollback_from: None,
                    },
                },
            )
            .await
            .unwrap();
        let state = backend
            .workspace_transaction(1, Some(&review.id))
            .await
            .unwrap()
            .unwrap();
        intelligence
            .editor
            .workspace
            .workspace_recovery
            .update(|entries| {
                entries.insert(1, WorkspaceRecoveryStatus::from_state(Some(&state)));
            });
        Self {
            owner,
            intelligence,
            fake,
            folder,
            review,
            after,
            pause: Some(pause),
            lock: Some(lock),
            state,
        }
    }
    async fn publish(&mut self) -> Result<(), String> {
        let documents = self
            .review
            .draft
            .documents
            .iter()
            .map(|(path, document)| ((1, path.clone()), document.document.clone()))
            .collect::<HashMap<_, _>>();
        let prepared = PreparedEditorRecovery::from_documents(1, &self.after, documents);
        self.intelligence
            .publish_resource_decision(
                &self.review,
                &self.state,
                self.pause.as_mut().unwrap(),
                prepared,
                &self.after,
                &|| true,
            )
            .await
    }
    fn release(&mut self) {
        self.pause.take();
        self.lock.take();
    }
    fn saved_text(&self, text: &str) -> bool {
        self.fake.editor_recovery_records.borrow()[&1]
            .state
            .files
            .iter()
            .any(|file| {
                file.document
                    .as_ref()
                    .is_some_and(|document| document.text.as_str() == text)
            })
    }
    async fn commit(&self) {
        self.intelligence
            .resource_context
            .unwrap()
            .api
            .with_value(Clone::clone)
            .advance_workspace_transaction(
                1,
                &AdvanceWorkspaceTransaction {
                    id: self.review.id.clone(),
                    generation: self.state.journal.generation.clone(),
                    revision: self.state.journal.revision,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Committed,
                        completed: 1,
                        active: false,
                        rollback_from: None,
                    },
                },
            )
            .await
            .unwrap();
    }
    async fn cleanup(self) {
        crate::idb::delete_transaction_root(1, 1, &self.review.id)
            .await
            .unwrap();
        drop(self.review);
        drop(self.owner);
        if let Some(folder) = self.folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn finalizing_publishes_editor_once_and_terminal_ack_preserves_newer_drafts_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mut case = Decision::new(mode).await;
        case.publish().await.unwrap();
        case.release();
        let workspace = case.intelligence.editor.workspace;
        assert_eq!(
            workspace
                .editor_recovery(&case.review.project, false)
                .unwrap(),
            case.after
        );
        let context = case.intelligence.resource_context.unwrap();
        assert_eq!(
            Workspace::for_project(context.api, context.projects, 1)
                .unwrap()
                .read("interrupted.rs")
                .await
                .unwrap(),
            ""
        );
        let status = workspace.workspace_recovery.get_untracked()[&1].clone();
        assert_eq!(status.recovery_label(), "Finish workspace recovery");
        assert!(status.message().contains("result is saved"));
        assert!(workspace.begin_editor_save(1).is_none());
        case.intelligence
            .editor
            .insert_native_text("newer draft", 123.0)
            .unwrap()
            .unwrap();
        let draft = workspace.content.get_untracked();
        wait("newer draft persists during cleanup", || {
            case.fake.editor_recovery_records.borrow()[&1]
                .state
                .files
                .iter()
                .any(|file| {
                    file.document
                        .as_ref()
                        .is_some_and(|document| document.text.as_str() == draft.as_str())
                })
        })
        .await;
        let context = case.intelligence.resource_context.unwrap();
        case.commit().await;
        // The window knows its editor decision, but still displays the last
        // Finalizing cursor after losing the terminal acknowledgement.
        let actions = install_workspace_recovery(&case.owner, case.intelligence);
        actions.check.run(1);
        // Preserve the pending cursor to exercise the explicit recovery action.
        workspace.workspace_recovery.update(|entries| {
            entries.insert(1, status);
        });
        actions.recover.run(1);
        wait("cleanup acknowledgement releases save fence", || {
            workspace.workspace_recovery.get_untracked().get(&1)
                == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        assert!(context.recovery.ready(1));
        assert_eq!(workspace.content.get_untracked(), draft);
        assert!(workspace.begin_editor_save(1).is_some());
        case.cleanup().await;
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn finalizing_publication_preserves_changed_editors_and_newer_database_drafts_in_both_modes()
{
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for database in [false, true] {
            let mut case = Decision::new(mode).await;
            let workspace = case.intelligence.editor.workspace;
            if database {
                let mut record = case.fake.editor_recovery_records.borrow()[&1].clone();
                record.revision += 1;
                record.state.files[0].document.as_mut().unwrap().text =
                    Arc::new("newer remote draft".into());
                case.fake
                    .editor_recovery_records
                    .borrow_mut()
                    .insert(1, record);
            } else {
                assert!(
                    case.intelligence
                        .editor
                        .insert_native_text("blocked", 122.0)
                        .is_err()
                );
                // Simulate losing editor ownership before the durable result
                // returns. The publication guard must still reject this edit.
                case.lock.take();
                case.intelligence
                    .editor
                    .insert_native_text("newer local draft", 123.0)
                    .unwrap()
                    .unwrap();
            }
            let content = workspace.content.get_untracked();
            let saved = case.fake.editor_recovery_records.borrow()[&1].clone();
            assert!(case.publish().await.is_err());
            case.release();
            assert_eq!(workspace.content.get_untracked(), content);
            assert_eq!(case.fake.editor_recovery_records.borrow()[&1], saved);
            assert!(matches!(
                workspace.workspace_recovery.get_untracked().get(&1),
                Some(WorkspaceRecoveryStatus::EditorConflict(_))
            ));
            assert!(
                !case
                    .intelligence
                    .resource_context
                    .unwrap()
                    .recovery
                    .acknowledge_workspace_commit(
                        1,
                        &case.review.id,
                        &case.fake.editor_recovery_records.borrow()[&1]
                    )
            );
            case.cleanup().await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn terminal_acknowledgements_preserve_other_window_drafts_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for explicit in [false, true] {
            let mut case = Decision::new(mode).await;
            case.publish().await.unwrap();
            case.release();
            let workspace = case.intelligence.editor.workspace;
            let content = workspace.content.get_untracked();
            let status = workspace.workspace_recovery.get_untracked()[&1].clone();
            let mut saved = case.fake.editor_recovery_records.borrow()[&1].clone();
            saved.revision += 1;
            saved.state.files[0].document.as_mut().unwrap().text =
                Arc::new("another window's later draft".into());
            case.fake
                .editor_recovery_records
                .borrow_mut()
                .insert(1, saved.clone());
            case.commit().await;
            let actions = install_workspace_recovery(&case.owner, case.intelligence);
            if explicit {
                workspace.workspace_recovery.update(|entries| {
                    entries.insert(1, status);
                });
                actions.recover.run(1);
            } else {
                actions.check.run(1);
            }
            wait("later database draft keeps the save fence", || {
                matches!(
                    workspace.workspace_recovery.get_untracked().get(&1),
                    Some(WorkspaceRecoveryStatus::EditorConflict(_))
                )
            })
            .await;
            assert_eq!(workspace.content.get_untracked(), content);
            assert_eq!(case.fake.editor_recovery_records.borrow()[&1], saved);
            assert!(workspace.begin_editor_save(1).is_none());
            let context = case.intelligence.resource_context.unwrap();
            assert!(!context.recovery.has_workspace_publication(1));
            if explicit {
                context.recovery.restore.run(1);
            } else {
                context.recovery.keep_current.run(1);
            }
            wait("explicit editor choice is reviewable", || {
                context.ui.confirm.get_untracked().is_some()
            })
            .await;
            let choice = context.ui.confirm.get_untracked().unwrap().action;
            context.ui.confirm.set(None);
            choice.run(());
            wait("explicit editor choice releases the conflict", || {
                context.recovery.ready(1)
                    && workspace.workspace_recovery.get_untracked().get(&1)
                        == Some(&WorkspaceRecoveryStatus::Ready)
            })
            .await;
            actions.check.run(1);
            wait("resolved editor choice survives another check", || {
                workspace.workspace_recovery.get_untracked().get(&1)
                    == Some(&WorkspaceRecoveryStatus::Ready)
            })
            .await;
            assert!(workspace.begin_editor_save(1).is_some());
            case.cleanup().await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn editor_publication_proof_is_bound_to_transaction_account_and_folder() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mut case = Decision::new(mode).await;
        case.publish().await.unwrap();
        case.release();
        let context = case.intelligence.resource_context.unwrap();
        assert!(!context.recovery.acknowledge_workspace_commit(
            1,
            &"a".repeat(32),
            &case.fake.editor_recovery_records.borrow()[&1]
        ));
        let original = context.settings.bridge_url.get_untracked();
        context
            .settings
            .bridge_url
            .set("http://different-host".into());
        assert!(!context.recovery.has_workspace_publication(1));
        assert!(!context.recovery.acknowledge_workspace_commit(
            1,
            &case.review.id,
            &case.fake.editor_recovery_records.borrow()[&1]
        ));
        context.settings.bridge_url.set(original);
        context
            .auth
            .generation
            .update(|generation| *generation += 1);
        assert!(!context.recovery.has_workspace_publication(1));
        assert!(!context.recovery.acknowledge_workspace_commit(
            1,
            &case.review.id,
            &case.fake.editor_recovery_records.borrow()[&1]
        ));
        case.cleanup().await;
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn cleanup_ack_waits_for_this_windows_committed_draft_save_reply_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for explicit in [false, true] {
            let mut case = Decision::new(mode).await;
            case.publish().await.unwrap();
            case.release();
            let workspace = case.intelligence.editor.workspace;
            let status = workspace.workspace_recovery.get_untracked()[&1].clone();
            let (ack, pending) = futures::channel::oneshot::channel();
            case.fake
                .recovery_save_acknowledgements
                .borrow_mut()
                .push_back(pending);
            case.intelligence
                .editor
                .insert_native_text("draft awaiting its save reply", 500.0)
                .unwrap()
                .unwrap();
            let draft = workspace.content.get_untracked();
            wait(
                "draft is committed while its acknowledgement is held",
                || case.saved_text(&draft),
            )
            .await;
            case.commit().await;
            let reads = case.fake.workspace_transactions.latest_reads.get();
            let actions = install_workspace_recovery(&case.owner, case.intelligence);
            if explicit {
                workspace.workspace_recovery.update(|entries| {
                    entries.insert(1, status);
                });
                actions.recover.run(1);
            } else {
                actions.check.run(1);
            }
            if explicit {
                wait(
                    "explicit recovery owns the editor while a save reply is held",
                    || workspace.editor_operation_active_untracked(1),
                )
                .await;
            } else {
                wait(
                    "terminal journal was checked with an in-flight save",
                    || case.fake.workspace_transactions.latest_reads.get() > reads,
                )
                .await;
            }
            crate::util::sleep_ms(50).await;
            assert!(!matches!(
                workspace.workspace_recovery.get_untracked().get(&1),
                Some(WorkspaceRecoveryStatus::EditorConflict(_))
            ));
            assert!(workspace.begin_editor_save(1).is_none());
            ack.send(()).unwrap();
            wait(
                "own save acknowledgement releases cleanup without a conflict",
                || {
                    workspace.workspace_recovery.get_untracked().get(&1)
                        == Some(&WorkspaceRecoveryStatus::Ready)
                },
            )
            .await;
            assert_eq!(workspace.content.get_untracked(), draft);
            assert!(case.saved_text(&draft));
            assert!(workspace.begin_editor_save(1).is_some());
            case.cleanup().await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_stale_recovery_read_after_an_own_save_reply_is_rechecked_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mut case = Decision::new(mode).await;
        case.publish().await.unwrap();
        case.release();
        let workspace = case.intelligence.editor.workspace;
        let actions = install_workspace_recovery(&case.owner, case.intelligence);
        actions.check.run(1);
        wait(
            "initial cleanup cursor is checked before holding a read",
            || {
                matches!(
                    workspace.workspace_recovery.get_untracked().get(&1),
                    Some(WorkspaceRecoveryStatus::Pending {
                        recovering: false,
                        ..
                    })
                )
            },
        )
        .await;
        crate::util::sleep_ms(20).await;
        let stale = case.fake.editor_recovery_records.borrow()[&1].clone();
        let (reply, pending) = futures::channel::oneshot::channel();
        case.fake
            .recovery_load_results
            .borrow_mut()
            .push_back(pending);
        case.commit().await;
        actions.check.run(1);
        wait("cleanup's editor read is in flight", || {
            case.fake.recovery_load_results.borrow().is_empty()
        })
        .await;
        case.intelligence
            .editor
            .insert_native_text("draft after the old database read", 700.0)
            .unwrap()
            .unwrap();
        let draft = workspace.content.get_untracked();
        wait(
            "new draft is acknowledged before the old read reply",
            || case.saved_text(&draft),
        )
        .await;
        reply.send(Ok(stale)).unwrap();
        wait(
            "older read is refreshed without a spurious conflict",
            || {
                workspace.workspace_recovery.get_untracked().get(&1)
                    == Some(&WorkspaceRecoveryStatus::Ready)
            },
        )
        .await;
        assert_eq!(workspace.content.get_untracked(), draft);
        assert!(case.saved_text(&draft));
        assert!(workspace.begin_editor_save(1).is_some());
        case.cleanup().await;
    }
}
