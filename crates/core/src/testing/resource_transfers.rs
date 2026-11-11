use super::*;

/// Run on an empty disposable filesystem which supports native object moves.
/// Adapters must never emulate this primitive with copying and deletion.
pub async fn workspace_resource_transfer_contract(vfs: &impl Vfs) {
    use VfsEntryKind::{Directory, File};
    vfs.create("moves", Directory).await.unwrap();
    vfs.create("moves/target", Directory).await.unwrap();
    vfs.write_bytes("moves/source", &[0, 255, 1]).await.unwrap();
    vfs.write_bytes("moves/occupied", b"preserved")
        .await
        .unwrap();
    vfs.write_bytes("moves/tree/nested/file", &[254, 0, 2])
        .await
        .unwrap();
    for to in [
        "moves/occupied",
        "moves/tree",
        "moves/missing/file",
        "moves/source",
    ] {
        assert!(vfs.move_atomic("moves/source", to, File).await.is_err());
        assert_eq!(vfs.read_bytes("moves/source").await.unwrap(), [0, 255, 1]);
        assert_eq!(
            vfs.read_bytes("moves/occupied").await.unwrap(),
            b"preserved"
        );
        assert_eq!(vfs.list("moves/tree/nested").await.unwrap().len(), 1);
        assert!(
            !vfs.list("moves")
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.name == "missing")
        );
    }
    for (from, to, kind) in [
        ("moves/source", "moves/new", Directory),
        ("moves/tree", "moves/new", File),
        ("moves/missing", "moves/new", File),
        ("moves/source", "../escape", File),
        ("../escape", "moves/new", File),
        ("moves/source", ".spin/file", File),
        ("", "moves/new", Directory),
        ("moves/tree", "", Directory),
    ] {
        assert!(vfs.move_atomic(from, to, kind).await.is_err());
    }
    vfs.move_atomic("moves/source", "moves/target/file", File)
        .await
        .unwrap();
    assert!(vfs.read_bytes("moves/source").await.is_err());
    assert_eq!(
        vfs.read_bytes("moves/target/file").await.unwrap(),
        [0, 255, 1]
    );
    vfs.move_atomic("moves/tree", "moves/target/tree", Directory)
        .await
        .unwrap();
    assert_eq!(
        vfs.read_bytes("moves/target/tree/nested/file")
            .await
            .unwrap(),
        [254, 0, 2]
    );
    assert!(
        !vfs.list("moves")
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.name == "tree")
    );
    vfs.move_atomic("moves/target/tree", "moves/tree", Directory)
        .await
        .unwrap();
    vfs.move_atomic("moves/target/file", "moves/source", File)
        .await
        .unwrap();
    assert_eq!(vfs.read_bytes("moves/source").await.unwrap(), [0, 255, 1]);
    assert_eq!(
        vfs.read_bytes("moves/tree/nested/file").await.unwrap(),
        [254, 0, 2]
    );
    vfs.delete("moves").await.unwrap();
}
