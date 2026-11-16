use super::*;
use transaction::Boundary;

const NOTE: &str = "Reviewed original empty registry; retain both original objects";

fn upgraded(
    home: &Path,
    before: &BTreeMap<String, (Identity, Vec<u8>)>,
    expected: &str,
) -> serde_json::Value {
    let result = execute(home, expected, NOTE).unwrap();
    assert_eq!(result["status"], "completed");
    let namespace = Namespace::open_existing(home).unwrap();
    assert!(namespace.key.home.has_persistent_volume());
    assert!(!namespace.key.home.uses_device_number());
    let backup = home
        .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE)
        .join(result["generation"].as_str().unwrap());
    for (name, (identity, bytes)) in before {
        let path = if name == ANCHOR {
            backup.join(storage::ORIGINAL_ANCHOR)
        } else {
            backup
                .join(storage::ORIGINAL_DIRECTORY)
                .join(name.strip_prefix(&format!("{DIRECTORY}/")).unwrap())
        };
        assert!(identity.matches(&File::open(&path).unwrap()).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), *bytes);
    }
    assert_eq!(execute(home, expected, NOTE).unwrap(), result);
    let report = inspect(home).unwrap();
    assert!(!report.eligible);
    assert_eq!(report.phase, "completed");
    assert_eq!(report.expected.as_deref(), Some(expected));
    result
}

#[test]
fn publication_preserves_original_objects_and_repeated_requests_do_not_reinitialize() {
    for changed in [false, true] {
        for sealed in [false, true] {
            let home = fixture(changed, sealed);
            let before = files(home.path());
            let expected = inspect(home.path()).unwrap().expected.unwrap();
            let old = (!changed).then(|| Namespace::open_existing(home.path()).unwrap());
            // Opening v1 intentionally migrates it, so inspect the actual latest
            // committed source rather than use a stale review.
            let before = if sealed || changed {
                before
            } else {
                files(home.path())
            };
            let expected = if sealed || changed {
                expected
            } else {
                inspect(home.path()).unwrap().expected.unwrap()
            };
            upgraded(home.path(), &before, &expected);
            if let Some(old) = old {
                assert!(old.ensure_current().is_err());
            }
        }
    }
}

#[test]
fn process_loss_resumes_each_durable_publication_boundary() {
    if let Some(home) = std::env::var_os("OPENWEBIDE_TEST_UPGRADE_HOME") {
        let expected = std::env::var("OPENWEBIDE_TEST_UPGRADE_EXPECTED").unwrap();
        let stop = std::env::var("OPENWEBIDE_TEST_UPGRADE_STOP").unwrap();
        let checkpoint = |boundary| {
            if format!("{boundary:?}") == stop {
                std::process::exit(73);
            }
            Ok(())
        };
        if std::env::var_os("OPENWEBIDE_TEST_UPGRADE_CANCEL").is_some() {
            transaction::cancel_observed(Path::new(&home), &expected, NOTE, checkpoint).unwrap();
        } else {
            transaction::execute_observed(Path::new(&home), &expected, NOTE, checkpoint).unwrap();
        }
        panic!("Publication boundary was not reached");
    }
    for changed in [false, true] {
        for boundary in [
            Boundary::Prepared,
            Boundary::Intent,
            Boundary::AnchorBackup,
            Boundary::DirectoryBackup,
            Boundary::DirectoryPublished,
            Boundary::AnchorPublished,
            Boundary::CompletionLinked,
            Boundary::Completed,
        ] {
            let home = fixture(changed, true);
            let before = files(home.path());
            let expected = inspect(home.path()).unwrap().expected.unwrap();
            child(home.path(), &expected, boundary);
            if !matches!(boundary, Boundary::Prepared | Boundary::Completed) {
                assert!(Namespace::open_existing(home.path()).is_err());
                assert_eq!(
                    inspect(home.path()).unwrap().expected.as_deref(),
                    Some(expected.as_str())
                );
            }
            upgraded(home.path(), &before, &expected);
        }
    }
}

fn child(home: &Path, expected: &str, boundary: Boundary) {
    child_action(home, expected, boundary, false);
}
fn child_action(home: &Path, expected: &str, boundary: Boundary, cancel: bool) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact", "native_recovery::upgrade::tests::publication::process_loss_resumes_each_durable_publication_boundary", "--nocapture",
    ]).env("OPENWEBIDE_TEST_UPGRADE_HOME", home).env("OPENWEBIDE_TEST_UPGRADE_EXPECTED", expected)
        .env("OPENWEBIDE_TEST_UPGRADE_STOP", format!("{boundary:?}"));
    if cancel {
        command.env("OPENWEBIDE_TEST_UPGRADE_CANCEL", "1");
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(73),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stale_reviews_unknown_targets_and_replacement_intents_do_not_overwrite_state() {
    let home = fixture(false, true);
    let before = files(home.path());
    assert!(execute(home.path(), &"0".repeat(64), NOTE).is_err());
    assert_eq!(files(home.path()), before);
    assert!(
        !home
            .path()
            .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE)
            .exists()
    );
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    child(home.path(), &expected, Boundary::Intent);
    let root = home
        .path()
        .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE);
    let current = root.join(storage::CURRENT);
    let bytes = std::fs::read(&current).unwrap();
    std::fs::rename(&current, root.join("retained-marker")).unwrap();
    std::fs::write(&current, &bytes).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert!(Namespace::open_existing(home.path()).is_err());
    assert_eq!(files(home.path()), before);
    std::fs::remove_file(&current).unwrap();
    std::fs::rename(root.join("retained-marker"), &current).unwrap();
    let marker: plan::Pointer = serde_json::from_slice(&bytes).unwrap();
    let unknown = root.join(&marker.id).join(storage::ORIGINAL_DIRECTORY);
    std::fs::create_dir(&unknown).unwrap();
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert!(unknown.is_dir());
    assert_eq!(files(home.path()), before);
}

fn advance_old(namespace: &Namespace, owner: Option<&(String, String)>) {
    // Simulate the original publisher from before the upgrade marker contract,
    // through the real old persistence primitive after the child lost its locks.
    try_advance_old(namespace, owner).unwrap();
}
fn try_advance_old(namespace: &Namespace, owner: Option<&(String, String)>) -> Result<(), String> {
    advance_old_observed(namespace, owner, |_| Ok(()))
}
fn advance_old_observed(
    namespace: &Namespace,
    owner: Option<&(String, String)>,
    barrier: impl Fn(persistence::Barrier) -> Result<(), String>,
) -> Result<(), String> {
    lock(&namespace.anchor, true)?;
    let result = (|| {
        let parent = root(&namespace.home)?;
        let anchor = anchor_record(&namespace.anchor)?;
        let data = bytes(
            &namespace.original_database(&parent, &anchor)?,
            MAX_DATABASE_BYTES,
        )?;
        let (connection, _) = database(&data, &namespace.key).unwrap();
        if let Some((id, record)) = owner {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS recovery_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_commands(id TEXT PRIMARY KEY,record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_retired_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL); PRAGMA user_version=11;").unwrap();
            connection
                .execute(
                    "INSERT INTO recovery_owners(id,record) VALUES(?1,?2)",
                    [id, record],
                )
                .unwrap();
        }
        connection
            .execute(
                "UPDATE recovery_revision SET revision=revision+1 WHERE id=1",
                [],
            )
            .unwrap();
        let data = connection.serialize("main").unwrap();
        namespace.persist(&parent, &anchor, &data, barrier)
    })();
    io(namespace.anchor.unlock())?;
    result
}

#[test]
fn original_commits_before_activation_invalidate_the_review_without_needing_cancellation() {
    let home = fixture(false, true);
    let namespace = Namespace::open_existing(home.path()).unwrap();
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    child(home.path(), &expected, Boundary::Prepared);
    advance_old(&namespace, None);
    let advanced = files(home.path());
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert!(cancel(home.path(), &expected, NOTE).is_err());
    assert_eq!(files(home.path()), advanced);
    let fresh = inspect(home.path()).unwrap().expected.unwrap();
    assert_ne!(fresh, expected);
    upgraded(home.path(), &advanced, &fresh);
}

#[test]
fn activation_fences_old_publishers_before_the_visible_intent() {
    for boundary in [Boundary::AnchorBackup, Boundary::Intent] {
        let home = fixture(false, true);
        let namespace = Namespace::open_existing(home.path()).unwrap();
        let before = files(home.path());
        let expected = inspect(home.path()).unwrap().expected.unwrap();
        child(home.path(), &expected, boundary);
        let area = home
            .path()
            .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE);
        assert_eq!(
            area.join(storage::CURRENT).exists(),
            boundary == Boundary::Intent
        );
        assert_eq!(namespace.anchor.metadata().unwrap().nlink(), 2);
        assert!(try_advance_old(&namespace, None).is_err());
        assert_eq!(files(home.path()), before);
        assert_eq!(
            inspect(home.path()).unwrap().expected.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            cancel(home.path(), &expected, NOTE).unwrap()["status"],
            "cancelled"
        );
        assert_eq!(namespace.anchor.metadata().unwrap().nlink(), 1);
        Namespace::open_existing(home.path()).unwrap();
        assert_eq!(files(home.path()), before);
    }
}

#[test]
fn cancellation_is_resumable_and_cannot_replace_partially_published_authority() {
    for (changed, stop) in [false, true].into_iter().flat_map(|changed| {
        [
            Boundary::CancelPrepared,
            Boundary::CancellationLinked,
            Boundary::CancellationUnfenced,
            Boundary::Cancelled,
        ]
        .map(|stop| (changed, stop))
    }) {
        let home = fixture(changed, true);
        let before = files(home.path());
        let expected = inspect(home.path()).unwrap().expected.unwrap();
        child(home.path(), &expected, Boundary::Intent);
        child_action(home.path(), &expected, stop, true);
        if stop != Boundary::Cancelled {
            assert!(Namespace::open_existing(home.path()).is_err());
            assert!(execute(home.path(), &expected, NOTE).is_err());
        }
        assert_eq!(
            cancel(home.path(), &expected, NOTE).unwrap()["status"],
            "cancelled"
        );
        assert_eq!(files(home.path()), before);
    }
    let home = fixture(true, true);
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    child(home.path(), &expected, Boundary::DirectoryBackup);
    assert!(cancel(home.path(), &expected, NOTE).is_err());
    assert!(Namespace::open_existing(home.path()).is_err());
    assert_eq!(
        execute(home.path(), &expected, NOTE).unwrap()["status"],
        "completed"
    );
}

#[test]
fn a_failed_parent_flush_cannot_publish_even_when_the_preparation_directory_exists() {
    let home = fixture(false, true);
    let before = files(home.path());
    let folder = Folder::capture_state(home.path()).unwrap();
    let parent = root(&folder).unwrap();
    let uid = folder.handle.as_file().metadata().unwrap().uid();
    for create in [true, false] {
        assert!(
            storage::Area::open_flushed(&parent, uid, create, true, |_| Err(
                std::io::Error::other("Interrupted parent flush")
            ))
            .is_err()
        );
        assert_eq!(files(home.path()), before);
        assert!(
            !storage::exists(
                &storage::directory(&parent, openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE, uid)
                    .unwrap(),
                storage::CURRENT
            )
            .unwrap()
        );
    }
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    upgraded(home.path(), &before, &expected);
}

struct OwnerFixture {
    root: tempfile::TempDir,
    scope: WorkspaceTransactionScope,
    authority: RecoveryAuthority,
    row: (String, String),
}
async fn owner_fixture(legacy: bool) -> OwnerFixture {
    let home = if legacy {
        fixture(false, true)
    } else {
        tempfile::tempdir().unwrap()
    };
    let namespace = Namespace::open(home.path()).unwrap();
    let selected = tempfile::tempdir().unwrap();
    let folder = Folder::capture(selected.path()).unwrap();
    let scope = WorkspaceTransactionScope {
        id: "1".repeat(32),
        generation: "2".repeat(32),
        revision: 1,
        root_identity: "3".repeat(64),
        root: EditorRecoveryRoot {
            mode: WorkspaceMode::Remote,
            path: Some("project".into()),
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
    let parent = root(&namespace.home).unwrap();
    let anchor = anchor_record(&namespace.anchor).unwrap();
    let data = bytes(
        &namespace.original_database(&parent, &anchor).unwrap(),
        MAX_DATABASE_BYTES,
    )
    .unwrap();
    let (connection, _) = database(&data, &namespace.key).unwrap();
    let row = connection
        .query_row("SELECT id,record FROM recovery_owners", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    OwnerFixture {
        root: selected,
        scope,
        authority,
        row,
    }
}
async fn recover_owner(home: &Path, owner: &OwnerFixture) {
    let namespace = Namespace::open_existing(home).unwrap();
    let folder = Folder::capture(owner.root.path()).unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(std::sync::Arc::new(home.into()));
    assert!(coordinator.acquire_ordinary(folder.clone()).await.is_err());
    let permit = namespace
        .permit(
            &folder,
            &owner.authority,
            &owner.scope,
            WorkspaceActivity {
                file_recovery: true,
                language_command: false,
            },
        )
        .unwrap();
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace
        .finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
        .unwrap();
    drop(lease);
    drop(coordinator.acquire_ordinary(folder).await.unwrap());
}

#[tokio::test]
async fn a_late_original_claim_before_activation_remains_recoverable() {
    let home = fixture(false, true);
    let namespace = Namespace::open_existing(home.path()).unwrap();
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    let owner = owner_fixture(true).await;
    child(home.path(), &expected, Boundary::Prepared);
    advance_old(&namespace, Some(&owner.row));
    let before = files(home.path());
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert_eq!(inspect(home.path()).unwrap().records["recovery_owners"], 1);
    assert!(cancel(home.path(), &expected, NOTE).is_err());
    assert_eq!(files(home.path()), before);
    recover_owner(home.path(), &owner).await;
}

#[tokio::test]
async fn a_claim_after_interrupted_cancellation_is_preserved_and_recoverable() {
    let home = fixture(false, true);
    let namespace = Namespace::open_existing(home.path()).unwrap();
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    let owner = owner_fixture(true).await;
    child(home.path(), &expected, Boundary::AnchorBackup);
    child_action(home.path(), &expected, Boundary::CancellationUnfenced, true);
    advance_old(&namespace, Some(&owner.row));
    let before = files(home.path());
    assert_eq!(inspect(home.path()).unwrap().phase, "cancelling");
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert_eq!(
        cancel(home.path(), &expected, NOTE).unwrap()["status"],
        "cancelled"
    );
    assert_eq!(files(home.path()), before);
    assert_eq!(
        cancel(home.path(), &expected, NOTE).unwrap()["status"],
        "cancelled"
    );
    recover_owner(home.path(), &owner).await;
}

#[tokio::test]
async fn a_new_claim_after_anchor_publication_is_never_reset_by_resumption_or_repeated_requests() {
    let home = fixture(true, true);
    let before = files(home.path());
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    let owner = owner_fixture(false).await;
    child(home.path(), &expected, Boundary::AnchorPublished);
    let folder = Folder::capture_state(home.path()).unwrap();
    let parent = root(&folder).unwrap();
    let anchor = child_anchor(&parent, folder.handle.as_file().metadata().unwrap().uid());
    let record = anchor_record(&anchor).unwrap();
    let namespace = Namespace {
        home: folder,
        directory: Namespace::directory(&parent, &record.key).unwrap(),
        anchor,
        key: record.key,
    };
    advance_old(&namespace, Some(&owner.row));
    upgraded(home.path(), &before, &expected);
    recover_owner(home.path(), &owner).await;
}
fn child_anchor(parent: &Dir, uid: u32) -> File {
    super::super::super::child(parent, ANCHOR, uid).unwrap()
}

#[test]
fn busy_originals_and_tampered_manifests_are_preserved_without_publication() {
    let home = fixture(false, true);
    let namespace = Namespace::open_existing(home.path()).unwrap();
    let expected = inspect(home.path()).unwrap().expected.unwrap();
    let before = files(home.path());
    lock(&namespace.anchor, true).unwrap();
    assert!(execute(home.path(), &expected, NOTE).is_err());
    namespace.anchor.unlock().unwrap();
    assert_eq!(files(home.path()), before);
    assert!(
        !home
            .path()
            .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE)
            .exists()
    );
    child(home.path(), &expected, Boundary::Intent);
    let area = home
        .path()
        .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE);
    let pointer: plan::Pointer =
        serde_json::from_slice(&std::fs::read(area.join(storage::CURRENT)).unwrap()).unwrap();
    let path = area.join(&pointer.id).join(storage::PLAN);
    let original = std::fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["note"] = "Unexpected replacement review".into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(execute(home.path(), &expected, NOTE).is_err());
    assert!(cancel(home.path(), &expected, NOTE).is_err());
    assert!(Namespace::open_existing(home.path()).is_err());
    assert_eq!(files(home.path()), before);
    std::fs::write(&path, original).unwrap();
    upgraded(home.path(), &before, &expected);
}

#[derive(Serialize, Deserialize)]
struct RemountFixture {
    home: String,
    expected: String,
    original: BTreeMap<String, (Identity, Vec<u8>)>,
    journal: Option<BTreeMap<String, (Identity, Vec<u8>)>>,
    device: u64,
}

fn journal_files(home: &Path) -> BTreeMap<String, (Identity, Vec<u8>)> {
    std::fs::read_dir(home.join(DIRECTORY))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file());
            (
                entry.file_name().into_string().unwrap(),
                (
                    Identity::of(&File::open(entry.path()).unwrap()).unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                ),
            )
        })
        .collect()
}

#[test]
#[ignore = "requires an owned APFS image remounted between separate upgrade processes"]
fn interrupted_upgrade_survives_an_actual_home_volume_remount() {
    let base = PathBuf::from(std::env::var_os("OPENWEBIDE_REMOUNT_FIXTURE").unwrap());
    let token = std::env::var("OPENWEBIDE_REMOUNT_TOKEN").unwrap();
    assert!(base.is_absolute());
    assert!(token.len() >= 32);
    assert_eq!(
        std::fs::read_to_string(base.join(".openwebide-remount-fixture")).unwrap(),
        token
    );
    let phase = std::env::var("OPENWEBIDE_REMOUNT_PHASE").unwrap();
    let control = base.join("upgrade-fixture.json");
    if phase == "prepare" {
        let home = fixture_at(Some(&base), false, true).keep();
        let stop = std::env::var("OPENWEBIDE_REMOUNT_UPGRADE_STOP").unwrap();
        let cancelling = stop == "CancellationUnfenced";
        let old = cancelling.then(|| Namespace::open_existing(&home).unwrap());
        let expected = inspect(&home).unwrap().expected.unwrap();
        let mut fixture = RemountFixture {
            home: home.file_name().unwrap().to_str().unwrap().into(),
            expected: expected.clone(),
            original: files(&home),
            journal: None,
            device: std::fs::metadata(&home).unwrap().dev(),
        };
        let mut control_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(control)
            .unwrap();
        control_file
            .write_all(&serde_json::to_vec(&fixture).unwrap())
            .unwrap();
        control_file.sync_all().unwrap();
        if let Some(old) = old {
            child(&home, &expected, Boundary::AnchorBackup);
            child_action(&home, &expected, Boundary::CancellationUnfenced, true);
            assert!(
                advance_old_observed(&old, None, |boundary| {
                    if boundary == persistence::Barrier::PartialDatabase {
                        return Err("Interrupted old journal after cancellation unfenced".into());
                    }
                    Ok(())
                })
                .is_err()
            );
            let head: persistence::Head = serde_json::from_slice(
                &std::fs::read(home.join(DIRECTORY).join(persistence::HEAD)).unwrap(),
            )
            .unwrap();
            assert!(head.phase == persistence::Phase::Prepared);
            fixture.original = files(&home);
            fixture.journal = Some(journal_files(&home));
            control_file.seek(std::io::SeekFrom::Start(0)).unwrap();
            control_file.set_len(0).unwrap();
            control_file
                .write_all(&serde_json::to_vec(&fixture).unwrap())
                .unwrap();
            control_file.sync_all().unwrap();
            std::process::exit(73);
        }
        transaction::execute_observed(&home, &expected, NOTE, |boundary| {
            if format!("{boundary:?}") == stop {
                std::process::exit(73);
            }
            Ok(())
        })
        .unwrap();
        panic!("The selected publication boundary was not reached");
    }
    assert_eq!(phase, "recover");
    let fixture: RemountFixture = serde_json::from_slice(&std::fs::read(control).unwrap()).unwrap();
    assert_eq!(Path::new(&fixture.home).components().count(), 1);
    assert!(!matches!(fixture.home.as_str(), "." | ".."));
    assert!(Path::new(&fixture.home).is_relative());
    let home = base.join(&fixture.home).canonicalize().unwrap();
    assert!(home.starts_with(base.canonicalize().unwrap()));
    assert_ne!(std::fs::metadata(&home).unwrap().dev(), fixture.device);
    assert_eq!(
        inspect(&home).unwrap().expected.as_deref(),
        Some(fixture.expected.as_str())
    );
    if let Some(journal) = fixture.journal {
        assert_eq!(inspect(&home).unwrap().phase, "cancelling");
        assert_eq!(files(&home), fixture.original);
        assert_eq!(journal_files(&home), journal);
        assert_eq!(
            cancel(&home, &fixture.expected, NOTE).unwrap()["status"],
            "cancelled"
        );
        assert_eq!(
            cancel(&home, &fixture.expected, NOTE).unwrap()["status"],
            "cancelled"
        );
        assert_eq!(files(&home), fixture.original);
        assert_eq!(journal_files(&home), journal);
        // Completing cancellation metadata never adopts the old raw-device
        // journal as ordinary recovery authority after a remount.
        assert!(Namespace::open_existing(&home).is_err());
        assert!(
            !home
                .join(openwebide_core::vfs::NATIVE_RECOVERY_UPGRADE)
                .join(storage::CURRENT)
                .exists()
        );
        println!("interrupted_cancellation_preserved_old_prepared_journal=true");
    } else {
        upgraded(&home, &fixture.original, &fixture.expected);
    }
    println!("interrupted_native_upgrade_remount_recovered=true");
}
