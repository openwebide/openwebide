use super::*;
use std::process::Command;

fn fixture() -> PathBuf {
    let home = tempfile::Builder::new()
        .prefix("openwebide-recovery-publication-")
        .tempdir()
        .unwrap()
        .keep();
    Namespace::open(&home).unwrap();
    println!("retained_publication_fixture={}", home.display());
    home
}
fn record(namespace: &Namespace) -> Anchor {
    anchor_record(&namespace.anchor).unwrap()
}
fn update(namespace: &Namespace, stop: Option<&str>) -> Result<(), String> {
    lock(&namespace.anchor, true)?;
    let anchor = record(namespace);
    let parent = root(&namespace.home)?;
    namespace.validate(&parent, &anchor)?;
    let data = bytes(
        &namespace.original_database(&parent, &anchor)?,
        MAX_DATABASE_BYTES,
    )?;
    let (connection, _) = database(&data, &namespace.key)?;
    connection.execute_batch("BEGIN IMMEDIATE; CREATE TABLE IF NOT EXISTS test_owner (id INTEGER PRIMARY KEY, value TEXT); INSERT OR REPLACE INTO test_owner VALUES (1, 'original owner'); UPDATE recovery_revision SET revision=revision+1; COMMIT;").unwrap();
    let data = connection.serialize("main").unwrap();
    let result = namespace.persist(&parent, &anchor, &data, |barrier| {
        if stop.is_some_and(|stop| stop == format!("{barrier:?}")) {
            // Actual process loss, without unwinding, closing the connection or
            // running a simulated recovery callback in the original process.
            std::process::exit(73);
        }
        Ok(())
    });
    namespace.anchor.unlock().unwrap();
    result
}
fn read(home: &Path) -> (u64, Option<String>) {
    let namespace = Namespace::open(home).unwrap();
    let anchor = record(&namespace);
    let data = bytes(
        &namespace
            .original_database(&root(&namespace.home).unwrap(), &anchor)
            .unwrap(),
        MAX_DATABASE_BYTES,
    )
    .unwrap();
    let (connection, revision) = database(&data, &namespace.key).unwrap();
    let owner = connection
        .query_row("SELECT value FROM test_owner WHERE id=1", [], |row| {
            row.get(0)
        })
        .ok();
    (revision, owner)
}
fn child(home: &Path, stop: &str) {
    let output = Command::new(std::env::current_exe().unwrap()).args([
        "--exact", "native_recovery::persistence::tests::process_loss_recovers_only_durable_original_snapshots", "--nocapture",
    ]).env("OPENWEBIDE_TEST_PUBLICATION_HOME", home)
        .env("OPENWEBIDE_TEST_PUBLICATION_STOP", stop).output().unwrap();
    std::fs::write(home.join("child.stdout"), &output.stdout).unwrap();
    std::fs::write(home.join("child.stderr"), &output.stderr).unwrap();
    assert_eq!(
        output.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn process_loss_recovers_only_durable_original_snapshots() {
    if let Some(home) = std::env::var_os("OPENWEBIDE_TEST_PUBLICATION_HOME") {
        let namespace = Namespace::open(Path::new(&home)).unwrap();
        update(
            &namespace,
            Some(&std::env::var("OPENWEBIDE_TEST_PUBLICATION_STOP").unwrap()),
        )
        .unwrap();
        panic!("The selected durability boundary was never reached");
    }
    for barrier in [
        Barrier::Snapshot,
        Barrier::Prepared,
        Barrier::PartialDatabase,
        Barrier::Database,
        Barrier::Committed,
    ] {
        let home = fixture();
        let namespace = Namespace::open(&home).unwrap();
        let db = home.join(DIRECTORY).join(DATABASE);
        let original = same_file::Handle::from_path(&db).unwrap();
        let before = std::fs::read(&db).unwrap();
        child(&home, &format!("{barrier:?}"));
        if barrier == Barrier::PartialDatabase {
            let partial = std::fs::read(&db).unwrap();
            assert_ne!(
                partial, before,
                "Actual original-file rewrite must have begun"
            );
            let head = namespace.head().unwrap().unwrap();
            assert_ne!(
                digest(&partial),
                head.digest,
                "The full snapshot must not yet be installed"
            );
        }
        if matches!(
            barrier,
            Barrier::Prepared | Barrier::PartialDatabase | Barrier::Database
        ) {
            assert!(
                namespace.ensure_current().is_err(),
                "Readers cannot authorize an uncommitted state"
            );
        }
        let (revision, owner) = read(&home);
        if barrier == Barrier::Snapshot {
            assert_eq!((revision, owner), (1, None));
            assert_eq!(std::fs::read(&db).unwrap(), before);
        } else {
            assert_eq!((revision, owner), (2, Some("original owner".into())));
        }
        assert_eq!(same_file::Handle::from_path(db).unwrap(), original);
        namespace.ensure_current().unwrap();
    }
}
#[test]
fn missing_and_identical_head_copies_never_become_a_new_empty_database() {
    let home = fixture();
    let head = home.join(DIRECTORY).join(HEAD);
    let original = home.join("original-head.json");
    let db = home.join(DIRECTORY).join(DATABASE);
    let before = std::fs::read(&db).unwrap();
    std::fs::rename(&head, &original).unwrap();
    assert!(Namespace::open(&home).is_err());
    std::fs::copy(&original, &head).unwrap();
    assert!(Namespace::open(&home).is_err());
    assert_eq!(std::fs::read(&db).unwrap(), before);
    std::fs::rename(&head, home.join("retained-copied-head.json")).unwrap();
    std::fs::rename(original, head).unwrap();
    assert_eq!(read(&home), (1, None));
}
#[test]
fn prepared_recovery_requires_original_snapshot_and_original_database() {
    for candidate in [true, false] {
        let home = fixture();
        child(&home, "Prepared");
        let directory = home.join(DIRECTORY);
        let head: Head =
            serde_json::from_slice(&std::fs::read(directory.join(HEAD)).unwrap()).unwrap();
        let selected = directory.join(if candidate {
            &head.snapshot.name
        } else {
            DATABASE
        });
        let retained = home.join("original-selected-file");
        std::fs::rename(&selected, &retained).unwrap();
        assert!(Namespace::open(&home).is_err());
        std::fs::copy(&retained, &selected).unwrap();
        assert!(Namespace::open(&home).is_err());
        // A failed observation must not rewrite or replace the presented copy.
        assert_eq!(
            std::fs::read(&selected).unwrap(),
            std::fs::read(&retained).unwrap()
        );
        std::fs::rename(&selected, home.join("retained-identical-copy")).unwrap();
        std::fs::rename(retained, selected).unwrap();
        assert_eq!(read(&home), (2, Some("original owner".into())));
    }
}
#[test]
fn old_namespace_migration_retains_original_anchor_and_database() {
    let home = tempfile::Builder::new()
        .prefix("openwebide-recovery-migration-")
        .tempdir()
        .unwrap()
        .keep();
    let folder = Folder::capture_state(&home).unwrap();
    let parent = root(&folder).unwrap();
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let anchor = parent.open_with(ANCHOR, &options).unwrap().into_std();
    Namespace::initialize(&folder, &parent, &anchor, uid).unwrap();
    let db_path = home.join(DIRECTORY).join(DATABASE);
    let db = same_file::Handle::from_path(&db_path).unwrap();
    let anchor_bytes = std::fs::read(home.join(ANCHOR)).unwrap();
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(read(&home), (1, None));
    assert_eq!(same_file::Handle::from_path(db_path).unwrap(), db);
    let sealed = std::fs::read(home.join(ANCHOR)).unwrap();
    assert!(sealed.starts_with(&anchor_bytes));
    assert!(anchor_record(&anchor).unwrap().sealed);
    println!("retained_migration_fixture={}", home.display());
}
#[test]
fn repeated_updates_are_monotonic_and_exclude_other_writers() {
    let home = fixture();
    let namespace = Namespace::open(&home).unwrap();
    let reader = Namespace::open(&home).unwrap();
    namespace.anchor.try_lock().unwrap();
    assert_eq!(Namespace::open(&home).err().unwrap(), BUSY);
    assert_eq!(reader.ensure_current().unwrap_err(), BUSY);
    namespace.anchor.unlock().unwrap();
    for expected in 2..=5 {
        update(&namespace, None).unwrap();
        assert_eq!(read(&home), (expected, Some("original owner".into())));
        reader.ensure_current().unwrap();
    }
    assert_eq!(
        std::fs::read_dir(home.join(DIRECTORY)).unwrap().count(),
        2,
        "Successful updates must not accumulate snapshots or temporary heads"
    );
}
#[test]
fn unpublished_orphans_are_bounded_without_deletion_or_database_effects() {
    let home = fixture();
    let namespace = Namespace::open(&home).unwrap();
    let before = std::fs::read(home.join(DIRECTORY).join(DATABASE)).unwrap();
    for index in 0..40 {
        let mut file = namespace
            .create(&format!("retained-unpublished-{index}"))
            .unwrap();
        file.write_all(b"retained interrupted data").unwrap();
    }
    assert!(update(&namespace, None).is_err());
    namespace.ensure_current().unwrap();
    assert_eq!(
        std::fs::read(home.join(DIRECTORY).join(DATABASE)).unwrap(),
        before
    );
    for index in 0..40 {
        assert_eq!(
            std::fs::read(
                home.join(DIRECTORY)
                    .join(format!("retained-unpublished-{index}"))
            )
            .unwrap(),
            b"retained interrupted data"
        );
    }
}

fn old_namespace() -> (PathBuf, Namespace, Anchor) {
    let home = tempfile::Builder::new()
        .prefix("openwebide-recovery-seal-")
        .tempdir()
        .unwrap()
        .keep();
    let folder = Folder::capture_state(&home).unwrap();
    let parent = root(&folder).unwrap();
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let anchor = parent.open_with(ANCHOR, &options).unwrap().into_std();
    Namespace::initialize(&folder, &parent, &anchor, uid).unwrap();
    let record = anchor_record(&anchor).unwrap();
    let directory = Namespace::directory(&parent, &record.key).unwrap();
    let namespace = Namespace {
        home: folder,
        directory,
        anchor,
        key: record.key.clone(),
    };
    println!("retained_seal_fixture={}", home.display());
    (home, namespace, record)
}
#[test]
fn interrupted_seal_prefixes_recover_and_cannot_authorize_initial_bytes_again() {
    for length in 0..=SEAL.len() {
        let (home, namespace, record) = old_namespace();
        let parent = root(&namespace.home).unwrap();
        let db = namespace.original_database(&parent, &record).unwrap();
        let initial = bytes(&db, MAX_DATABASE_BYTES).unwrap();
        lock(&namespace.anchor, true).unwrap();
        let error = namespace
            .migrate(&parent, &record, &db, |barrier| {
                if barrier == Barrier::Prepared {
                    Err("Injected loss before sealing".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, "Injected loss before sealing");
        // Model every possible short append, retaining the actual original
        // file. The production replay must finish it before touching SQLite.
        let mut anchor = &namespace.anchor;
        anchor.seek(SeekFrom::End(0)).unwrap();
        anchor.write_all(&SEAL[..length]).unwrap();
        anchor.sync_all().unwrap();
        namespace.anchor.unlock().unwrap();
        assert_eq!(read(&home), (1, None));
        // Even if the old bytes are written back into the same original inode,
        // loss of the head cannot re-admit an initial empty registry.
        std::fs::write(home.join(DIRECTORY).join(DATABASE), &initial).unwrap();
        std::fs::rename(
            home.join(DIRECTORY).join(HEAD),
            home.join("retained-head.json"),
        )
        .unwrap();
        assert!(Namespace::open(&home).is_err());
        assert_eq!(
            std::fs::read(home.join(DIRECTORY).join(DATABASE)).unwrap(),
            initial
        );
    }
}
#[test]
fn ordinary_readers_share_the_namespace_without_serializing_independent_roots() {
    let home = fixture();
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let home = home.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                for _ in 0..8 {
                    Namespace::open(&home).unwrap().ensure_current().unwrap();
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(read(&home), (1, None));
}
