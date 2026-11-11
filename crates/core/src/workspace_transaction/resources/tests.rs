use super::*;

fn plan(before: Snapshot, roots: &[&str], operations: Vec<FileOperation>) -> WorkspacePlan {
    let roots = roots.iter().map(|root| (*root).into()).collect::<Vec<_>>();
    let after = apply_operations(&before, &roots, &operations).unwrap();
    WorkspacePlan {
        version: 1,
        root: "owned-root".into(),
        operations,
        roots,
        before,
        after,
    }
}
fn rename(from: &str, to: &str, overwrite: bool, ignore_if_exists: bool) -> FileOperation {
    FileOperation::Rename {
        from: from.into(),
        to: to.into(),
        overwrite,
        ignore_if_exists,
    }
}
fn create(path: &str, overwrite: bool, ignore_if_exists: bool) -> FileOperation {
    FileOperation::Create {
        path: path.into(),
        overwrite,
        ignore_if_exists,
    }
}
fn transfer(from: ResourceLocation, to: ResourceLocation, kind: VfsEntryKind) -> ResourceStep {
    ResourceStep::Transfer { from, to, kind }
}
fn workspace(path: &str) -> ResourceLocation {
    ResourceLocation::Workspace(path.into())
}

#[test]
fn ordered_resources_retain_overwritten_objects_and_create_parents_once() {
    use VfsEntryKind::{Directory, File};
    let plan = plan(
        Snapshot::from([
            ("from".into(), EntryState::Directory),
            ("from/child".into(), EntryState::file([0, 255, 1])),
            ("destination".into(), EntryState::Directory),
            ("destination/prior".into(), EntryState::file(b"original")),
            ("old".into(), EntryState::file(b"deleted")),
            ("absent".into(), EntryState::Missing),
        ]),
        &["from", "destination", "old", "absent"],
        vec![
            rename("from", "destination", true, true),
            create("destination/new/deep", false, false),
            FileOperation::Delete {
                path: "old".into(),
                recursive: false,
                ignore_if_missing: false,
            },
            create("destination/child", true, true),
            rename(
                "destination/new/deep",
                "destination/new/final",
                false,
                false,
            ),
            FileOperation::Delete {
                path: "absent".into(),
                recursive: true,
                ignore_if_missing: true,
            },
        ],
    );
    let script = ResourceScript::prepare(&plan).unwrap();
    let expected = vec![
        transfer(
            workspace("destination"),
            ResourceLocation::Retained(0),
            Directory,
        ),
        transfer(workspace("from"), workspace("destination"), Directory),
        ResourceStep::Create {
            path: "destination/new".into(),
            kind: Directory,
        },
        ResourceStep::Create {
            path: "destination/new/deep".into(),
            kind: File,
        },
        transfer(workspace("old"), ResourceLocation::Retained(1), File),
        transfer(
            workspace("destination/child"),
            ResourceLocation::Retained(2),
            File,
        ),
        ResourceStep::Create {
            path: "destination/child".into(),
            kind: File,
        },
        transfer(
            workspace("destination/new/deep"),
            workspace("destination/new/final"),
            File,
        ),
    ];
    assert_eq!(script.steps, expected);
    for completed in 0..=expected.len() {
        assert_eq!(
            script.rollback(completed).unwrap(),
            expected[..completed]
                .iter()
                .rev()
                .map(ResourceStep::inverse)
                .collect::<Vec<_>>()
        );
    }
    assert!(script.rollback(expected.len() + 1).is_err());
}

#[test]
fn ignored_and_identity_operations_emit_no_primitive() {
    let plan = plan(
        Snapshot::from([
            ("a".into(), EntryState::file(b"a")),
            ("b".into(), EntryState::file(b"b")),
            ("absent".into(), EntryState::Missing),
        ]),
        &["a", "b", "absent"],
        vec![
            create("a", false, true),
            rename("a", "a", false, false),
            rename("a", "b", false, true),
            FileOperation::Delete {
                path: "absent".into(),
                recursive: false,
                ignore_if_missing: true,
            },
        ],
    );
    assert_eq!(plan.before, plan.after);
    assert!(ResourceScript::prepare(&plan).unwrap().steps.is_empty());
}

#[test]
fn equal_bytes_do_not_erase_object_moves_or_overwrites() {
    let swapped = plan(
        Snapshot::from([
            ("a".into(), EntryState::file(b"same")),
            ("b".into(), EntryState::file(b"same")),
            ("temporary".into(), EntryState::Missing),
        ]),
        &["a", "b", "temporary"],
        vec![
            rename("a", "temporary", false, false),
            rename("b", "a", false, false),
            rename("temporary", "b", false, false),
        ],
    );
    assert_eq!(swapped.before, swapped.after);
    assert_eq!(ResourceScript::prepare(&swapped).unwrap().steps.len(), 3);
    let overwritten = plan(
        Snapshot::from([("empty".into(), EntryState::file(Vec::new()))]),
        &["empty"],
        vec![create("empty", true, true)],
    );
    assert_eq!(overwritten.before, overwritten.after);
    assert_eq!(
        ResourceScript::prepare(&overwritten).unwrap().steps,
        vec![
            transfer(
                workspace("empty"),
                ResourceLocation::Retained(0),
                VfsEntryKind::File
            ),
            ResourceStep::Create {
                path: "empty".into(),
                kind: VfsEntryKind::File
            },
        ]
    );
}

#[test]
fn expanded_parent_steps_and_paths_have_independent_budgets() {
    for (depth, width) in [(32, 1), (3, 500)] {
        let mut before = Snapshot::new();
        let mut roots = Vec::new();
        let mut operations = Vec::new();
        for index in 0..MAX_OPERATIONS {
            let root = format!("root{index}");
            let path = std::iter::once(root.clone())
                .chain((0..depth).map(|_| "x".repeat(width)))
                .collect::<Vec<_>>()
                .join("/");
            before.insert(root.clone(), EntryState::Missing);
            roots.push(root);
            operations.push(create(&path, false, false));
        }
        let roots = roots.iter().map(String::as_str).collect::<Vec<_>>();
        let plan = plan(before, &roots, operations);
        plan.validate().unwrap();
        let error = ResourceScript::prepare(&plan).err().unwrap();
        assert!(error.contains("step or path budget"), "{error}");
    }
}
