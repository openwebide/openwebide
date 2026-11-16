use super::*;
use crate::folder_coordination::Coordinator;
use openwebide_core::{
    WorkspaceMode,
    editor::EditorRecoveryRoot,
    workspace_transaction::{RecoveryAuthority, WorkspaceActivity, WorkspaceTransactionScope},
};
mod publication;

fn legacy(file: &File, changed_device: bool) -> Identity {
    let identity = Identity::legacy(file).unwrap();
    if !changed_device {
        return identity;
    }
    let mut value = serde_json::to_value(identity).unwrap();
    value["device"] = (value["device"].as_u64().unwrap() + 1000).into();
    serde_json::from_value(value).unwrap()
}

fn fixture(changed_device: bool, sealed: bool) -> tempfile::TempDir {
    fixture_at(None, changed_device, sealed)
}
fn fixture_at(parent: Option<&Path>, changed_device: bool, sealed: bool) -> tempfile::TempDir {
    let home = if let Some(parent) = parent {
        tempfile::tempdir_in(parent).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let mut options = std::fs::OpenOptions::new();
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    options.read(true).write(true).create_new(true).mode(0o600);
    let mut anchor = options.open(home.path().join(ANCHOR)).unwrap();
    let directory = home.path().join(DIRECTORY);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let mut database_file = options.open(directory.join(DATABASE)).unwrap();
    let key = Key {
        uid: std::fs::metadata(home.path()).unwrap().uid(),
        nonce: "a".repeat(32),
        home: legacy(&File::open(home.path()).unwrap(), changed_device),
        directory: legacy(&File::open(&directory).unwrap(), changed_device),
        anchor: legacy(&anchor, changed_device),
    };
    let initial = sqlite(&key).unwrap();
    let data = if sealed {
        let (connection, _) = database(&initial, &key).unwrap();
        connection.execute_batch("CREATE TABLE recovery_revision(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL); INSERT INTO recovery_revision VALUES(1,1);
            CREATE TABLE recovery_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);
            CREATE TABLE recovery_commands(id TEXT PRIMARY KEY,record TEXT NOT NULL);
            CREATE TABLE recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL);
            CREATE TABLE recovery_retired_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);
            PRAGMA user_version=11;").unwrap();
        connection.serialize("main").unwrap().to_vec()
    } else {
        initial.clone()
    };
    database_file.write_all(&data).unwrap();
    database_file.sync_all().unwrap();
    let record = Anchor {
        format: 1,
        key: key.clone(),
        database: legacy(&database_file, changed_device),
        digest: digest(&initial),
        sealed,
    };
    anchor
        .write_all(&serde_json::to_vec(&record).unwrap())
        .unwrap();
    if sealed {
        anchor.write_all(SEAL).unwrap();
        let name = format!("snapshot-{}.sqlite3", "b".repeat(32));
        let mut snapshot = options.open(directory.join(&name)).unwrap();
        snapshot.write_all(&data).unwrap();
        snapshot.sync_all().unwrap();
        let mut head = options.open(directory.join(persistence::HEAD)).unwrap();
        let record = persistence::Head {
            format: 1,
            key,
            identity: legacy(&head, changed_device),
            revision: 1,
            digest: digest(&data),
            snapshot: persistence::Snapshot {
                name: name.clone(),
                identity: legacy(&snapshot, changed_device),
            },
            phase: persistence::Phase::Committed,
        };
        head.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        head.sync_all().unwrap();
        drop(snapshot);
        std::fs::remove_file(directory.join(name)).unwrap();
    }
    anchor.sync_all().unwrap();
    home
}

fn files(home: &Path) -> BTreeMap<String, (Identity, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for name in [
        ANCHOR.to_owned(),
        format!("{DIRECTORY}/{DATABASE}"),
        format!("{DIRECTORY}/{}", persistence::HEAD),
    ] {
        let path = home.join(&name);
        if path.exists() {
            result.insert(
                name,
                (
                    Identity::of(&File::open(&path).unwrap()).unwrap(),
                    std::fs::read(path).unwrap(),
                ),
            );
        }
    }
    result
}

#[test]
fn inspection_is_read_only_and_stable_for_unchanged_and_remounted_legacy_state() {
    for changed in [false, true] {
        for sealed in [false, true] {
            let home = fixture(changed, sealed);
            let before = files(home.path());
            let report = inspect(home.path()).unwrap();
            assert!(report.eligible);
            assert_eq!(report.device_changed, changed);
            assert!(report.expected.is_some());
            assert_eq!(inspect(home.path()).unwrap().expected, report.expected);
            assert!(report.records.values().all(|count| *count == 0));
            assert_eq!(files(home.path()), before);
            if changed {
                assert!(Namespace::open_existing(home.path()).is_err());
            } else {
                Namespace::open_existing(home.path()).unwrap();
            }
        }
    }
}

#[test]
fn inspection_rejects_unknown_files_replacement_and_prepared_or_corrupt_heads() {
    let home = fixture(true, true);
    let directory = home.path().join(DIRECTORY);
    let baseline = files(home.path());
    let unknown = directory.join("snapshot-unknown");
    std::fs::write(&unknown, b"preserve unknown state").unwrap();
    assert!(inspect(home.path()).is_err());
    assert_eq!(std::fs::read(&unknown).unwrap(), b"preserve unknown state");
    std::fs::remove_file(unknown).unwrap();
    let path = directory.join(persistence::HEAD);
    let original = std::fs::read(&path).unwrap();
    let mut head: serde_json::Value = serde_json::from_slice(&original).unwrap();
    head["phase"] = "prepared".into();
    std::fs::write(&path, serde_json::to_vec(&head).unwrap()).unwrap();
    assert!(inspect(home.path()).is_err());
    std::fs::write(&path, &original).unwrap();
    assert!(inspect(home.path()).unwrap().eligible);
    std::fs::rename(&path, directory.join("retained-original")).unwrap();
    std::fs::write(&path, original).unwrap();
    std::fs::remove_file(directory.join("retained-original")).unwrap();
    assert!(inspect(home.path()).is_err());
    assert_eq!(
        std::fs::read(home.path().join(ANCHOR)).unwrap(),
        baseline[ANCHOR].1
    );
}

#[test]
fn every_record_table_and_unknown_sql_state_prevent_an_empty_upgrade() {
    for selected in RECORDS {
        let connection = Connection::open_in_memory().unwrap();
        for table in RECORDS {
            connection
                .execute_batch(&format!("CREATE TABLE {table}(id TEXT,record TEXT)"))
                .unwrap();
        }
        connection
            .execute(&format!("INSERT INTO {selected} VALUES('id','record')"), [])
            .unwrap();
        let counts = inspection::records(&connection).unwrap();
        assert_eq!(counts[selected], 1);
        assert!(!counts.values().all(|count| *count == 0));
        connection.execute_batch("CREATE TABLE recovery_revision(id INTEGER,revision INTEGER); INSERT INTO recovery_revision VALUES(1,1),(2,2)").unwrap();
        assert!(inspection::records(&connection).is_err());
        connection
            .execute_batch("DROP TABLE recovery_revision")
            .unwrap();
        connection
            .execute_batch("CREATE TABLE unknown_history(value TEXT)")
            .unwrap();
        assert!(inspection::records(&connection).is_err());
    }
}

#[tokio::test]
async fn original_pending_owners_remain_recoverable_and_are_ineligible_for_upgrade() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = fixture(false, true);
        let root = tempfile::tempdir().unwrap();
        let folder = Folder::capture(root.path()).unwrap();
        let namespace = Namespace::open_existing(home.path()).unwrap();
        let scope = WorkspaceTransactionScope {
            id: "1".repeat(32),
            generation: "2".repeat(32),
            revision: 1,
            root_identity: "3".repeat(64),
            root: EditorRecoveryRoot {
                mode,
                path: (mode == WorkspaceMode::Remote).then(|| "project".into()),
            },
            worker: None,
            accepted_language_review_digest: None,
        };
        let authority = RecoveryAuthority::for_scope(1, 1, &scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(
                &folder,
                &authority,
                &scope,
                WorkspaceActivity {
                    file_recovery: true,
                    language_command: false,
                },
            )
            .unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(std::sync::Arc::new(home.path().into()));
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace.enroll(&folder, &lease, &permit).unwrap();
        drop(lease);
        let before = files(home.path());
        let report = inspect(home.path()).unwrap();
        assert!(!report.eligible);
        assert!(report.expected.is_none());
        assert_eq!(report.records["recovery_owners"], 1);
        assert_eq!(files(home.path()), before);
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace
            .finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
            .unwrap();
        drop(lease);
        assert!(inspect(home.path()).unwrap().eligible);
    }
}
