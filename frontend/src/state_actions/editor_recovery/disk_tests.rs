#[wasm_bindgen_test::wasm_bindgen_test]
async fn snapshot_comparison_yields_and_discards_superseded_results_in_both_modes() {
    crate::testing::initialize_runtime();
    use openwebide_core::editor::{
        Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll,
    };
    for mode in [
        openwebide_core::WorkspaceMode::Local,
        openwebide_core::WorkspaceMode::Remote,
    ] {
        let left = EditorRecovery {
            root: Some(EditorRecoveryRoot { mode, path: None }),
            files: vec![EditorRecoveryFile {
                path: "a.rs".into(),
                document: Some(Document::new("文😀\r\n".repeat(10_000)).recovery()),
                scroll: RecoveryScroll::default(),
                read_only: false,
            }],
            ..EditorRecovery::default()
        };
        let mut right = left.clone();
        let document = right.files[0].document.as_mut().unwrap();
        document.text = std::sync::Arc::new(document.text.as_str().to_owned());
        document.saved = std::sync::Arc::new(document.saved.as_str().to_owned());
        let current = std::cell::Cell::new(true);
        let mut pending = Box::pin(compare_recovery_snapshots(&left, &right, || current.get()));
        assert!(futures::poll!(&mut pending).is_pending());
        current.set(false);
        assert_eq!(pending.await, None);
        assert_eq!(
            compare_recovery_snapshots(&left, &right, || true).await,
            Some(true)
        );
        right.files[0].read_only = true;
        assert_eq!(
            compare_recovery_snapshots(&left, &right, || true).await,
            Some(false)
        );
    }
}

use super::*;
use std::{
    cell::Cell,
    future::Future,
    task::{Context, Poll, Waker},
};

#[wasm_bindgen_test::wasm_bindgen_test]
async fn review_disk_comparison_yields_without_partial_equality_or_stale_results() {
    crate::testing::initialize_runtime();
    let left = "ASCII 文😀\r\n".repeat(8000);
    let right = left.clone();
    let active = Cell::new(true);
    let current = || active.get();
    let mut pending = Box::pin(compare_review_disk(Some(&left), Some(&right), current));
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    active.set(false);
    assert_eq!(pending.await, None);
    active.set(true);
    assert_eq!(
        compare_review_disk(Some(&left), Some(&right), current).await,
        Some(true)
    );
    let mut changed = right;
    changed.replace_range(changed.len() - 2.., "xx");
    assert_eq!(
        compare_review_disk(Some(&left), Some(&changed), current).await,
        Some(false)
    );
    assert_eq!(
        compare_review_disk(None, Some(""), current).await,
        Some(false)
    );
    assert_eq!(compare_review_disk(None, None, current).await, Some(true));
    active.set(false);
    assert_eq!(compare_review_disk(None, None, current).await, None);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn overwrite_comparison_yields_retries_committed_sources_and_rejects_lost_owners() {
    crate::testing::initialize_runtime();
    for change in ["complete", "draft", "saved", "cancel", "remove", "dispose"] {
        let owner = Owner::new();
        let key = (1, "draft.txt".to_string());
        let source = "ASCII 文😀\r\n".repeat(8000);
        let workspace = owner.with(|| {
            let workspace = WorkspaceState::new();
            workspace.editor_documents.update(|documents| {
                documents.insert(
                    key.clone(),
                    openwebide_core::editor::Document::new(source.clone()),
                );
            });
            workspace
        });
        let expected = crate::state::workspace::RecoveryOverwrite {
            draft: source.clone(),
            disk: Some(source.clone()),
        };
        let active = Cell::new(true);
        let current = || active.get() && !workspace.editor_documents.is_disposed();
        let mut pending = Box::pin(approved_recovery_overwrite(
            workspace,
            &key,
            &expected,
            Some(&source),
            &current,
        ));
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        match change {
            "draft" => workspace.editor_documents.update(|documents| {
                documents
                    .get_mut(&key)
                    .unwrap()
                    .replace_selections("edit", None)
                    .unwrap();
            }),
            "saved" => workspace.editor_documents.update(|documents| {
                documents
                    .get_mut(&key)
                    .unwrap()
                    .mark_saved_source(std::sync::Arc::new("other".into()));
            }),
            "cancel" => active.set(false),
            "remove" => workspace.editor_documents.update(|documents| {
                documents.remove(&key);
            }),
            "dispose" => owner.cleanup(),
            _ => {}
        }
        assert_eq!(pending.await, matches!(change, "complete" | "saved"));
        owner.cleanup();
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn strict_disk_decoding_yields_and_rejects_lost_scope_before_publication() {
    crate::testing::initialize_runtime();
    let source = "ASCII 文😀e\u{301}\r\n".repeat(8000);
    let active = Cell::new(true);
    let current = || active.get();
    let mut pending = Box::pin(decode_disk_bytes(
        Some(source.clone().into_bytes()),
        &current,
        "invalid",
    ));
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    active.set(false);
    assert!(pending.await.is_err());
    active.set(true);
    assert_eq!(
        decode_disk_bytes(Some(source.clone().into_bytes()), &current, "invalid").await,
        Ok(Some(source))
    );
    assert_eq!(decode_disk_bytes(None, &current, "invalid").await, Ok(None));
    assert_eq!(
        decode_disk_bytes(Some(vec![0xff]), &current, "invalid").await,
        Err("invalid".into())
    );
    active.set(false);
    assert!(decode_disk_bytes(None, &current, "invalid").await.is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn disk_reconciliation_yields_retries_current_sources_and_discards_lost_owners() {
    crate::testing::initialize_runtime();
    for change in ["complete", "draft", "saved", "remove", "cancel", "dispose"] {
        let owner = Owner::new();
        let key = (1, "disk.txt".to_owned());
        let source = "x\n".repeat(40_000);
        let workspace = owner.with(|| {
            let workspace = WorkspaceState::new();
            workspace.editor_documents.update(|documents| {
                documents.insert(
                    key.clone(),
                    openwebide_core::editor::Document::for_editor(source.clone()).unwrap(),
                );
            });
            workspace
        });
        let active = Cell::new(true);
        let disk = if change == "draft" {
            "external change"
        } else {
            source.as_str()
        };
        let current = || active.get() && !workspace.editor_documents.is_disposed();
        let mut pending = Box::pin(classify_recovery_disk(workspace, &key, Some(disk), current));
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        match change {
            "draft" => workspace.editor_documents.update(|documents| {
                documents
                    .get_mut(&key)
                    .unwrap()
                    .replace_selections("edit", None)
                    .unwrap();
            }),
            "saved" => workspace.editor_documents.update(|documents| {
                documents
                    .get_mut(&key)
                    .unwrap()
                    .mark_saved_source(std::sync::Arc::new("other".into()));
            }),
            "remove" => workspace.editor_documents.update(|documents| {
                documents.remove(&key);
            }),
            "cancel" => active.set(false),
            "dispose" => owner.cleanup(),
            _ => {}
        }
        let result = pending.await.unwrap();
        assert_eq!(
            result,
            match change {
                "complete" => Some(RecoveryDiskState::Current),
                "draft" => Some(RecoveryDiskState::Conflict),
                "saved" => Some(RecoveryDiskState::AlreadySaved),
                _ => None,
            },
            "{change}"
        );
    }
}
