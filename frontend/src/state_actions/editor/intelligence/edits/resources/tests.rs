use super::*;
use crate::{
    state::{editor_recovery::EditorRecoveryState, settings::Theme},
    testing::fake_backend::FakeBackend,
};
use openwebide_core::{
    User, UserId, UserRole, WorkspaceMode,
    plugins::languages::intelligence::{Position, Range, results::FileEdit},
};
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};
mod decision;

#[wasm_bindgen(inline_js = r#"
export async function resourceTestFolder() {
 const parent = await navigator.storage.getDirectory();
 const name = 'resource-ui-' + crypto.randomUUID();
 return {parent, name, handle:await parent.getDirectoryHandle(name,{create:true})};
}
export function resourceTestHandle(folder) {return folder.handle;}
export async function resourceTestCleanup(folder) {await folder.parent.removeEntry(folder.name,{recursive:true});}
export function resourceTestPicker(handle) {
 const previous=window.showDirectoryPicker;
 window.showDirectoryPicker=async()=>structuredClone(handle);
 return ()=>{window.showDirectoryPicker=previous;};
}
"#)]
extern "C" {
    async fn resourceTestFolder() -> wasm_bindgen::JsValue;
    fn resourceTestHandle(folder: &wasm_bindgen::JsValue) -> wasm_bindgen::JsValue;
    async fn resourceTestCleanup(folder: &wasm_bindgen::JsValue);
    fn resourceTestPicker(handle: &wasm_bindgen::JsValue) -> js_sys::Function;
}
async fn wait(message: &str, condition: impl Fn() -> bool) {
    let deadline = js_sys::Date::now() + 4000.0;
    while !condition() {
        assert!(js_sys::Date::now() < deadline, "{message}");
        crate::util::sleep_ms(10).await;
    }
}
fn text(path: &str, before: &str, after: &str) -> WorkspaceChange {
    WorkspaceChange::Text {
        file: FileEdit {
            path: path.into(),
            source_digest: source_digest(before),
            revision: None,
            edits: vec![TextEdit {
                range: Range {
                    start: Position::default(),
                    end: protocol::position(before, before.len()).unwrap(),
                },
                new_text: after.into(),
            }],
        },
    }
}
async fn prepare(owner: &Owner, intelligence: EditorIntelligence, edit: WorkspaceEdit) {
    let fake = owner.with(expect_context::<send_wrapper::SendWrapper<Rc<FakeBackend>>>);
    super::super::tests::prepare(&fake, intelligence, edit).await;
}
async fn fixture(
    mode: WorkspaceMode,
) -> (
    Owner,
    EditorIntelligence,
    Rc<FakeBackend>,
    Option<wasm_bindgen::JsValue>,
) {
    let folder = if mode == WorkspaceMode::Local {
        Some(resourceTestFolder().await)
    } else {
        None
    };
    let (owner, mut intelligence, fake) = super::super::tests::fixture(mode);
    let handle = folder.as_ref().map(resourceTestHandle);
    owner.with(|| {
        provide_context(send_wrapper::SendWrapper::new(fake.clone()));
        let auth = AuthState::new();
        auth.user.set(Some(User {
            id: UserId::new(1),
            username: "owner".into(),
            role: UserRole::Admin,
            created_at: 0,
        }));
        let projects = ProjectsState::new();
        projects.projects.set(vec![Project {
            id: 1,
            name: "fixture".into(),
            mode,
            path: Some("test".into()),
            user_id: Some(UserId::new(1)),
            created_at: 0,
        }]);
        projects.active_project.set(Some(1));
        projects.projects_loaded.set(true);
        if let Some(handle) = handle {
            projects.local_handles.update(|handles| {
                handles.insert(1, handle.unchecked_into());
            });
        }
        let api: Api = StoredValue::new_local(fake.clone() as Rc<dyn crate::backend::Backend>);
        let settings = SettingsState::new(Theme::Dark, "http://localhost:3001".into());
        let ui = UiState::new();
        provide_context(settings);
        provide_context(ui);
        provide_context(intelligence.workspace_actions.unwrap());
        let state = EditorRecoveryState::new();
        let read_only = RwSignal::new(false);
        provide_context(read_only);
        let recovery = RecoveryActions::new(
            api,
            auth,
            projects,
            intelligence.editor.workspace,
            state,
            read_only,
            Callback::new(|_| ()),
        );
        intelligence.resource_context = Some(ResourceContext {
            api,
            projects,
            auth,
            settings,
            recovery,
            ui,
            host: None,
            legacy_review: true,
        });
    });
    let context = intelligence.resource_context.unwrap();
    let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
    for path in ["first.rs", "second.rs", "third.rs"] {
        fake.files
            .borrow_mut()
            .insert((1, path.into()), "foo".into());
        workspace.write(path, "foo").await.unwrap();
    }
    wait("editor recovery initialized", || context.recovery.ready(1)).await;
    (owner, intelligence, fake, folder)
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn approved_ordered_resources_publish_tabs_and_drafts_once_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let changes = vec![
            text("first.rs", "foo", "FOO"),
            WorkspaceChange::File {
                operation: FileOperation::Rename {
                    from: "first.rs".into(),
                    to: "moved.rs".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                },
            },
            text("moved.rs", "FOO", "final"),
            WorkspaceChange::File {
                operation: FileOperation::Create {
                    path: "created.rs".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                },
            },
            text("created.rs", "", "created draft"),
            WorkspaceChange::File {
                operation: FileOperation::Delete {
                    path: "third.rs".into(),
                    recursive: false,
                    ignore_if_missing: false,
                },
            },
        ];
        prepare(
            &owner,
            intelligence,
            WorkspaceEdit {
                files: vec![],
                changes,
            },
        )
        .await;
        wait("resource review prepared", || {
            intelligence.resources.get_untracked().is_some()
                || intelligence.error.get_untracked().is_some()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        let review = intelligence.resources.get_untracked().unwrap();
        let context = intelligence.resource_context.unwrap();
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        assert_eq!(workspace.read("first.rs").await.unwrap(), "foo");
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        fake.workspace_transactions.lose_commit_reply.set(true);
        let (release, validation) = futures::channel::oneshot::channel();
        fake.native_analysis_results
            .borrow_mut()
            .push_back(validation);
        owner.with(|| {
            intelligence.apply_resource_workspace(review.clone(), Signal::derive(|| false));
        });
        wait("resource analysis validation before durable intent", || {
            fake.native_analysis_inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
        })
        .await;
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        assert_eq!(workspace.read("first.rs").await.unwrap(), "foo");
        assert!(workspace.read("moved.rs").await.is_err());
        release.send(Ok(())).unwrap();
        wait("ordered resource operation completed", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        assert!(intelligence.resources.get_untracked().is_none());
        assert_eq!(workspace.read("moved.rs").await.unwrap(), "foo");
        assert_eq!(workspace.read("created.rs").await.unwrap(), "");
        assert!(workspace.read("first.rs").await.is_err());
        assert!(workspace.read("third.rs").await.is_err());
        let state = intelligence.editor.workspace;
        assert_eq!(state.open_file.get_untracked().as_deref(), Some("moved.rs"));
        assert_eq!(state.content.get_untracked().as_str(), "final");
        assert_eq!(
            state.editor_tabs.get_untracked()[&1],
            ["moved.rs", "second.rs", "created.rs"]
        );
        state.editor_documents.update(|documents| {
            let document = documents.get_mut(&(1, "moved.rs".into())).unwrap();
            assert!(document.is_dirty());
            assert_eq!(document.recovery().saved.as_str(), "foo");
            assert!(document.undo());
            assert_eq!(document.text(), "FOO");
            assert!(document.undo());
            assert_eq!(document.text(), "foo");
            let other = documents.get_mut(&(1, "second.rs".into())).unwrap();
            assert_eq!(other.text(), "foo!");
            assert!(other.undo());
            assert_eq!(other.text(), "foo");
        });
        let committed = fake
            .workspace_transactions
            .load(1, Some(&review.id))
            .unwrap();
        assert_eq!(
            committed.journal.progress.phase,
            TransactionPhase::Committed
        );
        let editor = fake.editor_recovery_records.borrow()[&1].clone();
        assert!(
            editor
                .state
                .files
                .iter()
                .any(|file| file.path == "created.rs"
                    && file.document.as_ref().unwrap().text.as_str() == "created draft")
        );
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn stale_resource_review_aborts_unstarted_journal_without_overwriting_external_files() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        prepare(
            &owner,
            intelligence,
            WorkspaceEdit {
                files: vec![],
                changes: vec![WorkspaceChange::File {
                    operation: FileOperation::Rename {
                        from: "first.rs".into(),
                        to: "moved.rs".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                }],
            },
        )
        .await;
        wait("resource preview before external edit", || {
            intelligence.resources.get_untracked().is_some()
                || intelligence.error.get_untracked().is_some()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        let review = intelligence.resources.get_untracked().unwrap();
        let context = intelligence.resource_context.unwrap();
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        workspace
            .write("first.rs", "external change")
            .await
            .unwrap();
        owner.with(|| {
            intelligence.apply_resource_workspace(review.clone(), Signal::derive(|| false));
        });
        wait("stale review rejected", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert!(intelligence.error.get_untracked().is_some());
        assert_eq!(workspace.read("first.rs").await.unwrap(), "external change");
        assert!(workspace.read("moved.rs").await.is_err());
        assert_eq!(
            intelligence
                .editor
                .workspace
                .open_file
                .get_untracked()
                .as_deref(),
            Some("first.rs")
        );
        assert_eq!(
            fake.workspace_transactions
                .load(1, Some(&review.id))
                .unwrap()
                .journal
                .progress
                .phase,
            TransactionPhase::Restored
        );
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

async fn prepare_create_review(
    owner: &Owner,
    intelligence: EditorIntelligence,
) -> Arc<WorkspaceResourceReview> {
    prepare(
        owner,
        intelligence,
        WorkspaceEdit {
            files: vec![],
            changes: vec![WorkspaceChange::File {
                operation: FileOperation::Create {
                    path: "interrupted.rs".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                },
            }],
        },
    )
    .await;
    wait("interrupted-operation preview", || {
        intelligence.resources.get_untracked().is_some()
            || intelligence.error.get_untracked().is_some()
    })
    .await;
    assert_eq!(intelligence.error.get_untracked(), None);
    intelligence.resources.get_untracked().unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn resource_review_identity_and_native_rejection_precede_journal_creation_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let obsolete = prepare_create_review(&owner, intelligence).await;
        let review = prepare_create_review(&owner, intelligence).await;
        intelligence.apply_resource_workspace(obsolete, Signal::derive(|| false));
        assert!(
            !fake
                .native_analysis_inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
        );
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        *fake.native_analysis_error.borrow_mut() = Some("native analysis changed".into());
        intelligence.apply_resource_workspace(review.clone(), Signal::derive(|| false));
        wait("native resource validation rejects", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert!(
            intelligence
                .error
                .get_untracked()
                .unwrap()
                .contains("native analysis changed")
        );
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        assert!(Arc::ptr_eq(
            &review,
            &intelligence.resources.get_untracked().unwrap()
        ));
        let context = intelligence.resource_context.unwrap();
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        assert!(workspace.read("interrupted.rs").await.is_err());
        *fake.native_analysis_error.borrow_mut() = None;
        intelligence.apply_resource_workspace(review, Signal::derive(|| false));
        wait("resource review retry completes", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        assert_eq!(workspace.read("interrupted.rs").await.unwrap(), "");
        assert_eq!(
            fake.native_analysis_inputs
                .borrow()
                .iter()
                .filter(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
                .count(),
            2
        );
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn resource_validation_rechecks_intrinsic_read_only_before_creating_intent_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let review = prepare_create_review(&owner, intelligence).await;
        let read_only = owner.with(expect_context::<RwSignal<bool>>);
        let (release, validation) = futures::channel::oneshot::channel();
        fake.native_analysis_results
            .borrow_mut()
            .push_back(validation);
        intelligence.apply_resource_workspace(review.clone(), Signal::derive(|| false));
        wait(
            "resource validation before intrinsic read-only change",
            || {
                fake.native_analysis_inputs
                    .borrow()
                    .iter()
                    .any(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
            },
        )
        .await;
        read_only.set(true);
        release.send(Ok(())).unwrap();
        wait(
            "changed intrinsic read-only rejects resource intent",
            || !intelligence.busy.get_untracked(),
        )
        .await;
        assert!(
            intelligence
                .error
                .get_untracked()
                .unwrap()
                .contains("Editor files changed during analysis validation")
        );
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        let context = intelligence.resource_context.unwrap();
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        assert!(workspace.read("interrupted.rs").await.is_err());
        read_only.set(false);
        intelligence.apply_resource_workspace(review, Signal::derive(|| false));
        wait("unlocked resource review can retry", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        assert_eq!(workspace.read("interrupted.rs").await.unwrap(), "");
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn unavailable_resource_capability_preserves_validated_text_only_edits_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, mut intelligence, fake, folder) = fixture(mode).await;
        intelligence
            .resource_context
            .as_mut()
            .unwrap()
            .legacy_review = false;
        fake.workspace_transactions.latest_error.set(true);
        prepare(
            &owner,
            intelligence,
            WorkspaceEdit {
                changes: vec![],
                files: vec![FileEdit {
                    path: "first.rs".into(),
                    source_digest: source_digest("foo"),
                    revision: None,
                    edits: vec![TextEdit {
                        range: Range {
                            start: Position::default(),
                            end: Position {
                                line: 0,
                                character: 3,
                            },
                        },
                        new_text: "bar".into(),
                    }],
                }],
            },
        )
        .await;
        wait("text-only review without resource access", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        assert!(
            intelligence
                .analysis
                .get_value()
                .unwrap()
                .workspace_files()
                .is_err()
        );
        intelligence.apply_workspace(
            intelligence.review.get_untracked().unwrap(),
            Signal::derive(|| false),
        );
        wait(
            "validated text-only publication without resource access",
            || !intelligence.busy.get_untracked(),
        )
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        assert_eq!(intelligence.editor.source().as_str(), "bar");
        assert_eq!(
            fake.native_analysis_inputs
                .borrow()
                .iter()
                .filter(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
                .count(),
            1
        );
        assert!(fake.workspace_transactions.records.borrow().is_empty());
        assert_eq!(fake.files.borrow()[&(1, "first.rs".into())], "foo");
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn retained_action_rejects_command_tampering_and_edit_only_downgrades_before_resource_inspection()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let review = ActionReview {
            title: "Reviewed action".into(),
            edit: WorkspaceEdit {
                files: vec![],
                changes: vec![WorkspaceChange::File {
                    operation: FileOperation::Create {
                        path: "action.rs".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                }],
            },
            command: Some(ServerCommand {
                id: "action-1".into(),
                server: "test-native-server".into(),
                name: "reviewed.command".into(),
                arguments: vec![serde_json::json!("approved")],
            }),
        };
        super::super::tests::retain(
            &fake,
            intelligence,
            Query::ReviewAction {
                id: "action-1".into(),
            },
            serde_json::to_value(&review).unwrap(),
        )
        .await
        .unwrap();
        let calls = fake.workspace_transactions.inspections.borrow().len();
        let mut tampered = review.clone();
        tampered
            .command
            .as_mut()
            .unwrap()
            .arguments
            .push(serde_json::json!("unreviewed"));
        intelligence.prepare_action(tampered);
        assert!(
            intelligence
                .error
                .get_untracked()
                .unwrap()
                .contains("action review changed")
        );
        intelligence.prepare_workspace(review.edit);
        assert!(
            intelligence
                .error
                .get_untracked()
                .unwrap()
                .contains("complete action")
        );
        assert!(intelligence.resources.get_untracked().is_none());
        assert_eq!(
            fake.workspace_transactions.inspections.borrow().len(),
            calls
        );
        assert!(
            !fake
                .native_analysis_inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
        );
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}
async fn persist_create_review(
    owner: &Owner,
    intelligence: EditorIntelligence,
) -> Arc<WorkspaceResourceReview> {
    let review = prepare_create_review(owner, intelligence).await;
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
    let patch = review
        .patch(pause.record(), &review.after_recovery().unwrap())
        .unwrap();
    context
        .api
        .with_value(Clone::clone)
        .begin_workspace_transaction(
            1,
            &BeginWorkspaceTransaction {
                language_review: None,
                id: review.id.clone(),
                plan: review.draft.plan.clone(),
                editor: patch,
            },
        )
        .await
        .unwrap();
    drop(pause);
    drop(lock);
    review
}
fn install_workspace_recovery(
    owner: &Owner,
    intelligence: EditorIntelligence,
) -> crate::state_actions::workspace_recovery::WorkspaceRecoveryActions {
    let context = intelligence.resource_context.unwrap();
    owner.with(|| {
        provide_context(context.recovery);
        crate::state_actions::workspace_recovery::WorkspaceRecoveryActions::install(
            context.api,
            context.auth,
            context.projects,
            intelligence.editor.workspace,
        )
    })
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn workspace_recovery_discovers_and_restores_ambiguous_create_in_both_modes() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    use openwebide_core::workspace_transaction::{
        AdvanceWorkspaceTransaction, TransactionProgress,
    };
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let review = persist_create_review(&owner, intelligence).await;
        let context = intelligence.resource_context.unwrap();
        let backend = context.api.with_value(Clone::clone);
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        for (revision, active) in [(1, false), (2, true)] {
            backend
                .advance_workspace_transaction(
                    1,
                    &AdvanceWorkspaceTransaction {
                        id: review.id.clone(),
                        generation: context
                            .api
                            .with_value(Clone::clone)
                            .workspace_transaction(1, Some(&review.id))
                            .await
                            .unwrap()
                            .unwrap()
                            .journal
                            .generation,
                        revision,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Applying,
                            completed: 0,
                            active,
                            rollback_from: None,
                        },
                    },
                )
                .await
                .unwrap();
        }
        // The primitive completed, but its reply/progress acknowledgement did not.
        workspace.write("interrupted.rs", "").await.unwrap();
        let actions = install_workspace_recovery(&owner, intelligence);
        let state = intelligence.editor.workspace;
        wait("pending journal discovered after reload", || {
            matches!(
                state.workspace_recovery.get_untracked().get(&1),
                Some(WorkspaceRecoveryStatus::Pending { .. })
            )
        })
        .await;
        assert!(state.begin_editor_save(1).is_none());
        assert!(!state.editor_operation_active_untracked(1));
        assert!(
            intelligence
                .editor
                .insert_native_text("draft", 100.0)
                .unwrap()
                .is_some()
        );
        let draft = state.content.get_untracked();
        let original_tabs = state.editor_tabs.get_untracked();
        actions.recover.run(1);
        wait("ambiguous operation restored", || {
            state.workspace_recovery.get_untracked().get(&1)
                == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        assert!(workspace.read("interrupted.rs").await.is_err());
        assert_eq!(state.content.get_untracked(), draft);
        assert_eq!(state.editor_tabs.get_untracked(), original_tabs);
        assert!(state.begin_editor_save(1).is_some());
        assert_eq!(
            fake.workspace_transactions
                .load(1, Some(&review.id))
                .unwrap()
                .journal
                .progress
                .phase,
            TransactionPhase::Restored
        );
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn workspace_recovery_cancels_unstarted_operation_without_folder_access_in_both_modes() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let review = persist_create_review(&owner, intelligence).await;
        let context = intelligence.resource_context.unwrap();
        let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
        workspace
            .write("interrupted.rs", "external file after review")
            .await
            .unwrap();
        context.projects.local_handles.set(HashMap::new());
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        let actions = install_workspace_recovery(&owner, intelligence);
        let state = intelligence.editor.workspace;
        wait("unstarted operation discovered", || {
            matches!(
                state.workspace_recovery.get_untracked().get(&1),
                Some(WorkspaceRecoveryStatus::Pending { .. })
            )
        })
        .await;
        actions.recover.run(1);
        wait("unstarted operation cancelled", || {
            state.workspace_recovery.get_untracked().get(&1)
                == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        assert_eq!(
            workspace.read("interrupted.rs").await.unwrap(),
            "external file after review"
        );
        assert_eq!(
            fake.workspace_transactions
                .load(1, Some(&review.id))
                .unwrap()
                .journal
                .progress
                .phase,
            TransactionPhase::Restored
        );
        assert_eq!(state.content.get_untracked().as_str(), "foo");
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn workspace_recovery_transport_failure_blocks_disk_saves_and_retries_in_both_modes() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        fake.workspace_transactions.latest_error.set(true);
        let actions = install_workspace_recovery(&owner, intelligence);
        let state = intelligence.editor.workspace;
        wait("recovery check failure surfaced", || {
            matches!(
                state.workspace_recovery.get_untracked().get(&1),
                Some(WorkspaceRecoveryStatus::Unavailable(_))
            )
        })
        .await;
        assert!(state.begin_editor_save(1).is_none());
        assert!(
            intelligence
                .editor
                .insert_native_text("still editable", 100.0)
                .unwrap()
                .is_some()
        );
        let draft = state.content.get_untracked();
        fake.workspace_transactions.latest_error.set(false);
        actions.check.run(1);
        wait("recovery transport restored", || {
            state.workspace_recovery.get_untracked().get(&1)
                == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        assert!(state.begin_editor_save(1).is_some());
        assert_eq!(state.content.get_untracked(), draft);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn workspace_recovery_late_discovery_cannot_cross_accounts_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        let review = persist_create_review(&owner, intelligence).await;
        let context = intelligence.resource_context.unwrap();
        fake.workspace_transactions.latest_delay_ms.set(100);
        install_workspace_recovery(&owner, intelligence);
        wait("delayed discovery dispatched", || {
            fake.workspace_transactions.latest_reads.get() > 0
        })
        .await;
        context.auth.user.set(None);
        context
            .auth
            .generation
            .update(|generation| *generation += 1);
        crate::util::sleep_ms(200).await;
        assert!(
            intelligence
                .editor
                .workspace
                .workspace_recovery
                .get_untracked()
                .is_empty()
        );
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn late_discovery_cannot_release_a_new_workspace_or_command_fence_in_both_modes() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    use openwebide_core::plugins::languages::intelligence::actions::ActionPhase;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for expected in [
            WorkspaceRecoveryStatus::Pending {
                id: "a".repeat(32),
                phase: TransactionPhase::Applying,
                recovering: false,
                error: None,
            },
            WorkspaceRecoveryStatus::LanguageAction {
                id: "b".repeat(32),
                title: "New command".into(),
                phase: ActionPhase::Running,
                recovering: false,
                error: None,
            },
        ] {
            let (owner, intelligence, fake, folder) = fixture(mode).await;
            fake.workspace_transactions.latest_delay_ms.set(100);
            install_workspace_recovery(&owner, intelligence);
            wait("old discovery in flight", || {
                fake.workspace_transactions.latest_reads.get() > 0
            })
            .await;
            let state = intelligence.editor.workspace;
            let lock = state.begin_workspace_edit(1).unwrap();
            state.workspace_recovery.update(|states| {
                states.insert(1, expected.clone());
            });
            drop(lock);
            crate::util::sleep_ms(200).await;
            assert_eq!(
                state.workspace_recovery.get_untracked().get(&1),
                Some(&expected)
            );
            assert!(state.begin_editor_save(1).is_none());
            drop(owner);
            if let Some(folder) = folder {
                resourceTestCleanup(&folder).await;
            }
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn workspace_recovery_commit_conflict_requires_explicit_editor_choice_in_both_modes() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, intelligence, fake, folder) = fixture(mode).await;
        prepare(
            &owner,
            intelligence,
            WorkspaceEdit {
                files: vec![],
                changes: vec![WorkspaceChange::File {
                    operation: FileOperation::Rename {
                        from: "first.rs".into(),
                        to: "moved.rs".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                }],
            },
        )
        .await;
        wait("commit-conflict review prepared", || {
            intelligence.resources.get_untracked().is_some()
        })
        .await;
        let review = intelligence.resources.get_untracked().unwrap();
        owner.with(|| {
            intelligence.apply_resource_workspace(review.clone(), Signal::derive(|| false));
        });
        wait("operation committed before publication conflict", || {
            !intelligence.busy.get_untracked()
        })
        .await;
        assert_eq!(intelligence.error.get_untracked(), None);
        let context = intelligence.resource_context.unwrap();
        let state = intelligence.editor.workspace;
        let actions = install_workspace_recovery(&owner, intelligence);
        actions.check.run(1);
        wait("committed journal checked", || {
            state.workspace_recovery.get_untracked().get(&1)
                == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        // Reproduce an original window whose editor did not publish the commit.
        let documents = review
            .draft
            .before_documents
            .iter()
            .map(|(path, document)| ((1, path.clone()), document.document.clone()))
            .collect();
        let old = PreparedEditorRecovery::from_documents(1, &review.before, documents);
        state.install_editor_recovery(&review.project, &review.before, old);
        assert_eq!(state.open_file.get_untracked().as_deref(), Some("first.rs"));
        state.workspace_recovery.update(|states| {
            states.insert(
                1,
                WorkspaceRecoveryStatus::Pending {
                    id: review.id.clone(),
                    phase: TransactionPhase::Applying,
                    recovering: false,
                    error: None,
                },
            );
        });
        actions.recover.run(1);
        wait(
            "late commit requires editor review instead of rollback",
            || {
                matches!(
                    state.workspace_recovery.get_untracked().get(&1),
                    Some(WorkspaceRecoveryStatus::EditorConflict(_))
                )
            },
        )
        .await;
        let reads = fake.workspace_transactions.latest_reads.get();
        actions.check.run(1);
        wait("terminal journal rechecked", || {
            fake.workspace_transactions.latest_reads.get() > reads
        })
        .await;
        crate::util::sleep_ms(30).await;
        assert!(matches!(
            state.workspace_recovery.get_untracked().get(&1),
            Some(WorkspaceRecoveryStatus::EditorConflict(_))
        ));
        assert!(state.begin_editor_save(1).is_none());
        assert_eq!(
            fake.workspace_transactions
                .load(1, Some(&review.id))
                .unwrap()
                .journal
                .progress
                .phase,
            TransactionPhase::Committed
        );
        context.recovery.restore.run(1);
        wait("explicit restore confirmation", || {
            context.ui.confirm.get_untracked().is_some()
        })
        .await;
        let choice = context.ui.confirm.get_untracked().unwrap().action;
        context.ui.confirm.set(None);
        choice.run(());
        wait("saved editor conflict resolved", || {
            context.recovery.ready(1)
                && state.workspace_recovery.get_untracked().get(&1)
                    == Some(&WorkspaceRecoveryStatus::Ready)
        })
        .await;
        assert_eq!(state.open_file.get_untracked().as_deref(), Some("moved.rs"));
        assert!(state.begin_editor_save(1).is_some());
        crate::idb::delete_transaction_root(1, 1, &review.id)
            .await
            .unwrap();
        drop(review);
        drop(owner);
        if let Some(folder) = folder {
            resourceTestCleanup(&folder).await;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn lost_browser_recovery_binding_requires_confirmation_and_rechecks_contents_and_cursor() {
    use crate::state::workspace::WorkspaceRecoveryStatus;
    use openwebide_core::workspace_transaction::{
        AdvanceWorkspaceTransaction, TransactionProgress,
    };
    let (owner, intelligence, fake, folder) = fixture(WorkspaceMode::Local).await;
    let review = persist_create_review(&owner, intelligence).await;
    let context = intelligence.resource_context.unwrap();
    let backend = context.api.with_value(Clone::clone);
    let workspace = Workspace::for_project(context.api, context.projects, 1).unwrap();
    for (revision, active) in [(1, false), (2, true)] {
        backend
            .advance_workspace_transaction(
                1,
                &AdvanceWorkspaceTransaction {
                    id: review.id.clone(),
                    generation: context
                        .api
                        .with_value(Clone::clone)
                        .workspace_transaction(1, Some(&review.id))
                        .await
                        .unwrap()
                        .unwrap()
                        .journal
                        .generation,
                    revision,
                    progress: TransactionProgress {
                        phase: TransactionPhase::Applying,
                        completed: 0,
                        active,
                        rollback_from: None,
                    },
                },
            )
            .await
            .unwrap();
    }
    workspace.write("interrupted.rs", "").await.unwrap();
    crate::idb::delete_transaction_root(1, 1, &review.id)
        .await
        .unwrap();
    let actions = install_workspace_recovery(&owner, intelligence);
    let original_handle = context
        .projects
        .local_handles
        .with_untracked(|handles| handles[&1].clone());
    wait("missing capability discovered", || {
        actions
            .missing_folders
            .with_untracked(|ids| ids.contains(&1))
    })
    .await;
    let wrong = resourceTestFolder().await;
    let wrong_workspace = Workspace::Local {
        handle: resourceTestHandle(&wrong).unchecked_into(),
    };
    wrong_workspace
        .write("interrupted.rs", "unrelated contents")
        .await
        .unwrap();
    let restore = resourceTestPicker(&resourceTestHandle(&wrong));
    actions.choose_folder.run(1);
    wait("different contents refused before confirmation", || {
        context.ui.toast.get_untracked().is_some()
    })
    .await;
    assert!(context.ui.confirm.get_untracked().is_none());
    assert!(
        crate::idb::load_transaction_root(1, 1, &review.id)
            .await
            .unwrap()
            .is_none()
    );
    restore.call0(&wasm_bindgen::JsValue::UNDEFINED).unwrap();
    let restore = resourceTestPicker(&resourceTestHandle(folder.as_ref().unwrap()));
    for change in ["cancel", "contents", "cursor", "confirm"] {
        context.ui.toast.set(None);
        actions.choose_folder.run(1);
        wait("explicit original-folder confirmation", || {
            context.ui.confirm.get_untracked().is_some()
        })
        .await;
        assert!(
            crate::idb::load_transaction_root(1, 1, &review.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(workspace.read("interrupted.rs").await.unwrap(), "");
        let approval = context.ui.confirm.get_untracked().unwrap().action;
        context.ui.confirm.set(None);
        if change == "cancel" {
            continue;
        }
        if change == "contents" {
            workspace
                .write("interrupted.rs", "external edit after folder review")
                .await
                .unwrap();
        }
        if change == "cursor" {
            let current = fake
                .workspace_transactions
                .load(1, Some(&review.id))
                .unwrap();
            backend
                .advance_workspace_transaction(
                    1,
                    &AdvanceWorkspaceTransaction {
                        id: review.id.clone(),
                        generation: context
                            .api
                            .with_value(Clone::clone)
                            .workspace_transaction(1, Some(&review.id))
                            .await
                            .unwrap()
                            .unwrap()
                            .journal
                            .generation,
                        revision: current.journal.revision,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Applying,
                            completed: 1,
                            active: false,
                            rollback_from: None,
                        },
                    },
                )
                .await
                .unwrap();
        }
        approval.run(());
        if change == "confirm" {
            wait("explicit capability retained", || {
                !actions
                    .missing_folders
                    .with_untracked(|ids| ids.contains(&1))
                    && actions.folder_access.with_untracked(|ids| ids.contains(&1))
            })
            .await;
            assert!(
                crate::idb::load_transaction_root(1, 1, &review.id)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                context
                    .projects
                    .local_handles
                    .with_untracked(|handles| handles[&1].clone()),
                original_handle
            );
            assert_eq!(workspace.read("interrupted.rs").await.unwrap(), "");
        } else {
            wait("changed grant review rejected", || {
                context.ui.toast.get_untracked().is_some()
            })
            .await;
            assert!(
                crate::idb::load_transaction_root(1, 1, &review.id)
                    .await
                    .unwrap()
                    .is_none()
            );
            if change == "contents" {
                assert_eq!(
                    workspace.read("interrupted.rs").await.unwrap(),
                    "external edit after folder review"
                );
                workspace.write("interrupted.rs", "").await.unwrap();
            }
        }
    }
    assert!(intelligence.editor.workspace.begin_editor_save(1).is_none());
    actions.recover.run(1);
    wait("regranted operation restored", || {
        intelligence
            .editor
            .workspace
            .workspace_recovery
            .get_untracked()
            .get(&1)
            == Some(&WorkspaceRecoveryStatus::Ready)
    })
    .await;
    assert!(workspace.read("interrupted.rs").await.is_err());
    assert_eq!(
        intelligence
            .editor
            .workspace
            .content
            .get_untracked()
            .as_str(),
        "foo"
    );
    restore.call0(&wasm_bindgen::JsValue::UNDEFINED).unwrap();
    crate::idb::delete_handle(1).await.unwrap();
    drop(review);
    drop(owner);
    resourceTestCleanup(&wrong).await;
    resourceTestCleanup(&folder.unwrap()).await;
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn lost_browser_folder_confirmation_cannot_grant_after_account_changes() {
    use openwebide_core::workspace_transaction::{
        AdvanceWorkspaceTransaction, TransactionProgress,
    };
    let (owner, intelligence, _, folder) = fixture(WorkspaceMode::Local).await;
    let review = persist_create_review(&owner, intelligence).await;
    let context = intelligence.resource_context.unwrap();
    context
        .api
        .with_value(Clone::clone)
        .advance_workspace_transaction(
            1,
            &AdvanceWorkspaceTransaction {
                id: review.id.clone(),
                generation: context
                    .api
                    .with_value(Clone::clone)
                    .workspace_transaction(1, Some(&review.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .journal
                    .generation,
                revision: 1,
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..TransactionProgress::default()
                },
            },
        )
        .await
        .unwrap();
    crate::idb::delete_transaction_root(1, 1, &review.id)
        .await
        .unwrap();
    let actions = install_workspace_recovery(&owner, intelligence);
    wait("missing folder before account change", || {
        actions
            .missing_folders
            .with_untracked(|ids| ids.contains(&1))
    })
    .await;
    let restore = resourceTestPicker(&resourceTestHandle(folder.as_ref().unwrap()));
    actions.choose_folder.run(1);
    wait("folder confirmation before account change", || {
        context.ui.confirm.get_untracked().is_some()
    })
    .await;
    let approval = context.ui.confirm.get_untracked().unwrap().action;
    context.ui.confirm.set(None);
    context.auth.user.set(None);
    context
        .auth
        .generation
        .update(|generation| *generation += 1);
    approval.run(());
    crate::util::sleep_ms(30).await;
    assert!(
        crate::idb::load_transaction_root(1, 1, &review.id)
            .await
            .unwrap()
            .is_none()
    );
    restore.call0(&wasm_bindgen::JsValue::UNDEFINED).unwrap();
    drop(review);
    drop(owner);
    resourceTestCleanup(&folder.unwrap()).await;
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn lost_browser_access_review_rejects_a_new_handle_while_an_operation_owns_the_project() {
    let (owner, intelligence, _, folder) = fixture(WorkspaceMode::Local).await;
    let context = intelligence.resource_context.unwrap();
    let project = context.projects.project(1).unwrap();
    let account = context.auth.generation.get_untracked();
    let granted = EditorRoot::for_project(&project, context.projects, Some(context.settings));
    let handle = context
        .projects
        .local_handles
        .with_untracked(|handles| handles[&1].clone());
    context.projects.local_handles.set(HashMap::new());
    let missing = EditorRoot::for_project(&project, context.projects, Some(context.settings));
    let lock = intelligence
        .editor
        .workspace
        .begin_workspace_edit(1)
        .unwrap();
    assert!(context.root_current(&project, &missing, account));
    context.projects.local_handles.update(|handles| {
        handles.insert(1, handle);
    });
    assert!(!context.root_current(&project, &missing, account));
    assert!(context.root_current(&project, &granted, account));
    drop(lock);
    drop(owner);
    resourceTestCleanup(&folder.unwrap()).await;
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn ordinary_remote_ui_review_uses_native_plan_and_same_reader_for_unopened_text() {
    let (owner, mut intelligence, fake, folder) = fixture(WorkspaceMode::Remote).await;
    assert!(folder.is_none());
    intelligence
        .resource_context
        .as_mut()
        .unwrap()
        .legacy_review = false;
    fake.files
        .borrow_mut()
        .insert((1, "unopened.rs".into()), "unopened".into());
    prepare(
        &owner,
        intelligence,
        WorkspaceEdit {
            files: vec![],
            changes: vec![
                WorkspaceChange::File {
                    operation: FileOperation::Rename {
                        from: "second.rs".into(),
                        to: "moved.rs".into(),
                        overwrite: false,
                        ignore_if_exists: false,
                    },
                },
                text("unopened.rs", "unopened", "unsaved change"),
            ],
        },
    )
    .await;
    wait("ordinary native review", || {
        intelligence.resources.get_untracked().is_some()
            || intelligence.error.get_untracked().is_some()
    })
    .await;
    assert_eq!(intelligence.error.get_untracked(), None);
    let review = intelligence.resources.get_untracked().unwrap();
    assert_eq!(review.draft.plan.version, 2);
    assert_eq!(
        review.draft.plan.root,
        crate::testing::transactions::identity(1)
    );
    assert_eq!(
        review.draft.documents["unopened.rs"].document.text(),
        "unsaved change"
    );
    assert!(fake.workspace_transactions.inspections.borrow().iter().any(|request| matches!(&request.action, openwebide_core::workspace_transaction::companion::FileAction::Read {path} if path == "unopened.rs")));
    assert!(fake.workspace_transactions.records.borrow().is_empty());
    assert_eq!(fake.files.borrow()[&(1, "second.rs".into())], "foo");
    assert_eq!(fake.files.borrow()[&(1, "unopened.rs".into())], "unopened");
    drop(review);
    owner.cleanup();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn ordinary_local_ui_review_requires_a_native_host_without_legacy_fallback() {
    let (owner, mut intelligence, fake, folder) = fixture(WorkspaceMode::Local).await;
    intelligence
        .resource_context
        .as_mut()
        .unwrap()
        .legacy_review = false;
    prepare(
        &owner,
        intelligence,
        WorkspaceEdit {
            files: vec![],
            changes: vec![WorkspaceChange::File {
                operation: FileOperation::Create {
                    path: "new.rs".into(),
                    overwrite: false,
                    ignore_if_exists: false,
                },
            }],
        },
    )
    .await;
    wait("missing native review host", || {
        intelligence.error.get_untracked().is_some()
    })
    .await;
    assert!(
        intelligence
            .error
            .get_untracked()
            .unwrap()
            .contains("paired execution host")
    );
    assert!(intelligence.resources.get_untracked().is_none());
    assert!(fake.workspace_transactions.records.borrow().is_empty());
    drop(owner);
    resourceTestCleanup(&folder.unwrap()).await;
}
