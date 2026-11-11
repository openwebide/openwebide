use super::*;
use crate::{MemoryVfs, Vfs, vfs::VfsEntryKind, workspace_transaction::VfsTransactionFiles};
use futures::executor::block_on;
fn rename(from: &str, to: &str) -> FileOperation {
    FileOperation::Rename {
        from: from.into(),
        to: to.into(),
        overwrite: false,
        ignore_if_exists: false,
    }
}
fn create(path: &str) -> FileOperation {
    FileOperation::Create {
        path: path.into(),
        overwrite: false,
        ignore_if_exists: false,
    }
}
fn edit(path: &str, source: &str, replacement: &str) -> FileEdit {
    FileEdit {
        path: path.into(),
        source_digest: source_digest(source),
        revision: None,
        edits: vec![super::super::results::TextEdit {
            range: super::super::Range {
                start: super::super::Position::default(),
                end: super::super::position(source, source.len()).unwrap(),
            },
            new_text: replacement.into(),
        }],
    }
}
#[test]
fn ordered_text_and_resource_changes_preserve_drafts_baselines_and_history() {
    block_on(async {
        let vfs = MemoryVfs::new();
        vfs.create("source", VfsEntryKind::Directory).await.unwrap();
        vfs.write("source/a.rs", "disk α\r\n").await.unwrap();
        vfs.write_bytes("source/data", &[0, 255, 128])
            .await
            .unwrap();
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: "root",
        };
        let mut document = Document::new("disk α\r\n");
        document.replace_selections("draft ", None).unwrap();
        let original = document.text().to_string();
        let saved = document.recovery().saved;
        let operations = vec![
            rename("source", "renamed"),
            create("created.rs"),
            rename("created.rs", "final.rs"),
        ];
        let mut draft = WorkspaceDraft::prepare(
            &files,
            "root".into(),
            operations.clone(),
            [(
                "source/a.rs".into(),
                WorkspaceDocument::new("source/a.rs", document),
            )]
            .into(),
            vec!["source/a.rs".into()],
            Some("source/a.rs".into()),
            || true,
        )
        .await
        .unwrap();
        draft
            .text(edit("source/a.rs", &original, "before move 文😀\r\n"))
            .unwrap();
        draft.resource(operations[0].clone()).unwrap();
        draft
            .text(edit(
                "renamed/a.rs",
                "before move 文😀\r\n",
                "after move 文😀\r\n",
            ))
            .unwrap();
        draft.resource(operations[1].clone()).unwrap();
        draft.document(&files, "created.rs", || true).await.unwrap();
        draft.text(edit("created.rs", "", "new draft")).unwrap();
        draft.resource(operations[2].clone()).unwrap();
        draft.finish().unwrap();
        assert_eq!(draft.order, ["renamed/a.rs", "final.rs"]);
        assert_eq!(draft.selected.as_deref(), Some("renamed/a.rs"));
        assert_eq!(draft.original_path("renamed/a.rs"), Some("source/a.rs"));
        assert_eq!(draft.original_path("renamed/data"), Some("source/data"));
        assert_eq!(draft.original_path("final.rs"), None);
        let changed = &mut draft.documents.get_mut("renamed/a.rs").unwrap().document;
        assert_eq!(changed.recovery().saved, saved);
        assert!(changed.is_dirty());
        assert!(changed.undo());
        assert!(changed.undo());
        assert_eq!(changed.text(), original);
        assert!(changed.undo());
        assert_eq!(changed.text(), "disk α\r\n");
        assert_eq!(draft.documents["final.rs"].document.text(), "new draft");
        assert!(
            draft.documents["final.rs"]
                .document
                .recovery()
                .saved
                .is_empty()
        );
        // Preparation never mutates disk or sends an unapproved server operation.
        assert_eq!(vfs.read("source/a.rs").await.unwrap(), "disk α\r\n");
        assert!(vfs.read("renamed/a.rs").await.is_err());
        assert_eq!(
            draft.plan.after["renamed/data"].bytes().unwrap(),
            [0, 255, 128]
        );
    });
}
#[test]
fn ignored_resources_keep_unsaved_documents_and_overwrite_is_explicit() {
    block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write("a", "a").await.unwrap();
        vfs.write("b", "b").await.unwrap();
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: "root",
        };
        let mut document = Document::new("b");
        document.replace_selections("draft ", None).unwrap();
        let ignore = FileOperation::Create {
            path: "b".into(),
            overwrite: false,
            ignore_if_exists: true,
        };
        let mut overwrite = rename("a", "b");
        if let FileOperation::Rename { overwrite, .. } = &mut overwrite {
            *overwrite = true;
        }
        let mut draft = WorkspaceDraft::prepare(
            &files,
            "root".into(),
            vec![ignore.clone(), overwrite.clone()],
            [("b".into(), WorkspaceDocument::new("b", document))].into(),
            vec!["b".into()],
            Some("b".into()),
            || true,
        )
        .await
        .unwrap();
        draft.resource(ignore).unwrap();
        assert_eq!(draft.documents["b"].document.text(), "draft b");
        draft.resource(overwrite).unwrap();
        assert!(!draft.documents.contains_key("b"));
        assert!(draft.order.is_empty());
        assert!(draft.selected.is_none());
        draft.document(&files, "b", || true).await.unwrap();
        assert_eq!(draft.documents["b"].document.text(), "a");
        assert_eq!(draft.original_path("b"), Some("a"));
        draft.finish().unwrap();
    });
}
#[test]
fn deleted_sources_binary_text_stale_digests_and_read_only_changes_are_rejected() {
    block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write_bytes("binary", &[0, 255]).await.unwrap();
        vfs.write("a", "a").await.unwrap();
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: "root",
        };
        let operation = FileOperation::Delete {
            path: "a".into(),
            recursive: false,
            ignore_if_missing: false,
        };
        let mut readonly = WorkspaceDocument::new("a", Document::new("a"));
        readonly.read_only = true;
        let mut draft = WorkspaceDraft::prepare(
            &files,
            "root".into(),
            vec![operation.clone()],
            [("a".into(), readonly)].into(),
            vec!["a".into()],
            None,
            || true,
        )
        .await
        .unwrap();
        assert!(draft.text(edit("a", "a", "new")).is_err());
        assert!(draft.resource(operation.clone()).is_err());
        draft.documents.get_mut("a").unwrap().read_only = false;
        assert!(draft.text(edit("a", "stale", "new")).is_err());
        assert!(draft.document(&files, "binary", || true).await.is_err());
        draft.resource(operation).unwrap();
        assert!(draft.document(&files, "a", || true).await.is_err());
        draft.finish().unwrap();
        assert_eq!(vfs.read("a").await.unwrap(), "a");
    });
}

#[test]
fn untouched_read_only_git_document_does_not_block_other_resource_changes() {
    block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write("a", "a").await.unwrap();
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: "root",
        };
        let mut readonly = WorkspaceDocument::new(".git/config", Document::new("read-only"));
        readonly.read_only = true;
        let operation = rename("a", "b");
        let mut draft = WorkspaceDraft::prepare(
            &files,
            "root".into(),
            vec![operation.clone()],
            [(".git/config".into(), readonly)].into(),
            vec![".git/config".into()],
            Some(".git/config".into()),
            || true,
        )
        .await
        .unwrap();
        draft.resource(operation).unwrap();
        draft.finish().unwrap();
        assert_eq!(draft.documents[".git/config"].document.text(), "read-only");
        assert_eq!(draft.selected.as_deref(), Some(".git/config"));
        assert!(draft.text(edit(".git/config", "read-only", "no")).is_err());
    });
}

#[test]
fn native_review_shares_ordered_text_planning_without_reconstructing_resources() {
    block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write("source/a.rs", "disk α\r\n").await.unwrap();
        vfs.write("unopened.rs", "unopened").await.unwrap();
        let root = "a".repeat(64);
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: &root,
        };
        let operation = rename("source", "target");
        let mut draft = WorkspaceDraft::prepare_native(
            &files,
            root.clone(),
            vec![operation.clone()],
            BTreeMap::new(),
            vec![],
            None,
            || true,
        )
        .await
        .unwrap();
        draft
            .document(&files, "source/a.rs", || true)
            .await
            .unwrap();
        draft
            .text(edit("source/a.rs", "disk α\r\n", "draft 😀\r\n"))
            .unwrap();
        draft.resource(operation).unwrap();
        draft
            .document(&files, "unopened.rs", || true)
            .await
            .unwrap();
        draft
            .text(edit("unopened.rs", "unopened", "other draft"))
            .unwrap();
        draft.finish().unwrap();
        assert_eq!(draft.plan.version, 2);
        assert_eq!(draft.plan.root, root);
        assert_eq!(draft.original_path("target/a.rs"), Some("source/a.rs"));
        assert_eq!(
            draft.documents["target/a.rs"].document.text(),
            "draft 😀\r\n"
        );
        assert_eq!(
            draft.documents["unopened.rs"].document.text(),
            "other draft"
        );
        assert_eq!(vfs.read("source/a.rs").await.unwrap(), "disk α\r\n");
        assert_eq!(
            draft.plan.after["target/a.rs"].bytes(),
            Some("disk α\r\n".as_bytes())
        );
        assert!(
            !resource::ResourceScript::prepare(&draft.plan)
                .unwrap()
                .steps
                .is_empty()
        );
    });
}
