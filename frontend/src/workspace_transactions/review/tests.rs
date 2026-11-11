use super::*;
use crate::{
    backend::{Api, Backend},
    testing::fake_backend::FakeBackend,
    workspace::Workspace,
};
use leptos::prelude::*;
use openwebide_core::{
    WorkspaceMode,
    editor::EditorRecoveryRoot,
    workspace_transaction::{
        EditorRecoveryPatch, JournalRecord, TransactionPhase, TransactionProgress, WorkspacePlan,
        WorkspaceTransactionState,
    },
};
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};

#[wasm_bindgen(inline_js = r#"
export async function nativeReviewTestFolder() {
 const parent=await navigator.storage.getDirectory(), name='native-review-'+crypto.randomUUID();
 return {parent,name,handle:await parent.getDirectoryHandle(name,{create:true})};
}
export function nativeReviewTestHandle(folder) {return folder.handle;}
export async function nativeReviewTestCleanup(folder) {await folder.parent.removeEntry(folder.name,{recursive:true});}
"#)]
extern "C" {
    async fn nativeReviewTestFolder() -> wasm_bindgen::JsValue;
    fn nativeReviewTestHandle(folder: &wasm_bindgen::JsValue) -> wasm_bindgen::JsValue;
    async fn nativeReviewTestCleanup(folder: &wasm_bindgen::JsValue);
}
fn setup() -> (Owner, Rc<FakeBackend>, Api, WorkspaceTransactionExecutor) {
    let owner = Owner::new();
    let fake = Rc::new(FakeBackend::default());
    let api: Api = owner.with(|| StoredValue::new_local(fake.clone() as Rc<dyn Backend>));
    let executor = WorkspaceTransactionExecutor::select(
        fake.clone(),
        Workspace::Remote { api, project_id: 1 },
        1,
        1,
    )
    .unwrap();
    (owner, fake, api, executor)
}
fn record(id: &str, phase: TransactionPhase) -> WorkspaceTransactionState {
    WorkspaceTransactionState {
        language_review: None,
        journal: JournalRecord {
            id: id.into(),
            generation: "c".repeat(32),
            revision: 1,
            plan: WorkspacePlan {
                version: 1,
                root: "legacy".into(),
                operations: vec![],
                roots: vec![],
                before: BTreeMap::new(),
                after: BTreeMap::new(),
            },
            progress: TransactionProgress {
                phase,
                ..TransactionProgress::default()
            },
        },
        editor: EditorRecoveryPatch {
            revision: 0,
            root: EditorRecoveryRoot {
                mode: WorkspaceMode::Remote,
                path: Some("fixture".into()),
            },
            before: vec![],
            after: vec![],
            order_before: vec![],
            order_after: vec![],
            selected_before: None,
            selected_after: None,
        },
        editor_revision: 0,
    }
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn normal_remote_reader_prepares_native_plan_and_loads_unopened_text_without_fallback() {
    let (owner, fake, _api, executor) = setup();
    fake.files
        .borrow_mut()
        .insert((1, "original.rs".into()), "original".into());
    fake.files
        .borrow_mut()
        .insert((1, "unopened.rs".into()), "unopened".into());
    let files = executor
        .review_files("a".repeat(32), Rc::new(|| true))
        .await
        .unwrap();
    let operation = FileOperation::Rename {
        from: "original.rs".into(),
        to: "moved.rs".into(),
        overwrite: false,
        ignore_if_exists: false,
    };
    let mut draft = files
        .prepare(
            vec![operation.clone()],
            BTreeMap::new(),
            vec![],
            None,
            || true,
        )
        .await
        .unwrap();
    draft.resource(operation).unwrap();
    assert_eq!(
        draft
            .document(&files, "unopened.rs", || true)
            .await
            .unwrap()
            .document
            .text(),
        "unopened"
    );
    draft.finish().unwrap();
    assert_eq!(draft.plan.version, 2);
    assert_eq!(draft.plan.root, crate::testing::transactions::identity(1));
    assert_eq!(
        draft.plan.after["moved.rs"].bytes(),
        Some(b"original".as_slice())
    );
    assert!(fake.workspace_transactions.inspections.borrow().iter().any(|request| matches!(&request.action, openwebide_core::workspace_transaction::companion::FileAction::Read {path} if path=="unopened.rs")));
    let reads = fake.workspace_transactions.inspections.borrow().len();
    assert!(files.create("new", VfsEntryKind::File).await.is_err());
    assert!(files.write_bytes("original.rs", b"changed").await.is_err());
    assert!(files.delete("original.rs").await.is_err());
    assert_eq!(
        fake.workspace_transactions.inspections.borrow().len(),
        reads
    );
    fake.workspace_transactions.inspection_error.set(true);
    assert!(
        files
            .read_bytes("original.rs")
            .await
            .unwrap_err()
            .contains("unavailable")
    );
    files.close().await;
    assert!(fake.workspace_transactions.records.borrow().is_empty());
    assert_eq!(fake.files.borrow()[&(1, "original.rs".into())], "original");
    owner.cleanup();
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn local_review_without_native_host_never_captures_a_legacy_binding() {
    let (owner, fake, _api, _) = setup();
    let folder = nativeReviewTestFolder().await;
    let handle = nativeReviewTestHandle(&folder).unchecked_into();
    let id = "b".repeat(32);
    let executor =
        WorkspaceTransactionExecutor::select(fake.clone(), Workspace::Local { handle }, 1, 1)
            .unwrap();
    let error = executor
        .review_files(id.clone(), Rc::new(|| true))
        .await
        .err()
        .unwrap();
    assert!(error.contains("paired execution host"), "{error}");
    assert!(
        crate::idb::load_transaction_root(1, 1, &id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fake.workspace_transactions.inspections.borrow().is_empty());
    nativeReviewTestCleanup(&folder).await;
    owner.cleanup();
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn fresh_journal_inspection_refuses_existing_ids_pending_work_and_failed_reads() {
    let (owner, fake, _api, executor) = setup();
    let existing = "c".repeat(32);
    fake.workspace_transactions.records.borrow_mut().insert(
        (1, existing.clone()),
        record(&existing, TransactionPhase::Committed),
    );
    assert!(
        executor
            .review_files(existing, Rc::new(|| true))
            .await
            .err()
            .unwrap()
            .contains("already has a journal")
    );
    let pending = "d".repeat(32);
    fake.workspace_transactions.records.borrow_mut().insert(
        (1, pending.clone()),
        record(&pending, TransactionPhase::Finalizing),
    );
    assert!(
        executor
            .review_files("e".repeat(32), Rc::new(|| true))
            .await
            .err()
            .unwrap()
            .contains("pending workspace operation")
    );
    fake.workspace_transactions.records.borrow_mut().clear();
    fake.workspace_transactions.latest_error.set(true);
    assert!(
        executor
            .review_files("e".repeat(32), Rc::new(|| true))
            .await
            .is_err()
    );
    assert!(fake.workspace_transactions.inspections.borrow().is_empty());
    fake.workspace_transactions.latest_error.set(false);
    fake.workspace_transactions.command_journal_error.set(true);
    let error = executor
        .review_files("e".repeat(32), Rc::new(|| true))
        .await
        .err()
        .unwrap();
    assert!(error.contains("Workspace activity transport unavailable"));
    assert!(fake.workspace_transactions.inspections.borrow().is_empty());
    owner.cleanup();
}

mod companion;

#[wasm_bindgen_test::wasm_bindgen_test]
async fn queued_local_review_reads_fresh_journal_state_after_acquiring_the_workspace_lock() {
    use futures::{FutureExt, channel::oneshot};
    let (owner, fake, _api, _) = setup();
    let folder = nativeReviewTestFolder().await;
    let handle = nativeReviewTestHandle(&folder).unchecked_into();
    let executor =
        WorkspaceTransactionExecutor::select(fake.clone(), Workspace::Local { handle }, 1, 1)
            .unwrap();
    let (entered, acquired) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let held = super::with_workspace_lock(
        crate::workspace_transactions::workspace_lock_name(1),
        async move {
            entered.send(()).unwrap();
            released.await.map_err(|_| "Lost test lock".to_string())
        },
    );
    let mut held = Box::pin(held);
    assert!(held.as_mut().now_or_never().is_none());
    acquired.await.unwrap();
    let mut review = Box::pin(executor.review_files("a".repeat(32), Rc::new(|| true)));
    assert!(review.as_mut().now_or_never().is_none());
    crate::util::sleep_ms(20).await;
    assert_eq!(fake.workspace_transactions.latest_reads.get(), 0);
    let pending = "b".repeat(32);
    fake.workspace_transactions.records.borrow_mut().insert(
        (1, pending.clone()),
        record(&pending, TransactionPhase::Finalizing),
    );
    release.send(()).unwrap();
    held.await.unwrap();
    let error = review.await.err().unwrap();
    assert!(error.contains("pending workspace operation"), "{error}");
    assert_eq!(fake.workspace_transactions.latest_reads.get(), 1);
    nativeReviewTestCleanup(&folder).await;
    owner.cleanup();
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn queued_execution_never_recovers_a_replacement_with_the_same_id_and_revision_in_both_modes()
{
    use futures::{FutureExt, channel::oneshot};
    use openwebide_core::workspace_transaction::{
        BeginWorkspaceTransaction, WorkspaceTransactionAction,
    };
    crate::testing::initialize_runtime();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, fake, _api, remote) = setup();
        let folder = nativeReviewTestFolder().await;
        let executor = if mode == WorkspaceMode::Local {
            WorkspaceTransactionExecutor::select(
                fake.clone(),
                Workspace::Local {
                    handle: nativeReviewTestHandle(&folder).unchecked_into(),
                },
                1,
                1,
            )
            .unwrap()
        } else {
            remote
        };
        let id = "a".repeat(32);
        let mut state = record(&id, TransactionPhase::Prepared);
        state.journal.plan.root = executor.root_identity(&id).await.unwrap();
        state.editor.root.mode = mode;
        state.editor.root.path = (mode == WorkspaceMode::Remote).then(|| "fixture".into());
        let input = BeginWorkspaceTransaction {
            language_review: None,
            id: id.clone(),
            plan: state.journal.plan.clone(),
            editor: state.editor.clone(),
        };
        fake.workspace_transactions
            .records
            .borrow_mut()
            .insert((1, id.clone()), state);
        let original = executor.prepare(&input).await.unwrap();
        let (entered, acquired) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let mut held = Box::pin(super::with_workspace_lock(
            crate::workspace_transactions::workspace_lock_name(1),
            async move {
                entered.send(()).unwrap();
                released.await.map_err(|_| "Lost test lock".to_string())
            },
        ));
        assert!(held.as_mut().now_or_never().is_none());
        acquired.await.unwrap();
        let mut execution = Box::pin(executor.execute(
            input.clone(),
            original.generation.clone(),
            WorkspaceTransactionAction::Recover,
            Rc::new(|| true),
        ));
        assert!(execution.as_mut().now_or_never().is_none());
        crate::util::sleep_ms(20).await;
        // Model another host replacing a retired row while this window waits.
        fake.workspace_transactions
            .records
            .borrow_mut()
            .remove(&(1, id.clone()));
        let replacement = executor.prepare(&input).await.unwrap();
        assert_ne!(replacement.generation, original.generation);
        assert_eq!(replacement.id, original.id);
        assert_eq!(replacement.revision, original.revision);
        let unchanged = fake.workspace_transactions.load(1, Some(&id)).unwrap();
        release.send(()).unwrap();
        held.await.unwrap();
        let error = execution.await.unwrap_err();
        assert!(error.contains("another reviewed operation"), "{error}");
        let current = fake.workspace_transactions.load(1, Some(&id)).unwrap();
        assert_eq!(current.journal, unchanged.journal);
        assert_eq!(current.editor, unchanged.editor);
        assert!(fake.editor_recovery_records.borrow().is_empty());
        crate::idb::delete_transaction_root(1, 1, &id)
            .await
            .unwrap();
        nativeReviewTestCleanup(&folder).await;
        owner.cleanup();
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn retained_language_authority_is_fresh_manual_and_only_requested_for_initial_apply_in_both_modes()
 {
    use openwebide_core::{
        plugins::{ProjectPlugin, languages::intelligence as protocol},
        workspace_transaction::{
            BeginWorkspaceTransaction, LanguageReview, WorkspaceTransactionAction,
        },
    };
    for mode in [WorkspaceMode::Remote, WorkspaceMode::Local] {
        let owner = Owner::new();
        let fake = Rc::new(FakeBackend::default());
        let mut state = record(&"a".repeat(32), TransactionPhase::Prepared);
        state.journal.plan.version = 2;
        state.journal.plan.root = "b".repeat(64);
        state.editor.root.mode = mode;
        state.editor.root.path = (mode == WorkspaceMode::Remote).then(|| "fixture".into());
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([{"id":"custom","name":"Custom","extensions":["custom"],"features":{"languageServer":{"command":"custom-lsp"}}}])).unwrap();
        fake.project_plugin_entries.borrow_mut().insert(
            1,
            vec![ProjectPlugin {
                id: 1,
                revision: 1,
                prepared: prepared.clone(),
                enabled: true,
            }],
        );
        let result = serde_json::json!({"changes":[]});
        state.language_review = Some(Box::new(LanguageReview {
            prepared,
            binding_revision: 1,
            input: protocol::Request {
                version: protocol::VERSION,
                client: "window".into(),
                document: "document".into(),
                request: 1,
                revision: 1,
                language: "custom".into(),
                path: "main.custom".into(),
                source: String::new(),
                source_digest: openwebide_core::plugins::languages::source_digest(""),
                query: protocol::Query::Rename {
                    position: Default::default(),
                    new_name: "next".into(),
                },
            },
            context: protocol::AnalysisContext {
                files: vec![],
                identities: vec![],
                revisions: vec![],
                server: "original".into(),
                proof: "opaque.signature".into(),
                result_digest: openwebide_core::plugins::content_digest(
                    &serde_json::to_vec(&result).unwrap(),
                ),
            },
            result,
            transaction_digest: LanguageReview::transaction_digest(
                &state.journal.plan,
                &state.editor,
            )
            .unwrap(),
        }));
        let input = BeginWorkspaceTransaction {
            id: state.journal.id.clone(),
            plan: state.journal.plan.clone(),
            editor: state.editor.clone(),
            language_review: state.language_review.clone(),
        };
        fake.workspace_transactions
            .records
            .borrow_mut()
            .insert((1, input.id.clone()), state.clone());
        let current: Rc<dyn Fn() -> bool> = Rc::new(|| true);
        let (_, _, grant) = crate::workspace_transactions::reviewed_record(
            fake.clone(),
            1,
            &input,
            &state.journal.generation,
            WorkspaceTransactionAction::Apply,
            &current,
        )
        .await
        .unwrap();
        let grant = grant.unwrap();
        assert!(openwebide_core::workspace_transaction::valid_id(&grant));
        let authority = fake
            .plugin_authorities
            .borrow()
            .get(&grant)
            .unwrap()
            .clone();
        assert_eq!(authority.0.project_id, Some(1));
        assert_eq!(authority.0.session_id, None);
        assert!(authority.0.user_action);
        assert_eq!(
            authority.1,
            input.language_review.as_ref().unwrap().prepared
        );
        fake.project_plugin_entries.borrow_mut().clear();
        assert!(
            crate::workspace_transactions::reviewed_record(
                fake.clone(),
                1,
                &input,
                &state.journal.generation,
                WorkspaceTransactionAction::Apply,
                &current
            )
            .await
            .is_err()
        );
        for (action, phase) in [
            (
                WorkspaceTransactionAction::Recover,
                TransactionPhase::Applying,
            ),
            (
                WorkspaceTransactionAction::Apply,
                TransactionPhase::Committed,
            ),
        ] {
            state.journal.progress.phase = phase;
            fake.workspace_transactions
                .records
                .borrow_mut()
                .insert((1, input.id.clone()), state.clone());
            let (_, _, grant) = crate::workspace_transactions::reviewed_record(
                fake.clone(),
                1,
                &input,
                &state.journal.generation,
                action,
                &current,
            )
            .await
            .unwrap();
            assert!(grant.is_none());
        }
        assert_eq!(fake.plugin_authorities.borrow().len(), 1);
        assert!(fake.language_inputs.borrow().is_empty());
        owner.cleanup();
    }
}
